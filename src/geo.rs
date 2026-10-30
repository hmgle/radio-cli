//! Integer geographic grid shared by storage, drawing and mouse hit testing.
pub const WORLD_WIDTH: i64 = 360_000_000;
pub const WORLD_HEIGHT: i64 = 180_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeoBounds {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}
impl Default for GeoBounds {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: WORLD_WIDTH,
            height: WORLD_HEIGHT,
        }
    }
}
impl GeoBounds {
    pub fn cell(self, col: usize, row: usize, columns: usize, rows: usize) -> Self {
        let edge =
            |n: usize, size: i64, count: usize| (n as i64 * size + count as i64 - 1) / count as i64;
        let left = edge(col, self.width, columns);
        let top = edge(row, self.height, rows);
        Self {
            x: (self.x + left).rem_euclid(WORLD_WIDTH),
            y: self.y + top,
            width: edge(col + 1, self.width, columns) - left,
            height: edge(row + 1, self.height, rows) - top,
        }
    }
    pub fn project(self, x: i64, y: i64, columns: usize, rows: usize) -> Option<(usize, usize)> {
        let dx = (x - self.x).rem_euclid(WORLD_WIDTH);
        let dy = y - self.y;
        (dx < self.width && dy >= 0 && dy < self.height).then(|| {
            (
                (dx * columns as i64 / self.width) as usize,
                (dy * rows as i64 / self.height) as usize,
            )
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeoFilter {
    Cell(GeoBounds),
    Missing,
}

pub fn coordinates(lat: Option<f64>, lon: Option<f64>) -> Option<(i64, i64)> {
    let (lat, lon) = (lat?, lon?);
    if !lat.is_finite()
        || !lon.is_finite()
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
    {
        return None;
    }
    Some((
        ((lon + 180.0) * 1e6).round() as i64 % WORLD_WIDTH,
        (((90.0 - lat) * 1e6).round() as i64).clamp(0, WORLD_HEIGHT - 1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cells_partition_wrapped_viewports_without_gaps_or_overlap() {
        for b in [
            GeoBounds::default(),
            GeoBounds {
                x: 350_000_000,
                y: 10,
                width: 45_000_000,
                height: 22_500_000,
            },
        ] {
            for (w, h) in [(7, 3), (100, 25), (319, 79)] {
                for (lat, lon) in [
                    (-90.0, -180.0),
                    (90.0, 180.0),
                    (0.0, 179.99),
                    (89.0, -179.99),
                    (45.0, 170.0),
                ] {
                    let (x, y) = coordinates(Some(lat), Some(lon)).unwrap();
                    let projected = b.project(x, y, w, h);
                    let hits: Vec<_> = (0..h)
                        .flat_map(|r| (0..w).map(move |c| (c, r)))
                        .filter(|&(c, r)| b.cell(c, r, w, h).project(x, y, 1, 1).is_some())
                        .collect();
                    assert_eq!(hits, projected.into_iter().collect::<Vec<_>>());
                }
            }
        }
    }
}
