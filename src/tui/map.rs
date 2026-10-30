//! Offline map rendering and bounded Explore state.
use crate::{
    geo::{GeoBounds, WORLD_HEIGHT, WORLD_WIDTH},
    model::{Facet, Filter},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
};
use std::{sync::Arc, time::Instant};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub filter: Filter,
    pub bounds: GeoBounds,
    pub columns: usize,
    pub rows: usize,
    pub countries: bool,
}
#[derive(Clone)]
pub(super) struct Image {
    pub request: Request,
    pub revision: u64,
    pub land: Arc<Vec<char>>,
    pub counts: Vec<u32>,
    pub countries: Vec<Facet>,
}
pub(super) struct State {
    pub bounds: GeoBounds,
    pub zoom: u8,
    pub cursor: (i64, i64),
    pub area: Rect,
    pub size: (usize, usize),
    pub deadline: Option<Instant>,
    pub submitted: Option<(Request, u64)>,
    pub initialized: bool,
    pub chooser: bool,
    pub country_index: usize,
    pub country_origin: Option<Option<String>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            bounds: GeoBounds::default(),
            zoom: 0,
            cursor: (180_000_000, 90_000_000),
            area: Rect::default(),
            size: (64, 16),
            deadline: None,
            submitted: None,
            initialized: false,
            chooser: false,
            country_index: 0,
            country_origin: None,
        }
    }
}
impl State {
    pub fn cell(&self) -> GeoBounds {
        let (col, row) = self
            .bounds
            .project(self.cursor.0, self.cursor.1, self.size.0, self.size.1)
            .unwrap_or((0, 0));
        self.bounds.cell(col, row, self.size.0, self.size.1)
    }
    pub fn resize(&mut self, area: Rect) {
        self.area = area;
        if area.width > 0 && area.height > 0 {
            self.size = (area.width as usize, area.height as usize);
        }
    }
    pub fn zoom(&mut self, inward: bool, anchor: (i64, i64)) {
        let next = if inward {
            self.zoom.saturating_add(1).min(3)
        } else {
            self.zoom.saturating_sub(1)
        };
        let dx = (anchor.0 - self.bounds.x).rem_euclid(WORLD_WIDTH);
        let dy = anchor.1 - self.bounds.y;
        let width = WORLD_WIDTH >> next;
        let height = WORLD_HEIGHT >> next;
        self.bounds = GeoBounds {
            x: (anchor.0 - dx * width / self.bounds.width).rem_euclid(WORLD_WIDTH),
            y: (anchor.1 - dy * height / self.bounds.height).clamp(0, WORLD_HEIGHT - height),
            width,
            height,
        };
        if next == 0 {
            self.bounds = GeoBounds::default();
        }
        self.zoom = next;
        self.cursor = anchor;
    }
    pub fn step(&mut self, dx: i64, dy: i64) {
        let (col, row) = self
            .bounds
            .project(self.cursor.0, self.cursor.1, self.size.0, self.size.1)
            .unwrap_or((0, 0));
        let columns = self.size.0 as i64;
        let rows = self.size.1 as i64;
        let mut col = col as i64 + dx;
        let row = row as i64 + dy;
        if self.bounds.width == WORLD_WIDTH {
            col = col.rem_euclid(columns);
        } else if !(0..columns).contains(&col) {
            let step = (self.bounds.width + columns - 1) / columns;
            self.bounds.x = (self.bounds.x + dx * step).rem_euclid(WORLD_WIDTH);
        }
        if !(0..rows).contains(&row) {
            let step = (self.bounds.height + rows - 1) / rows;
            self.bounds.y = (self.bounds.y + dy * step).clamp(0, WORLD_HEIGHT - self.bounds.height);
        }
        // Move by cells, whose widths can differ by one microdegree, instead
        // of accumulating a truncated distance that can leave the cursor behind.
        let cell = self.bounds.cell(
            col.clamp(0, columns - 1) as usize,
            row.clamp(0, rows - 1) as usize,
            self.size.0,
            self.size.1,
        );
        self.cursor = (
            (cell.x + cell.width / 2) % WORLD_WIDTH,
            cell.y + cell.height / 2,
        );
    }
    pub fn point(&self, col: u16, row: u16) -> (i64, i64) {
        let cell = self.bounds.cell(
            (col - self.area.x) as usize,
            (row - self.area.y) as usize,
            self.size.0,
            self.size.1,
        );
        (
            (cell.x + cell.width / 2) % WORLD_WIDTH,
            cell.y + cell.height / 2,
        )
    }
}

