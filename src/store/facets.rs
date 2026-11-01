use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};

type Values = BTreeSet<(u8, String)>;

fn values(country: String, languages: String, tags: String) -> Values {
    let mut values = Values::new();
    if !country.is_empty() {
        values.insert((0, country));
    }
    for (kind, text) in [(1, languages), (2, tags)] {
        values.extend(
            text.split(',')
                .filter(|v| !v.is_empty())
                .map(|v| (kind, v.to_owned())),
        );
    }
    values
}

pub(super) fn station_values(conn: &Connection, uuid: &str) -> Result<Values> {
    Ok(conn
        .query_row(
            "SELECT country,languages,tags FROM stations WHERE uuid=?1 AND active=1",
            [uuid],
            |r| Ok(values(r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .unwrap_or_default())
}

pub(super) fn update(conn: &Connection, before: &Values, after: &Values) -> Result<()> {
    for (kind, value) in before.difference(after) {
        conn.prepare_cached("UPDATE facets_cache SET n=n-1 WHERE kind=?1 AND value=?2")?
            .execute(params![kind, value])?;
        conn.prepare_cached("DELETE FROM facets_cache WHERE kind=?1 AND value=?2 AND n=0")?
            .execute(params![kind, value])?;
    }
    for (kind, value) in after.difference(before) {
        conn.prepare_cached(
            "INSERT INTO facets_cache(kind,value,n) VALUES (?1,?2,1)
            ON CONFLICT(kind,value) DO UPDATE SET n=n+1",
        )?
        .execute(params![kind, value])?;
    }
    Ok(())
}

/// Compare only changed facet memberships before replacing directory rows.
/// Custom stations win UUID collisions and retain their existing memberships.
pub(super) fn publication_delta(conn: &Connection) -> Result<BTreeMap<(u8, String), i64>> {
    let mut delta = BTreeMap::new();
    for (sql, sign) in [
        (
            "SELECT s.country,s.languages,s.tags FROM stations s
          LEFT JOIN incoming i ON i.uuid=s.uuid
          WHERE s.custom=0 AND s.active=1 AND (i.uuid IS NULL OR
            s.country IS NOT i.country OR s.languages IS NOT i.languages OR s.tags IS NOT i.tags)",
            -1,
        ),
        (
            "SELECT i.country,i.languages,i.tags FROM incoming i
          LEFT JOIN stations s ON s.uuid=i.uuid
          WHERE s.uuid IS NULL OR (s.custom=0 AND (s.active<>1 OR
            s.country IS NOT i.country OR s.languages IS NOT i.languages OR s.tags IS NOT i.tags))",
            1,
        ),
    ] {
        let mut stmt = conn.prepare(sql)?;
        for row in stmt.query_map([], |r| Ok(values(r.get(0)?, r.get(1)?, r.get(2)?)))? {
            for value in row? {
                *delta.entry(value).or_default() += sign;
            }
        }
    }
    Ok(delta)
}

pub(super) fn apply_delta(conn: &Connection, delta: BTreeMap<(u8, String), i64>) -> Result<()> {
    for ((kind, value), n) in delta {
        if n > 0 {
            conn.prepare_cached(
                "INSERT INTO facets_cache(kind,value,n) VALUES (?1,?2,?3)
                 ON CONFLICT(kind,value) DO UPDATE SET n=n+excluded.n",
            )?
            .execute(params![kind, value, n])?;
        } else if n < 0 {
            conn.prepare_cached("UPDATE facets_cache SET n=n+?3 WHERE kind=?1 AND value=?2")?
                .execute(params![kind, value, n])?;
            conn.prepare_cached("DELETE FROM facets_cache WHERE kind=?1 AND value=?2 AND n=0")?
                .execute(params![kind, value])?;
        }
    }
    Ok(())
}

/// Called inside the publication transaction, never for the staging table.
pub(super) fn rebuild(conn: &Connection) -> Result<()> {
    let mut counts = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT country,languages,tags FROM stations WHERE active=1")?;
    let rows = stmt.query_map([], |r| Ok(values(r.get(0)?, r.get(1)?, r.get(2)?)))?;
    for row in rows {
        for value in row? {
            *counts.entry(value).or_insert(0_i64) += 1;
        }
    }
    conn.execute("DELETE FROM facets_cache", [])?;
    let mut insert = conn.prepare("INSERT INTO facets_cache(kind,value,n) VALUES (?1,?2,?3)")?;
    for ((kind, value), n) in counts {
        insert.execute(params![kind, value, n])?;
    }
    Ok(())
}
