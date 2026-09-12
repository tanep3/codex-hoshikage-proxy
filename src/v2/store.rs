use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::{Error, Result, id, now};

pub struct Store {
    db: Mutex<Connection>,
    pub root: PathBuf,
    pub instance: String,
    pub generation: String,
    _lock: File,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join("owner.lock"))?;
        // One owner even across independent server processes.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::code(503, "store_in_use"));
        }
        let mut db = Connection::open(root.join("metadata.sqlite3"))?;
        std::fs::set_permissions(
            root.join("metadata.sqlite3"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS record_sequences(sequence INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,id TEXT NOT NULL,UNIQUE(kind,id));
            CREATE TABLE IF NOT EXISTS records (kind TEXT NOT NULL, id TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(kind,id));
            CREATE TABLE IF NOT EXISTS operations (request_key TEXT PRIMARY KEY, id TEXT UNIQUE NOT NULL, fingerprint TEXT NOT NULL, value TEXT NOT NULL);")?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: Option<String> = tx
            .query_row("SELECT value FROM metadata WHERE key='schema'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if version.as_deref().is_some_and(|v| v != "2") {
            return Err(Error::code(503, "schema_mismatch"));
        }
        tx.execute("INSERT OR IGNORE INTO metadata VALUES ('schema','2')", [])?;
        for (key, value) in [
            ("instance", id("pxy")),
            ("generation", id("gen")),
            ("recovery_state", "ready".into()),
        ] {
            tx.execute(
                "INSERT OR IGNORE INTO metadata VALUES (?1,?2)",
                params![key, value],
            )?;
        }
        let instance =
            tx.query_row("SELECT value FROM metadata WHERE key='instance'", [], |r| {
                r.get(0)
            })?;
        let generation = tx.query_row(
            "SELECT value FROM metadata WHERE key='generation'",
            [],
            |r| r.get(0),
        )?;
        // An interrupted read-only backup has no surviving worker after owner-lock recovery.
        tx.execute("UPDATE metadata SET value='ready' WHERE key='recovery_state' AND value='backup_blocked'",[])?;
        // Dispatching is never replayable after process death. Accepted is safe to resume.
        for mut r in list(&tx, "response")? {
            if matches!(r["phase"].as_str(), Some("dispatching" | "started")) {
                r["phase"] = json!("unknown");
                if r["interrupt_delivery"] == "dispatching" {
                    r["interrupt_delivery"] = json!("unknown");
                }
                r["hold_revision"] = json!(r["hold_revision"].as_u64().unwrap_or(0) + 1);
                r["execution_status"] = json!("unknown");
                put(&tx, "response", r["response_id"].as_str().unwrap(), &r)?;
                if let Some(key) = r["request_key"].as_str()
                    && let Some(mut op) = operation(&tx, key)?
                    && (op["state"] == "accepted" || op["state"] == "running")
                {
                    op["state"] = json!("unknown");
                    save_operation(&tx, &op)?;
                }
            }
        }
        for mut h in list(&tx, "legacy_hold")? {
            if h["state"] == "held" || h["state"] == "administratively_released" {
                h["phase"] = json!("unknown");
                h["execution_status"] = json!("unknown");
                h["hold_state"] = h["state"].clone();
                h["hold_revision"] = json!(h["hold_revision"].as_u64().unwrap_or(0) + 1);
                h["restart_hold"] = json!(true);
                put(&tx, "legacy_hold", h["response_id"].as_str().unwrap(), &h)?;
            }
        }
        super::interactions::recover(&tx)?;
        tx.commit()?;
        for dir in ["blobs", "staging"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self {
            db: Mutex::new(db),
            root: root.into(),
            instance,
            generation,
            _lock: lock,
        })
    }
    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| Error::code(503, "store_unavailable"))?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn get(&self, kind: &str, id: &str) -> Result<Value> {
        self.transaction(|tx| get(tx, kind, id))
    }
    pub fn list(&self, kind: &str) -> Result<Vec<Value>> {
        self.transaction(|tx| list(tx, kind))
    }
    pub fn update(
        &self,
        kind: &str,
        id: &str,
        f: impl FnOnce(&mut Value) -> Result<()>,
    ) -> Result<Value> {
        self.transaction(|tx| {
            let mut value = get(tx, kind, id)?;
            f(&mut value)?;
            put(tx, kind, id, &value)?;
            Ok(value)
        })
    }
}
pub fn get(tx: &Transaction<'_>, kind: &str, id: &str) -> Result<Value> {
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM records WHERE kind=?1 AND id=?2",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?;
    serde_json::from_str(&value.ok_or_else(|| Error::code(404, "resource_not_found"))?)
        .map_err(Into::into)
}
pub fn put(tx: &Transaction<'_>, kind: &str, id: &str, value: &Value) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO record_sequences(kind,id) VALUES (?1,?2)",
        params![kind, id],
    )?;
    tx.execute("INSERT INTO records VALUES (?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value",params![kind,id,serde_json::to_string(value)?])?;
    Ok(())
}
pub fn list(tx: &Transaction<'_>, kind: &str) -> Result<Vec<Value>> {
    let mut stmt = tx.prepare("SELECT value FROM records WHERE kind=?1 ORDER BY id")?;
    let rows = stmt.query_map([kind], |r| r.get::<_, String>(0))?;
    rows.map(|v| Ok(serde_json::from_str(&v?)?)).collect()
}
pub fn operation(tx: &Transaction<'_>, key: &str) -> Result<Option<Value>> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM operations WHERE request_key=?1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|r| serde_json::from_str(&r).map_err(Into::into))
        .transpose()
}
pub fn reserve(tx: &Transaction<'_>, key: &str, kind: &str, body: &Value) -> Result<(Value, bool)> {
    let fingerprint = crate::control::fingerprint(&json!({ "kind":kind,"body":body}));
    if let Some(value) = operation(tx, key)? {
        if value["fingerprint"] != fingerprint {
            return Err(Error::code(409, "idempotency_conflict"));
        }
        return Ok((value, false));
    }
    let value = json!({
        "operation_id":id("op"),
        "request_key":key,
        "kind":kind,
        "fingerprint":fingerprint,
        "state":"accepted",
        "created_at_ms":now(),
        "error":null
    });
    save_operation(tx, &value)?;
    Ok((value, true))
}
pub fn save_operation(tx: &Transaction<'_>, v: &Value) -> Result<()> {
    tx.execute("INSERT INTO operations VALUES (?1,?2,?3,?4) ON CONFLICT(request_key) DO UPDATE SET value=excluded.value",params![v["request_key"].as_str(),v["operation_id"].as_str(),v["fingerprint"].as_str(),v.to_string()])?;
    Ok(())
}
pub fn public_operation(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("fingerprint");
        o.remove("request_key");
    }
    v
}

impl Store {
    pub fn metadata(&self, key: &str) -> Result<String> {
        self.transaction(|tx| {
            Ok(
                tx.query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
                    r.get(0)
                })?,
            )
        })
    }
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.transaction(|tx|{
tx.execute("INSERT INTO metadata VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
Ok(())}
)
    }
    pub fn backup_database(&self, path: &Path) -> Result<()> {
        let db = self
            .db
            .lock()
            .map_err(|_| Error::code(503, "store_unavailable"))?;
        db.execute(
            "VACUUM INTO ?1",
            [path
                .to_str()
                .ok_or_else(|| Error::code(400, "invalid_argument"))?],
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        File::open(path)?.sync_all()?;
        Ok(())
    }
}
