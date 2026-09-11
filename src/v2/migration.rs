//! One-time, verified import of the legacy ledgers. Original bytes remain archived.
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path};
fn io(e: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::other(e)
}
pub fn open(root: &Path) -> std::io::Result<Option<Connection>> {
    let path = root
        .parent()
        .ok_or_else(|| std::io::Error::other("missing state parent"))?
        .join("v2/metadata.sqlite3");
    if !path.exists() {
        return Ok(None);
    }
    let mut db = Connection::open(path).map_err(io)?;
    db.execute_batch("PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; CREATE TABLE IF NOT EXISTS legacy_records(kind TEXT NOT NULL,id TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(kind,id));").map_err(io)?;
    let tx = db.transaction().map_err(io)?;
    let migrated: Option<String> = tx
        .query_row(
            "SELECT value FROM metadata WHERE key='legacy_migrated'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(io)?;
    if migrated.is_none() {
        let archive = root.join("pre-v2");
        std::fs::create_dir_all(&archive)?;
        let mut evidence = Vec::new();
        for (kind, name) in [
            ("execution", "executions.jsonl"),
            ("mapping", "mappings.jsonl"),
        ] {
            let bytes = match std::fs::read(root.join(name)) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e),
            };
            let text = std::str::from_utf8(&bytes).map_err(io)?;
            let mut count = 0;
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let value = if kind == "execution" {
                    serde_json::to_value(
                        serde_json::from_str::<crate::control::Execution>(line).map_err(io)?,
                    )
                    .map_err(io)?
                } else {
                    serde_json::to_value(
                        serde_json::from_str::<crate::store::ResponseMapping>(line).map_err(io)?,
                    )
                    .map_err(io)?
                };
                let id = value["response_id"]
                    .as_str()
                    .ok_or_else(|| std::io::Error::other("missing legacy response id"))?;
                tx.execute("INSERT INTO legacy_records VALUES (?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value",params![kind,id,value.to_string()]).map_err(io)?;
                count += 1;
            }
            let archived = archive.join(name);
            if archived.exists() {
                if std::fs::read(&archived)? != bytes {
                    return Err(std::io::Error::other(
                        "legacy archive differs; inspect migration before continuing",
                    ));
                }
            } else {
                let pending = archive.join(format!("{}.pending-{}", name, uuid::Uuid::new_v4()));
                let mut f = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(&pending)?;
                f.write_all(&bytes)?;
                f.sync_all()?;
                std::fs::rename(&pending, &archived)?;
            }
            evidence.push(json!({
                "file":name,
                "sha256":format!("{:x}",Sha256::digest(&bytes)),
                "lines":count
            }));
        }
        std::fs::File::open(&archive)?.sync_all()?;
        std::fs::File::open(root)?.sync_all()?;
        let raw_records = {
            let mut q = tx
                .prepare("SELECT value FROM legacy_records WHERE kind='execution'")
                .map_err(io)?;
            q.query_map([], |r| r.get::<_, String>(0))
                .map_err(io)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(io)?
        };
        for raw in raw_records {
            let r: crate::control::Execution = serde_json::from_str(&raw).map_err(io)?;
            if matches!(r.phase.as_str(), "dispatching" | "started" | "unknown")
                && !crate::control::terminal(&r.last_observed_status)
            {
                let hold = json!({
                    "response_id":r.response_id,
                    "thread_id":r.thread_id,
                    "turn_id":r.turn_id,
                    "path":r.cwd.as_deref().unwrap_or("/"),
                    "provider":r.model_id.as_deref().and_then(|m|m.split('/').next()),
                    "state":"held",
                    "phase":"unknown",
                    "execution_status":"unknown",
                    "hold_state":"held",
                    "hold_revision":1,
                    "last_observed_status":r.last_observed_status
                });
                crate::v2::store::put(&tx, "legacy_hold", &r.response_id, &hold).map_err(io)?;
            }
        }
        tx.execute(
            "INSERT INTO metadata VALUES ('legacy_migrated',?1)",
            [json!({ "sources":evidence}).to_string()],
        )
        .map_err(io)?;
    }
    tx.commit().map_err(io)?;
    Ok(Some(db))
}
pub fn read(db: &Connection, kind: &str) -> std::io::Result<Vec<Value>> {
    let mut stmt = db
        .prepare("SELECT value FROM legacy_records WHERE kind=?1 ORDER BY id")
        .map_err(io)?;
    let rows = stmt
        .query_map([kind], |r| r.get::<_, String>(0))
        .map_err(io)?;
    rows.map(|r| serde_json::from_str(&r.map_err(io)?).map_err(io))
        .collect()
}
pub fn put(db: &Connection, kind: &str, id: &str, value: &Value) -> std::io::Result<()> {
    db.execute("INSERT INTO legacy_records VALUES (?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value",params![kind,id,value.to_string()]).map_err(io)?;
    Ok(())
}
