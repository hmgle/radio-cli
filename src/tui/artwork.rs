use super::{
    state::{App, Focus, Source},
    worker::View,
};
use crate::{
    artwork::{Task, fetch},
    config::{ArtworkMode, Config, Theme},
    model::Station,
};
use ratatui::layout::{Rect, Size};
use ratatui_image::{
    Resize,
    picker::{Picker, cap_parser::QueryStdioOptions},
    protocol::Protocol,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SIZE: Size = Size::new(12, 6);

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Target {
    identity: String,
    url: String,
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

pub(super) fn area(app: &App, screen: Rect) -> Option<Rect> {
    let rect = app.rects[Focus::Details.index()];
    (app.theme != Theme::Mono
        && screen.width >= 100
        && screen.height >= 24
        && rect.width >= 26
        && rect.height >= 13
        && !app.help
        && app.filter_edit.is_none()
        && app.confirmation.is_none())
    .then(|| Rect::new(rect.x + 1, rect.y + 1, SIZE.width, SIZE.height))
}

fn target(app: &App, view: &View, screen: Rect) -> Option<Target> {
    area(app, screen)?;
    let station = station(app, view)?;
    (!station.custom && !station.favicon.is_empty()).then(|| Target {
        identity: station.stationuuid.clone(),
        url: station.favicon.clone(),
    })
}

pub(super) fn image<'a>(
    app: &'a App,
    view: &View,
    screen: Rect,
) -> Option<(&'a Arc<Protocol>, Rect)> {
    let rect = area(app, screen)?;
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
    /// Called once with raw mode enabled, before crossterm reads any events.
    pub fn new(config: Config, theme: Theme) -> Option<Self> {
        if config.settings.artwork == ArtworkMode::Off || theme == Theme::Mono {
            return None;
        }
        let picker = if std::env::var_os("TMUX").is_some() {
            Picker::halfblocks()
        } else {
            Picker::from_query_stdio_with_options(QueryStdioOptions {
                timeout: Duration::from_millis(200),
                ..QueryStdioOptions::default()
            })
            .unwrap_or_else(|_| Picker::halfblocks())
        };
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
                let image = fetch(&config, &request.identity, &request.url).await?;
                Ok(picker.new_protocol(image, SIZE.into(), Resize::Fit(None))?)
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
        let screen = Rect::new(0, 0, 100, 24);
        draw(&mut app, &view, 100, 24);
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
        let terminal = draw(&mut app, &view, 100, 24);
        let rect = area(&app, screen).unwrap();
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
        assert!(image(&app, &view, Rect::new(0, 0, 160, 48)).is_some());
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
                        frame.render_widget(
                            ratatui_image::Image::new(&protocol),
                            Rect::new(0, 0, 12, 6),
                        )
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
        }
    }

    #[test]
    fn disabled_and_monochrome_modes_do_not_query_terminal_or_spawn_workers() {
        let (_dir, mut config, _, _) = fixture();
        assert!(Controller::new(config.clone(), Theme::Default).is_none());
        config.settings.artwork = ArtworkMode::Track;
        assert!(Controller::new(config, Theme::Mono).is_none());
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
        let screen = Rect::new(0, 0, 100, 24);
        draw(&mut app, &view, 100, 24);
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
}