/// Scanline even-odd fill of all polygon rings. Geometry stays in read-only
/// binary storage; scratch space is bounded by vertex count and viewport size.
pub(super) fn raster(bounds: GeoBounds, columns: usize, rows: usize) -> Vec<char> {
    let data = include_bytes!("map/land.bin");
    let width = columns * 2;
    let height = rows * 4;
    let mut masks = vec![0u8; columns * rows];
    let mut crossings = Vec::with_capacity(256);
    let bits = [[1u8, 8], [2, 16], [4, 32], [64, 128]];
    for py in 0..height {
        crossings.clear();
        let lat = 90_000_000.0
            - (bounds.y as f64 + (py as f64 + 0.5) * bounds.height as f64 / height as f64);
        let mut offset = 0;
        while offset < data.len() {
            let count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            let point = |i: usize| {
                let at = offset + i * 8;
                (
                    i32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as f64,
                    i32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()) as f64,
                )
            };
            let mut previous = point(count - 1);
            for i in 0..count {
                let current = point(i);
                if (current.1 > lat) != (previous.1 > lat) {
                    crossings.push(
                        current.0
                            + (lat - current.1) * (previous.0 - current.0)
                                / (previous.1 - current.1),
                    );
                }
                previous = current;
            }
            offset += count * 8;
        }
        crossings.sort_unstable_by(f64::total_cmp);
        for span in crossings.chunks_exact(2) {
            for wrap in [-1, 0, 1] {
                let left =
                    span[0] + 180_000_000.0 + wrap as f64 * WORLD_WIDTH as f64 - bounds.x as f64;
                let right =
                    span[1] + 180_000_000.0 + wrap as f64 * WORLD_WIDTH as f64 - bounds.x as f64;
                let start = (left * width as f64 / bounds.width as f64 - 0.5)
                    .ceil()
                    .clamp(0.0, width as f64) as usize;
                let end = (right * width as f64 / bounds.width as f64 - 0.5)
                    .ceil()
                    .clamp(0.0, width as f64) as usize;
                for px in start..end {
                    masks[(py / 4) * columns + px / 2] |= bits[py % 4][px % 2];
                }
            }
        }
    }
    masks
        .into_iter()
        .map(|b| {
            if b == 0 {
                ' '
            } else {
                char::from_u32(0x2800 + b as u32).unwrap()
            }
        })
        .collect()
}

// Fixed count bands keep a color's meaning stable across pans and filters;
// a busy city must not wash out every other marker through max normalization.
const DENSITY_LABELS: [&str; 4] = ["1–9", "10–49", "50–199", "200+"];

fn density_band(count: u32) -> usize {
    match count {
        0..=9 => 0,
        10..=49 => 1,
        50..=199 => 2,
        _ => 3,
    }
}

fn density_styles(theme: crate::config::Theme) -> [Style; 4] {
    use crate::config::Theme;
    // Blue -> teal -> amber -> red. Darker counterparts remain legible on
    // the light theme's white background; Mono preserves NO_COLOR behavior.
    let colors = match theme {
        Theme::Default => [
            Color::Indexed(75),
            Color::Indexed(80),
            Color::Indexed(215),
            Color::Indexed(203),
        ],
        Theme::Light => [
            Color::Indexed(25),
            Color::Indexed(30),
            Color::Indexed(130),
            Color::Indexed(160),
        ],
        Theme::Mono => [Color::Reset; 4],
    };
    colors.map(|color| {
        let style = Style::default().fg(color);
        if theme == Theme::Mono {
            style.add_modifier(Modifier::UNDERLINED)
        } else {
            style
        }
    })
}

fn draw_density_legend(
    frame: &mut Frame,
    area: Rect,
    styles: [Style; 4],
    theme: crate::config::Theme,
) {
    use super::styles::{Role, style};
    let label = crate::t!("explore_density");
    let keys_width: usize = DENSITY_LABELS
        .iter()
        .map(|label| label.width() + 4)
        .sum::<usize>()
        - 2;
    let mut x = area.x;
    let buffer = frame.buffer_mut();
    if label.width() + 2 + keys_width <= usize::from(area.width) {
        buffer.set_string(x, area.y, label, style(theme, Role::Muted));
        x += label.width() as u16 + 2;
    }
    for (label, style) in DENSITY_LABELS.into_iter().zip(styles) {
        // Never paint a partial key that could misidentify its color band.
        let width = label.width() as u16 + 2;
        if x + width > area.right() {
            break;
        }
        buffer[(x, area.y)].set_char('⠿').set_style(style);
        buffer.set_string(
            x + 2,
            area.y,
            label,
            super::styles::style(theme, Role::Muted),
        );
        x += width + 2;
    }
}

