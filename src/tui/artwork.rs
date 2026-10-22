use super::{
    state::{App, Focus, Source},
    worker::View,
};
use crate::{
    artwork::{Task, fetch},
    config::{ArtworkMode, Config, Theme},
    model::Station,
};
use anyhow::Context;
use ratatui::{
    Frame,
    buffer::CellDiffOption,
    layout::{Rect, Size},
};
use ratatui_image::{Resize, picker::Picker, protocol::Protocol};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(test)]
const SIZE: Size = Size::new(12, 6);

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Target {
    identity: String,
    url: String,
    track_generation: Option<u64>,
    fallback: Option<String>,
    size: Size,
}

#[derive(Default)]
pub(super) struct ImageState {
    pub target: Option<Target>,
    pub protocol: Option<Arc<Protocol>>,
}

pub(super) fn station<'a>(app: &App, view: &'a View) -> Option<&'a Station> {
    if app.inspect_info {
        (view.inspected_request == app.info_request && view.inspected_error.is_none())
            .then_some(view.inspected.as_ref())
            .flatten()
    } else if app.source == Source::Recordings {
        None
    } else {
        app.selected(view)
    }
}

pub(super) fn area(app: &App, view: &View, screen: Rect) -> Option<Rect> {
    let rect = app.rects[Focus::Details.index()];
    if app.theme == Theme::Mono
        || screen.width < 100
        || screen.height < 24
        || rect.width < 26
        || app.help
        || app.filter_edit.is_some()
        || app.confirmation.is_some()
    {
        return None;
    }
    let station = station(app, view)?;
    let lines =
        super::render::station_lines(app, station, view, rect.width.saturating_sub(2)).len();
    let top = u16::try_from(lines).ok()?.saturating_add(2);
    let height = rect.height.saturating_sub(top + 1);
    // Keep text in the same cells whether artwork is loading, missing or changing.
    // Short panels retain all their space for scrolling station details.
    (height >= 4).then(|| {
        Rect::new(
            rect.x + 1,
            rect.y + top,
            rect.width.saturating_sub(2).min(120),
            height.min(60),
        )
    })
}

/// Draws the image centered in `rect` and returns the cells it covers.
pub(super) fn render(frame: &mut Frame, image: &Protocol, rect: Rect) -> Rect {
    let size = image.area().as_size();
    let width = size.width.min(rect.width);
    let height = size.height.min(rect.height);
    let rect = Rect::new(
        rect.x + (rect.width - width) / 2,
        rect.y + (rect.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(ratatui_image::Image::new(image), rect);
    // ratatui-image 10 embeds entire graphics escape sequences in one cell.
    // Their apparent Unicode width otherwise makes Ratatui's diff skip later
    // rows, including the remaining Kitty placeholders. Keep this compatibility
    // fix while using the image crate version that supports Rust 1.88.
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let cell = &mut frame.buffer_mut()[(x, y)];
            if cell.symbol().contains('\x1b') {
                cell.set_diff_option(CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN));
            }
        }
    }
    rect
}

fn target(app: &App, view: &View, screen: Rect) -> Option<Target> {
    let size = area(app, view, screen)?.as_size();
    let station = station(app, view)?;
    if let Some(cover) = &view.track_artwork
        && cover.request.stationuuid == station.stationuuid
        && view
            .status
            .station
            .as_ref()
            .is_some_and(|current| current.stationuuid == station.stationuuid)
        && cover.request.source_url == view.status.source_url
        && cover.request.title == view.status.title
        && view.status.playback_state == crate::engine::PlaybackState::Playing
        && !matches!(
            view.status.recording_state,
            crate::engine::RecordingState::Probing
                | crate::engine::RecordingState::Recording
                | crate::engine::RecordingState::Finalizing
        )
    {
        return Some(Target {
            size,
            identity: station.stationuuid.clone(),
            url: cover.url.clone(),
            track_generation: Some(cover.generation),
            fallback: (!station.custom && !station.favicon.is_empty())
                .then(|| station.favicon.clone()),
        });
    }
    (!station.custom && !station.favicon.is_empty()).then(|| Target {
        size,
        identity: station.stationuuid.clone(),
        url: station.favicon.clone(),
        track_generation: None,
        fallback: None,
    })
}

