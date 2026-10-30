use super::*;
use crate::geo::{GeoBounds, GeoFilter, WORLD_WIDTH};

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE stations ADD COLUMN geo_x INTEGER;
        ALTER TABLE stations ADD COLUMN geo_y INTEGER;
        CREATE INDEX station_geo ON stations(geo_x,geo_y) WHERE active=1 AND geo_x IS NOT NULL;
        INSERT INTO meta(key,value) VALUES ('geo_revision','1');",
    )?;
    let mut read = conn.prepare("SELECT rowid,json FROM stations WHERE json_extract(json,'$.geo_lat') IS NOT NULL AND json_extract(json,'$.geo_long') IS NOT NULL")?;
    let mut write = conn.prepare("UPDATE stations SET geo_x=?2,geo_y=?3 WHERE rowid=?1")?;
    for row in read.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, json) = row?;
        let s: Station = serde_json::from_str(&json)?;
        if let Some((x, y)) = crate::geo::coordinates(s.geo_lat, s.geo_long) {
            write.execute(params![id, x, y])?;
        }
    }
    Ok(())
}
pub(super) fn changed(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='geo_revision'",
        [],
    )?;
    Ok(())
}
pub(super) fn predicate(b: GeoBounds, args: &mut Vec<Value>) -> String {
    args.extend([b.y.into(), (b.y + b.height).into()]);
    let mut sql = "s.geo_x IS NOT NULL AND s.geo_y>=? AND s.geo_y<?".to_owned();
    if b.width < WORLD_WIDTH {
        args.push(b.x.into());
        args.push(((b.x + b.width) % WORLD_WIDTH).into());
        sql.push_str(if b.x + b.width < WORLD_WIDTH {
            " AND (s.geo_x>=? AND s.geo_x<?)"
        } else {
            " AND (s.geo_x>=? OR s.geo_x<?)"
        });
    }
    sql
}
impl Store {
    pub fn geo_revision(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='geo_revision'",
            [],
            |r| r.get(0),
        )?)
    }
    /// Stream compact coordinates into a bounded grid; never retain the atlas.
    pub fn geo_counts(
        &self,
        filter: &Filter,
        bounds: GeoBounds,
        columns: usize,
        rows: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<u32>> {
        anyhow::ensure!(
            (1..=320).contains(&columns) && (1..=80).contains(&rows),
            "invalid map size"
        );
        let mut f = filter.clone();
        f.geo = Some(GeoFilter::Cell(bounds));
        let plain = f.query.trim().is_empty()
            && f.country.is_none()
            && f.language.is_none()
            && f.tag.is_none()
            && f.min_bitrate == 0
            && !f.custom
            && !f.favorites
            && !f.history;
        let (conditions, args, source) = if plain {
            let mut args = vec![];
            (
                format!("s.active=1 AND {}", predicate(bounds, &mut args)),
                args,
                "stations s INDEXED BY station_geo",
            )
        } else {
            self.conditions(&f)
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT s.geo_x,s.geo_y FROM {source} WHERE {conditions}"
        ))?;
        let mut points = stmt.query(params_from_iter(args))?;
        let mut grid = vec![0u32; columns * rows];
        let mut n = 0usize;
        while let Some(row) = points.next()? {
            if n.is_multiple_of(1024) && cancelled() {
                bail!(DomainError::new(ErrorCode::Cancelled));
            }
            n += 1;
            if let Some((x, y)) = bounds.project(row.get(0)?, row.get(1)?, columns, rows) {
                grid[y * columns + x] = grid[y * columns + x].saturating_add(1);
            }
        }
        Ok(grid)
    }
    pub fn geo_missing_countries(&self, filter: &Filter) -> Result<Vec<crate::model::Facet>> {
        let mut f = filter.clone();
        f.geo = Some(GeoFilter::Missing);
        f.country = None;
        let (conditions, args, source) = self.conditions(&f);
        let mut stmt=self.conn.prepare_cached(&format!("SELECT s.country,count(*) FROM {source} WHERE {conditions} GROUP BY s.country ORDER BY s.country LIMIT 1000"))?;
        Ok(stmt
            .query_map(params_from_iter(args), |r| {
                Ok(crate::model::Facet {
                    value: r.get(0)?,
                    count: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}