pub(super) fn draw(
    frame: &mut Frame,
    state: &mut State,
    image: Option<&Arc<Image>>,
    error: Option<&str>,
    rect: Rect,
    focused: bool,
    theme: crate::config::Theme,
) {
    use super::styles::{Role, style};
    let block = Block::default()
        .borders(Borders::ALL)
        .title(crate::t!("explore_title", "zoom" => 1<<state.zoom))
        .border_style(style(
            theme,
            if focused { Role::Focus } else { Role::Border },
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width < 8 || inner.height < 3 {
        state.area = Rect::default();
        frame.render_widget(Paragraph::new(crate::t!("explore_small")), inner);
        return;
    }
    if state.chooser {
        state.area = Rect::default();
        let countries = image.map(|i| i.countries.as_slice()).unwrap_or_default();
        let start = state
            .country_index
            .saturating_sub(inner.height.saturating_sub(2) as usize);
        let mut lines = vec![crate::t!("explore_countries").to_string()];
        if image.is_none() {
            lines.push(
                error
                    .map(crate::clean_text)
                    .unwrap_or_else(|| crate::t!("explore_loading").into()),
            );
        }
        lines.extend(
            countries
                .iter()
                .enumerate()
                .skip(start)
                .take(inner.height.saturating_sub(1) as usize)
                .map(|(i, c)| {
                    format!(
                        "{} {} ({})",
                        if i == state.country_index { ">" } else { " " },
                        if c.value.is_empty() {
                            crate::t!("explore_unknown").to_string()
                        } else {
                            crate::clean_text(&crate::territories::display_in(
                                crate::messages::locale(),
                                &c.value,
                                &c.value,
                            ))
                        },
                        c.count
                    )
                }),
        );
        frame.render_widget(Paragraph::new(lines.join("\n")), inner);
        return;
    }
    let height = (inner.height - 2).min(80).min(inner.width / 4).max(1);
    let width = inner.width.min(height * 4).min(320);
    let area = Rect::new(inner.x + (inner.width - width) / 2, inner.y, width, height);
    state.resize(area);
    let matching = image.filter(|i| {
        i.request.bounds == state.bounds && (i.request.columns, i.request.rows) == state.size
    });
    let densities = density_styles(theme);
    let land_style = Style::default().fg(match theme {
        crate::config::Theme::Default => Color::Indexed(240),
        crate::config::Theme::Light => Color::Indexed(246),
        crate::config::Theme::Mono => Color::Reset,
    });
    if let Some(image) = matching {
        let cursor =
            state
                .bounds
                .project(state.cursor.0, state.cursor.1, state.size.0, state.size.1);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let i = y * width as usize + x;
                let selected = cursor == Some((x, y));
                let count = image.counts[i];
                // Highlight only the selected cell with a large circle; other
                // cells retain their original 2x4 Braille mask and density color.
                let symbol = if selected {
                    '●'
                } else if image.land[i] == ' ' && count > 0 {
                    '⠐'
                } else {
                    image.land[i]
                };
                let marker_style = if count > 0 {
                    densities[density_band(count)]
                } else if selected {
                    style(theme, Role::Focus)
                } else {
                    land_style
                };
                let cell = &mut frame.buffer_mut()[(area.x + x as u16, area.y + y as u16)];
                cell.set_char(symbol).set_style(marker_style);
            }
        }
    }
    draw_density_legend(
        frame,
        Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
        densities,
        theme,
    );
    let cell = state.cell();
    let count = matching.and_then(|i| {
        state
            .bounds
            .project(state.cursor.0, state.cursor.1, state.size.0, state.size.1)
            .map(|(x, y)| i.counts[y * state.size.0 + x])
    });
    let text = if let Some(error) = error {
        crate::clean_text(error)
    } else if let Some(count) = count {
        format!(
            "{} · {:.2}°, {:.2}° · {:.2}° × {:.2}°",
            crate::t!("explore_count","count"=>count),
            90.0 - state.cursor.1 as f64 / 1e6,
            state.cursor.0 as f64 / 1e6 - 180.0,
            cell.width as f64 / 1e6,
            cell.height as f64 / 1e6
        )
    } else {
        crate::t!("explore_loading").into()
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default()),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arrows_advance_exactly_one_cell_on_uneven_grids() {
        for (columns, rows) in [(68, 17), (76, 19), (319, 79)] {
            let mut state = State {
                size: (columns, rows),
                ..State::default()
            };
            let start = state
                .bounds
                .project(state.cursor.0, state.cursor.1, columns, rows)
                .unwrap();
            for step in 1..=columns * 2 {
                state.step(1, 0);
                assert_eq!(
                    state
                        .bounds
                        .project(state.cursor.0, state.cursor.1, columns, rows),
                    Some(((start.0 + step) % columns, start.1))
                );
            }
            for step in 1..=rows {
                state.step(0, 1);
                assert_eq!(
                    state
                        .bounds
                        .project(state.cursor.0, state.cursor.1, columns, rows),
                    Some((start.0, (start.1 + step).min(rows - 1)))
                );
            }
            state.step(-1, -1);
            assert_eq!(
                state
                    .bounds
                    .project(state.cursor.0, state.cursor.1, columns, rows),
                Some(((start.0 + columns - 1) % columns, rows - 2))
            );
        }
    }
    #[test]
    fn density_colors_preserve_braille_subpixels_in_every_theme() {
        use crate::config::Theme;
        use ratatui::{Terminal, backend::TestBackend};
        for theme in [Theme::Default, Theme::Light, Theme::Mono] {
            let rect = Rect::new(0, 0, 82, 24);
            let mut terminal = Terminal::new(TestBackend::new(rect.width, rect.height)).unwrap();
            let mut state = State::default();
            terminal
                .draw(|frame| draw(frame, &mut state, None, None, rect, true, theme))
                .unwrap();
            let (columns, rows) = state.size;
            let mut counts = vec![0; columns * rows];
            counts[..8].copy_from_slice(&[1, 9, 10, 49, 50, 199, 200, u32::MAX]);
            let request = Request {
                filter: Filter::default(),
                bounds: state.bounds,
                columns,
                rows,
                countries: false,
            };
            let shapes = ['⠁', '⠂', '⠃', '⠿', '⣀', '⣿', '⣤', '⣷', '⡇'];
            let mut land = vec!['⠿'; columns * rows];
            land[..9].copy_from_slice(&shapes);
            land[9..12].fill(' ');
            counts[9] = 10; // Offshore station, absent from the land mask.
            let image = Arc::new(Image {
                request,
                revision: 0,
                land: Arc::new(land),
                counts,
                countries: vec![],
            });
            let selected = state.bounds.cell(4, 0, columns, rows);
            state.cursor = (
                selected.x + selected.width / 2,
                selected.y + selected.height / 2,
            );
            terminal
                .draw(|frame| {
                    frame
                        .buffer_mut()
                        .set_style(rect, super::super::styles::base(theme));
                    draw(frame, &mut state, Some(&image), None, rect, true, theme);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(state.area.x + 9, state.area.y)].symbol(), "⠐");
            assert_eq!(buffer[(state.area.x + 11, state.area.y)].symbol(), " ");
            let marker = |column: u16| &buffer[(state.area.x + column, state.area.y)];
            for column in 0..8 {
                let expected = if column == 4 {
                    '●'
                } else {
                    shapes[column as usize]
                };
                assert_eq!(marker(column).symbol(), expected.to_string());
                assert!(!marker(column).modifier.contains(Modifier::BOLD));
                assert_eq!(
                    marker(column).modifier.contains(Modifier::UNDERLINED),
                    theme == Theme::Mono
                );
                assert!(!marker(column).modifier.contains(Modifier::REVERSED));
            }
            assert_eq!(marker(8).symbol(), "⡇");
            if theme != Theme::Mono {
                assert_ne!(marker(8).fg, marker(0).fg);
            }
            for column in [0, 2, 4, 6] {
                assert_eq!(marker(column).fg, marker(column + 1).fg);
            }
            if theme == Theme::Mono {
                for cell in &buffer.content {
                    assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
                }
            } else {
                for column in [0, 2, 4] {
                    assert_ne!(marker(column).fg, marker(column + 2).fg);
                }
                if theme == Theme::Light {
                    for column in 0..8 {
                        assert_ne!(marker(column).fg, marker(column).bg);
                    }
                }
            }
            let legend: String = (rect.x..rect.right())
                .map(|x| buffer[(x, rect.bottom() - 3)].symbol())
                .collect();
            for label in DENSITY_LABELS {
                assert!(legend.contains(label));
            }
            // Moving selection restores the old mask and marks even an empty
            // sea cell with a large circle, without a fictitious density.
            let empty = state.bounds.cell(10, 0, columns, rows);
            state.cursor = (empty.x + empty.width / 2, empty.y + empty.height / 2);
            terminal
                .draw(|frame| draw(frame, &mut state, Some(&image), None, rect, true, theme))
                .unwrap();
            let cell = &terminal.backend().buffer()[(state.area.x + 10, state.area.y)];
            assert_eq!(cell.symbol(), "●");
            assert!(!cell.modifier.contains(Modifier::REVERSED));
            assert_eq!(
                terminal.backend().buffer()[(state.area.x + 4, state.area.y)].symbol(),
                "⣀"
            );
        }
    }

    #[test]
    fn offline_raster_has_land_and_ocean_in_known_locations() {
        let land = raster(GeoBounds::default(), 180, 45);
        for (lat, lon, expected) in [
            (48.0, 10.0, true),
            (25.0, 15.0, true),
            (0.0, -140.0, false),
            (-25.0, -120.0, false),
        ] {
            let (x, y) = crate::geo::coordinates(Some(lat), Some(lon)).unwrap();
            let (x, y) = GeoBounds::default().project(x, y, 180, 45).unwrap();
            assert_eq!(land[y * 180 + x] != ' ', expected, "{lat},{lon}");
        }
    }
    #[test]
    fn zoom_keeps_pointer_anchor_and_pan_wraps_longitude() {
        let mut s = State {
            cursor: (1, 1),
            ..State::default()
        };
        let anchor = (350_000_000, 30_000_000);
        let before = s.bounds.project(anchor.0, anchor.1, 100, 25);
        s.zoom(true, anchor);
        assert_eq!(s.bounds.project(anchor.0, anchor.1, 100, 25), before);
        for _ in 0..20 {
            s.zoom(true, s.cursor);
        }
        assert_eq!(s.zoom, 3);
        for _ in 0..1000 {
            s.step(1, -1);
            assert!(
                s.bounds
                    .project(s.cursor.0, s.cursor.1, s.size.0, s.size.1)
                    .is_some()
            );
        }
        for _ in 0..20 {
            s.zoom(false, s.cursor);
        }
        assert_eq!(s.bounds, GeoBounds::default());
    }
    #[test]
    #[ignore = "release map raster timing"]
    fn map_raster_benchmark() {
        for (columns, rows) in [(100, 25), (320, 80)] {
            let mut samples = vec![];
            for _ in 0..20 {
                let start = Instant::now();
                std::hint::black_box(raster(GeoBounds::default(), columns, rows));
                samples.push(start.elapsed());
            }
            samples.sort();
            eprintln!(
                "raster {columns}x{rows}: P50 {:?}, P95 {:?}",
                samples[10], samples[18]
            );
            let request = Request {
                filter: Filter::default(),
                bounds: GeoBounds::default(),
                columns,
                rows,
                countries: false,
            };
            let image = Arc::new(Image {
                land: Arc::new(raster(request.bounds, columns, rows)),
                revision: 0,
                counts: vec![1; columns * rows],
                countries: vec![],
                request,
            });
            let mut state = State::default();
            let rect = Rect::new(0, 0, columns as u16 + 2, rows as u16 + 4);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(rect.width, rect.height))
                    .unwrap();
            let mut samples = vec![];
            for _ in 0..20 {
                state.cursor.0 = (state.cursor.0 + WORLD_WIDTH / columns as i64) % WORLD_WIDTH;
                let start = Instant::now();
                terminal
                    .draw(|frame| {
                        draw(
                            frame,
                            &mut state,
                            Some(&image),
                            None,
                            rect,
                            true,
                            crate::config::Theme::Default,
                        )
                    })
                    .unwrap();
                samples.push(start.elapsed());
            }
            samples.sort();
            eprintln!(
                "cached frame {columns}x{rows}: P50 {:?}, P95 {:?}",
                samples[10], samples[18]
            );
        }
    }
}