pub(super) fn image<'a>(
    app: &'a App,
    view: &View,
    screen: Rect,
) -> Option<(&'a Arc<Protocol>, Rect)> {
    let rect = area(app, view, screen)?;
    (app.artwork.target == target(app, view, screen))
        .then_some(app.artwork.protocol.as_ref().map(|image| (image, rect)))
        .flatten()
}

pub(super) struct Controller {
    config: Config,
    picker: Picker,
    desired: Option<Target>,
    task: Option<(Target, Task<Protocol>)>,
    retry: Instant,
}

impl Controller {
    pub fn demand(&self, app: &App, view: &View, screen: Rect) -> Option<String> {
        if self.config.settings.artwork != ArtworkMode::Track || area(app, view, screen).is_none() {
            return None;
        }
        let station = station(app, view)?;
        (station.hls == 0
            && view
                .status
                .station
                .as_ref()
                .is_some_and(|current| current.stationuuid == station.stationuuid))
        .then(|| station.stationuuid.clone())
    }

    /// Called once with raw mode enabled, before crossterm reads any events.
    pub fn new(
        config: Config,
        theme: Theme,
        pending: &mut std::collections::VecDeque<crossterm::event::Event>,
    ) -> Option<Self> {
        if config.settings.artwork == ArtworkMode::Off || theme == Theme::Mono {
            return None;
        }
        let picker = super::capabilities::picker(pending);
        Some(Self {
            config,
            picker,
            desired: None,
            task: None,
            retry: Instant::now(),
        })
    }

    pub fn update(&mut self, app: &mut App, view: &View, screen: Rect) -> bool {
        let desired = target(app, view, screen);
        let mut dirty = false;
        if desired != self.desired {
            self.desired = desired.clone();
            app.artwork.target = desired;
            app.artwork.protocol = None;
            self.retry = Instant::now();
            if let Some((_, task)) = &mut self.task {
                task.cancel();
            }
            dirty = true;
        }
        if let Some((key, task)) = &self.task
            && task.finished()
            && let Some(result) = task.poll()
        {
            if Some(key) == self.desired.as_ref() {
                app.artwork.protocol = result.ok().map(Arc::new);
                self.retry = Instant::now() + Duration::from_secs(30);
                dirty = true;
            }
            self.task = None;
        }
        if self.task.is_none()
            && app.artwork.protocol.is_none()
            && Instant::now() >= self.retry
            && let Some(key) = self.desired.clone()
        {
            let config = self.config.clone();
            let picker = self.picker.clone();
            let request = key.clone();
            match Task::start(move || async move {
                let image = match fetch(&config, &request.identity, &request.url).await {
                    Ok(image) => image,
                    Err(error) => {
                        let Some(fallback) = request.fallback else {
                            return Err(error);
                        };
                        fetch(&config, &request.identity, &fallback)
                            .await
                            .with_context(|| {
                                format!(
                                    "track artwork failed: {error:#}; station logo fallback failed"
                                )
                            })?
                    }
                };
                tokio::task::spawn_blocking(move || {
                    Ok(picker.new_protocol(image, request.size.into(), Resize::Scale(None))?)
                })
                .await?
            }) {
                Ok(task) => self.task = Some((key, task)),
                Err(_) => self.retry = Instant::now() + Duration::from_secs(30),
            }
        }
        dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Engine, test_support};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::atomic::AtomicBool;

