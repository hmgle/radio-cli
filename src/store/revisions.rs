use super::*;

/// An opaque list cache key for one database, scoped to its filter's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListRevision([u64; 4]);

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS content_revisions (
            name TEXT PRIMARY KEY,n INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;",
    )?;
    for table in ["stations", "favorites", "history", "recordings"] {
        conn.execute(
            "INSERT OR IGNORE INTO content_revisions(name) VALUES (?1)",
            [table],
        )?;
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            // Triggers observe writes from every connection, including another
            // CLI process, while preferences and planner hints stay unrelated.
            conn.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS revision_{table}_{operation}
                 AFTER {operation} ON {table} BEGIN
                 UPDATE content_revisions SET n=n+1 WHERE name='{table}'; END;"
            ))?;
        }
    }
    Ok(())
}

impl Store {
    /// Track content relevant to a station or recording list across connections.
    /// Preferences and maintenance do not invalidate this key. Compare keys
    /// only for the same database and filter; read rows in the same snapshot
    /// when coupling a key to cached query results.
    pub fn list_revision(&self, filter: &Filter) -> Result<ListRevision> {
        let revisions = self.conn.query_row(
            "SELECT
                (SELECT n FROM content_revisions WHERE name='stations'),
                (SELECT n FROM content_revisions WHERE name='favorites'),
                (SELECT n FROM content_revisions WHERE name='history'),
                (SELECT n FROM content_revisions WHERE name='recordings')",
            [],
            |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]),
        )?;
        Ok(ListRevision(if filter.recordings {
            [0, 0, 0, revisions[3]]
        } else {
            [
                revisions[0],
                revisions[1],
                if filter.history { revisions[2] } else { 0 },
                0,
            ]
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_six_migration_tracks_external_writes_and_rolled_back_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        for table in ["stations", "favorites", "history", "recordings"] {
            for operation in ["INSERT", "UPDATE", "DELETE"] {
                store
                    .conn
                    .execute_batch(&format!("DROP TRIGGER revision_{table}_{operation}"))
                    .unwrap();
            }
        }
        store
            .conn
            .execute_batch("DROP TABLE content_revisions; PRAGMA user_version=6")
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "existing".into(),
            active: true,
            ..Station::default()
        };
        store.upsert(&station).unwrap();
        let filter = Filter::default();
        let before = store.list_revision(&filter).unwrap();
        let external = Connection::open(&path).unwrap();
        external.execute_batch("BEGIN").unwrap();
        external
            .execute(
                "INSERT INTO favorites VALUES (?1,'now')",
                [&station.stationuuid],
            )
            .unwrap();
        external.execute_batch("ROLLBACK").unwrap();
        assert_eq!(store.list_revision(&filter).unwrap(), before);
        external
            .execute(
                "INSERT INTO favorites VALUES (?1,'now')",
                [&station.stationuuid],
            )
            .unwrap();
        assert_ne!(store.list_revision(&filter).unwrap(), before);
        let after = store.list_revision(&filter).unwrap();
        external
            .execute("INSERT INTO meta VALUES ('pref_volume','60')", [])
            .unwrap();
        assert_eq!(store.list_revision(&filter).unwrap(), after);
        drop(store);
        assert_eq!(
            Store::open(&path).unwrap().list_revision(&filter).unwrap(),
            after
        );
    }
}
