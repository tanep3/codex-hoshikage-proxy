//! Durable storage for the OpenAI-compatible control API.
//! Imports the old V2-owned legacy records once, without mutating the old database.
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use std::path::Path;

fn io(error: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::other(error)
}

pub fn open(root: &Path) -> std::io::Result<Connection> {
    std::fs::create_dir_all(root)?;
    let path = root.join("control.sqlite3");
    let mut db = Connection::open(path).map_err(io)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA busy_timeout=5000;
         CREATE TABLE IF NOT EXISTS control_records(
           kind TEXT NOT NULL,
           id TEXT NOT NULL,
           value TEXT NOT NULL,
           PRIMARY KEY(kind,id)
         );
         CREATE TABLE IF NOT EXISTS control_metadata(
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );",
    )
    .map_err(io)?;

    let imported: Option<String> = db
        .query_row(
            "SELECT value FROM control_metadata WHERE key='initial_import'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(io)?;
    if imported.is_none() {
        import_existing(&mut db, root)?;
    }
    Ok(db)
}

fn import_existing(db: &mut Connection, root: &Path) -> std::io::Result<()> {
    let old_v2 = root
        .parent()
        .ok_or_else(|| std::io::Error::other("missing response state parent"))?
        .join("v2/metadata.sqlite3");
    let tx = db.transaction().map_err(io)?;
    let mut sources = Vec::new();

    if old_v2.exists() {
        let old = Connection::open_with_flags(
            &old_v2,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(io)?;
        for kind in ["execution", "mapping"] {
            let mut statement = old
                .prepare("SELECT id,value FROM legacy_records WHERE kind=?1 ORDER BY id")
                .map_err(io)?;
            let rows = statement
                .query_map([kind], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(io)?;
            let mut count = 0usize;
            for row in rows {
                let (id, raw) = row.map_err(io)?;
                let _: Value = serde_json::from_str(&raw).map_err(io)?;
                tx.execute(
                    "INSERT OR REPLACE INTO control_records(kind,id,value) VALUES (?1,?2,?3)",
                    params![kind, id, raw],
                )
                .map_err(io)?;
                count += 1;
            }
            sources.push(format!("old_v2:{kind}:{count}"));
        }
    }

    for (kind, name) in [
        ("execution", "executions.jsonl"),
        ("mapping", "mappings.jsonl"),
    ] {
        let path = root.join(name);
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut count = 0usize;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value = serde_json::from_str(line).map_err(io)?;
            let id = value["response_id"]
                .as_str()
                .ok_or_else(|| std::io::Error::other("control import missing response_id"))?;
            tx.execute(
                "INSERT OR IGNORE INTO control_records(kind,id,value) VALUES (?1,?2,?3)",
                params![kind, id, line],
            )
            .map_err(io)?;
            count += 1;
        }
        sources.push(format!("jsonl:{kind}:{count}"));
    }

    tx.execute(
        "INSERT INTO control_metadata(key,value) VALUES ('initial_import',?1)",
        [sources.join(",")],
    )
    .map_err(io)?;
    tx.commit().map_err(io)?;
    Ok(())
}

pub fn read(db: &Connection, kind: &str) -> std::io::Result<Vec<Value>> {
    let mut statement = db
        .prepare("SELECT value FROM control_records WHERE kind=?1 ORDER BY id")
        .map_err(io)?;
    let rows = statement
        .query_map([kind], |row| row.get::<_, String>(0))
        .map_err(io)?;
    rows.map(|row| serde_json::from_str(&row.map_err(io)?).map_err(io))
        .collect()
}

pub fn put(db: &Connection, kind: &str, id: &str, value: &Value) -> std::io::Result<()> {
    db.execute(
        "INSERT INTO control_records(kind,id,value) VALUES (?1,?2,?3)
         ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value",
        params![kind, id, value.to_string()],
    )
    .map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_legacy_v2_records_once_without_mutating_the_source() {
        let base =
            std::env::temp_dir().join(format!("control-db-migration-{}", uuid::Uuid::new_v4()));
        let responses = base.join("responses");
        let legacy = base.join("v2/metadata.sqlite3");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let old = Connection::open(&legacy).unwrap();
        old.execute_batch(
            "CREATE TABLE legacy_records(
               kind TEXT NOT NULL,
               id TEXT NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY(kind,id)
             );",
        )
        .unwrap();
        let execution = serde_json::json!({"response_id":"response-1","phase":"finished"});
        let mapping = serde_json::json!({"response_id":"response-1","thread_id":"thread-1"});
        old.execute(
            "INSERT INTO legacy_records(kind,id,value) VALUES ('execution','response-1',?1)",
            [execution.to_string()],
        )
        .unwrap();
        old.execute(
            "INSERT INTO legacy_records(kind,id,value) VALUES ('mapping','response-1',?1)",
            [mapping.to_string()],
        )
        .unwrap();
        drop(old);

        let db = open(&responses).unwrap();
        assert_eq!(read(&db, "execution").unwrap(), vec![execution.clone()]);
        assert_eq!(read(&db, "mapping").unwrap(), vec![mapping]);
        drop(db);

        // A later legacy change cannot overwrite the independent V1 store.
        let old = Connection::open(&legacy).unwrap();
        old.execute(
            "UPDATE legacy_records SET value=?1 WHERE kind='execution' AND id='response-1'",
            [serde_json::json!({"response_id":"response-1","phase":"unknown"}).to_string()],
        )
        .unwrap();
        drop(old);
        let reopened = open(&responses).unwrap();
        assert_eq!(read(&reopened, "execution").unwrap(), vec![execution]);

        std::fs::remove_dir_all(base).unwrap();
    }
}