    fn fixture() -> (tempfile::TempDir, Config, App, View) {
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let engine = Engine::new(config.clone(), Arc::new(AtomicBool::new(false))).unwrap();
        let mut view = View::new(engine.status.clone());
        view.rows = Arc::new(vec![Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Logo station".into(),
            favicon: "http://localhost/logo.png".into(),
            ..Station::default()
        }]);
        view.total = 1;
        view.page = Some(super::super::worker::PageState {
            generation: 0,
            requested: view.filter.clone(),
            phase: super::super::worker::PagePhase::Ready,
        });
        (dir, config, App::default(), view)
    }

    fn draw(app: &mut App, view: &View, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, app, view))
            .unwrap();
        terminal
    }

    #[test]
    fn logo_rendering_respects_selection_inspection_theme_size_and_popups() {
        let (_dir, _, mut app, mut view) = fixture();
        let screen = Rect::new(0, 0, 160, 60);
        draw(&mut app, &view, 160, 60);
        app.artwork.target = target(&app, &view, screen);
        let protocol = Picker::halfblocks()
            .new_protocol(
                ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_pixel(
                    40,
                    40,
                    ::image::Rgb([210, 20, 50]),
                )),
                SIZE.into(),
                Resize::Fit(None),
            )
            .unwrap();
        app.artwork.protocol = Some(Arc::new(protocol));
        let terminal = draw(&mut app, &view, 160, 60);
        let rect = area(&app, &view, screen).unwrap();
        assert!((rect.y..rect.bottom()).any(|y| {
            (rect.x..rect.right()).any(|x| {
                terminal.backend().buffer()[(x, y)].fg == ratatui::style::Color::Rgb(210, 20, 50)
            })
        }));
        app.theme = Theme::Light;
        assert!(image(&app, &view, screen).is_some());
        app.theme = Theme::Mono;
        assert!(image(&app, &view, screen).is_none());
        app.theme = Theme::Default;
        app.help = true;
        assert!(image(&app, &view, screen).is_none());
        app.help = false;
        draw(&mut app, &view, 80, 24);
        assert!(image(&app, &view, Rect::new(0, 0, 80, 24)).is_none());
        draw(&mut app, &view, 160, 48);
        assert!(target(&app, &view, Rect::new(0, 0, 160, 48)).is_some());
        draw(&mut app, &view, 160, 60);
        app.inspect_info = true;
        app.info_request = 8;
        view.inspected_request = 7;
        assert!(target(&app, &view, screen).is_none());
        view.inspected_request = 8;
        view.inspected = Some(view.rows[0].clone());
        assert!(image(&app, &view, screen).is_some());
        view.inspected.as_mut().unwrap().favicon.clear();
        assert!(image(&app, &view, screen).is_none());
        app.inspect_info = false;
        Arc::make_mut(&mut view.rows)[0].custom = true;
        assert!(image(&app, &view, screen).is_none());
    }

    #[test]
    fn light_theme_keeps_artwork_colors_and_recolors_the_interface() {
        use ratatui::style::Color;
        let (_dir, _, mut app, view) = fixture();
        let screen = Rect::new(0, 0, 160, 60);
        draw(&mut app, &view, 160, 60);
        app.artwork.target = target(&app, &view, screen);
        let color = Color::Rgb(210, 20, 50);
        app.artwork.protocol = Some(Arc::new(
            Picker::halfblocks()
                .new_protocol(
                    ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_pixel(
                        40,
                        40,
                        ::image::Rgb([210, 20, 50]),
                    )),
                    SIZE.into(),
                    Resize::Fit(None),
                )
                .unwrap(),
        ));
        app.theme = Theme::Light;
        let terminal = draw(&mut app, &view, 160, 60);
        let buffer = terminal.backend().buffer();
        let rect = area(&app, &view, screen).unwrap();
        let image: Vec<_> = (rect.y..rect.bottom())
            .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| buffer[(x, y)].fg == color)
            .collect();
        assert!(!image.is_empty());
        assert!(image.iter().all(|&p| buffer[p].bg == color));
        let player = app.rects[Focus::Player.index()];
        assert_eq!(buffer[(player.x, player.y)].bg, Color::White);
    }

    #[test]
    fn images_use_remaining_details_space_without_moving_text() {
        let (_dir, _, mut app, view) = fixture();
        let screen = Rect::new(0, 0, 160, 60);
        let before = draw(&mut app, &view, 160, 60).backend().buffer().clone();
        let rect = area(&app, &view, screen).unwrap();
        assert!(rect.width > 12 && rect.height > 6);
        app.artwork.target = target(&app, &view, screen);
        app.artwork.protocol = Some(Arc::new(
            Picker::halfblocks()
                .new_protocol(
                    ::image::DynamicImage::new_rgb8(256, 256),
                    rect,
                    Resize::Scale(None),
                )
                .unwrap(),
        ));
        let during = draw(&mut app, &view, 160, 60).backend().buffer().clone();
        let details = app.rects[Focus::Details.index()];
        for y in details.y..rect.y {
            for x in details.x..details.right() {
                assert_eq!(before[(x, y)], during[(x, y)]);
            }
        }
        app.artwork.protocol = None;
        let after = draw(&mut app, &view, 160, 60).backend().buffer().clone();
        assert_eq!(before, after);
        draw(&mut app, &view, 100, 24);
        assert!(area(&app, &view, Rect::new(0, 0, 100, 24)).is_none());
    }

    #[test]
    fn artwork_is_centered_at_its_encoded_size() {
        let bounds = Rect::new(3, 2, 30, 12);
        let picker = Picker::halfblocks();
        for (width, height) in [(32, 32), (64, 16), (16, 64)] {
            let protocol = picker
                .new_protocol(
                    ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_pixel(
                        width,
                        height,
                        ::image::Rgb([210, 20, 50]),
                    )),
                    Rect::new(0, 0, 12, 6),
                    Resize::Scale(None),
                )
                .unwrap();
            let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
            terminal
                .draw(|frame| {
                    super::render(frame, &protocol, bounds);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let colored: Vec<_> = (0..18)
                .flat_map(|y| (0..40).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    buffer[(x, y)].fg != ratatui::style::Color::Reset
                        || buffer[(x, y)].bg != ratatui::style::Color::Reset
                })
                .collect();
            assert!(!colored.is_empty());
            let left = colored.iter().map(|p| p.0).min().unwrap();
            let right = colored.iter().map(|p| p.0).max().unwrap() + 1;
            let top = colored.iter().map(|p| p.1).min().unwrap();
            let bottom = colored.iter().map(|p| p.1).max().unwrap() + 1;
            assert_eq!(right - left, protocol.area().width);
            assert_eq!(bottom - top, protocol.area().height);
            assert!((i32::from(left - bounds.x) - i32::from(bounds.right() - right)).abs() <= 1);
            assert!((i32::from(top - bounds.y) - i32::from(bounds.bottom() - bottom)).abs() <= 1);
        }
    }

    #[test]
    fn protocol_sequences_pass_through_terminal_writer() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        use ratatui_image::picker::ProtocolType;
        for kind in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(kind);
            let protocol = picker
                .new_protocol(
                    ::image::DynamicImage::new_rgb8(96, 96),
                    SIZE.into(),
                    Resize::Fit(None),
                )
                .unwrap();
            let expected_rows = protocol.area().height;
            let expected_columns = protocol.area().width;
            let mut bytes = Vec::new();
            {
                let writer = super::super::output::TerminalWriter::new(&mut bytes, false);
                let mut terminal = Terminal::with_options(
                    CrosstermBackend::new(writer),
                    TerminalOptions {
                        viewport: Viewport::Fixed(Rect::new(0, 0, 20, 10)),
                    },
                )
                .unwrap();
                terminal
                    .draw(|frame| {
                        super::render(frame, &protocol, Rect::new(0, 0, 12, 6));
                    })
                    .unwrap();
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            ratatui::widgets::Paragraph::new("image hidden"),
                            frame.area(),
                        )
                    })
                    .unwrap();
                terminal
                    .draw(|frame| {
                        super::render(frame, &protocol, Rect::new(0, 0, 12, 6));
                    })
                    .unwrap();
            }
            let text = String::from_utf8_lossy(&bytes);
            let marker = match kind {
                ProtocolType::Kitty => "\x1b_G",
                ProtocolType::Sixel => "\x1bP",
                ProtocolType::Iterm2 => "\x1b]1337;",
                ProtocolType::Halfblocks => unreachable!(),
            };
            assert!(text.contains(marker), "missing {kind:?} output");
            if kind == ProtocolType::Kitty {
                assert_eq!(
                    text.matches('\u{10eeee}').count(),
                    usize::from(expected_rows * expected_columns) * 2,
                    "all Kitty rows must survive buffer diffing and reappearance"
                );
            }
        }
    }

    #[test]
    fn disabled_and_monochrome_modes_do_not_query_terminal_or_spawn_workers() {
        let (_dir, mut config, _, _) = fixture();
        assert!(Controller::new(config.clone(), Theme::Default, &mut Default::default()).is_none());
        config.settings.artwork = ArtworkMode::Track;
        assert!(Controller::new(config, Theme::Mono, &mut Default::default()).is_none());
    }

    #[test]
    fn selection_changes_cancel_old_fetches_and_publish_only_the_latest_logo() {
        use std::{
            io::{Read, Write},
            sync::mpsc,
        };
        let (_dir, config, mut app, mut view) = fixture();
        let (opened, ready) = mpsc::channel();
        let server = test_support::Server::new(move |request, stream| {
            if request.starts_with("GET /old ") {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 100\r\n\r\n",
                );
                opened.send(()).unwrap();
                let _ = stream.read(&mut [0]);
            } else {
                let mut bytes = std::io::Cursor::new(Vec::new());
                ::image::DynamicImage::new_rgb8(24, 24)
                    .write_to(&mut bytes, ::image::ImageFormat::Png)
                    .unwrap();
                test_support::response(stream, 200, "image/png", bytes.get_ref());
            }
        });
        Arc::make_mut(&mut view.rows)[0].favicon = format!("{}/old", server.url);
        let mut controller = Controller {
            config,
            picker: Picker::halfblocks(),
            desired: None,
            task: None,
            retry: Instant::now(),
        };
        let screen = Rect::new(0, 0, 160, 60);
        draw(&mut app, &view, 160, 60);
        controller.update(&mut app, &view, screen);
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        Arc::make_mut(&mut view.rows)[0].favicon = format!("{}/new", server.url);
        controller.update(&mut app, &view, screen);
        assert!(app.artwork.protocol.is_none());
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.artwork.protocol.is_none() && Instant::now() < deadline {
            controller.update(&mut app, &view, screen);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.artwork.protocol.is_some());
        assert!(image(&app, &view, screen).is_some());
    }

    #[test]
    fn track_covers_require_current_playback_and_fall_back_to_the_logo() {
        use crate::engine::{
            PlaybackState, RecordingState,
            artwork::{Request, TrackArtwork},
        };
        let (_dir, mut config, mut app, mut view) = fixture();
        let server = test_support::Server::new(|request, stream| {
            if request.starts_with("GET /cover ") {
                test_support::response(stream, 200, "text/html", b"not a cover");
            } else {
                let mut bytes = std::io::Cursor::new(Vec::new());
                ::image::DynamicImage::new_rgb8(24, 24)
                    .write_to(&mut bytes, ::image::ImageFormat::Png)
                    .unwrap();
                test_support::response(stream, 200, "image/png", bytes.get_ref());
            }
        });
        let screen = Rect::new(0, 0, 160, 60);
        Arc::make_mut(&mut view.rows)[0].favicon = format!("{}/logo", server.url);
        view.status.station = Some(view.rows[0].clone());
        view.status.playback_state = PlaybackState::Playing;
        view.status.source_url = "http://localhost/live".into();
        view.status.title = "Track".into();
        view.track_artwork = Some(TrackArtwork {
            generation: 1,
            request: Request {
                stationuuid: view.rows[0].stationuuid.clone(),
                source_url: view.status.source_url.clone(),
                title: "Track".into(),
            },
            url: format!("{}/cover", server.url),
        });
        config.settings.artwork = ArtworkMode::Track;
        let mut controller = Controller {
            config,
            picker: Picker::halfblocks(),
            desired: None,
            task: None,
            retry: Instant::now(),
        };
        draw(&mut app, &view, 160, 60);
        assert_eq!(
            target(&app, &view, screen).unwrap().track_generation,
            Some(1)
        );
        assert!(controller.demand(&app, &view, screen).is_some());
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.artwork.protocol.is_none() && Instant::now() < deadline {
            controller.update(&mut app, &view, screen);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            image(&app, &view, screen).is_some(),
            "invalid cover content must fall back to the valid logo"
        );
        view.status.title = "Next track".into();
        assert!(
            image(&app, &view, screen).is_none(),
            "stale art must disappear before another worker result"
        );
        assert_eq!(target(&app, &view, screen).unwrap().track_generation, None);
        view.status.title = "Track".into();
        for state in [
            RecordingState::Probing,
            RecordingState::Recording,
            RecordingState::Finalizing,
        ] {
            view.status.recording_state = state;
            assert_eq!(target(&app, &view, screen).unwrap().track_generation, None);
        }
        view.status.recording_state = RecordingState::Idle;
        view.status.station.as_mut().unwrap().stationuuid = "another station".into();
        assert!(controller.demand(&app, &view, screen).is_none());
        assert_eq!(target(&app, &view, screen).unwrap().track_generation, None);
    }
}
