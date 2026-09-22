//! Durable control metadata. No prompts or generated output are stored here.
//! A synchronous append + sync_all inside the mutex is deliberately cancellation-safe.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::Mutex,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    pub response_id: String,
    pub sequence: u64,
    pub client_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub phase: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub last_observed_status: String,
    pub last_observed_at_ms: u128,
    pub started_at_ms: u128,
    pub suppress_auto_approval: bool,
    pub interrupt_state: Option<String>,
}

pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "interrupted")
}

pub fn fingerprint(value: &Value) -> String {
    // serde_json's default Map is a BTreeMap, recursively canonicalizing keys.
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON value"))
    )
}

impl Execution {
    pub fn new(response_id: String, key: Option<String>, fingerprint: Option<String>) -> Self {
        Self {
            response_id,
            sequence: 0,
            client_request_id: key,
            fingerprint,
            phase: "received".into(),
            thread_id: None,
            turn_id: None,
            model_id: None,
            cwd: None,
            last_observed_status: "unknown".into(),
            last_observed_at_ms: crate::journal::now_ms(),
            started_at_ms: crate::journal::now_ms(),
            suppress_auto_approval: false,
            interrupt_state: None,
        }
    }
    pub fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("execution record");
        v.as_object_mut().unwrap().remove("fingerprint");
        v["output_retrieval"] = Value::String("unavailable".into());
        v
    }
}

struct Inner {
    file: File,
    database: rusqlite::Connection,
    records: HashMap<String, Execution>,
    failed: bool,
}

pub struct ControlStore {
    inner: Mutex<Inner>,
}

impl ControlStore {
    pub fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let path = root.join("executions.jsonl");
        let mut records = HashMap::new();
        let database = crate::control_db::open(root)?;
        let contents = Ok::<_, std::io::Error>(
            crate::control_db::read(&database, "execution")?
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        match contents {
            Ok(contents) => {
                // Fail closed even on a torn tail: never silently forget a reserved ID.
                for line in contents.lines() {
                    let mut record: Execution = serde_json::from_str(line)?;
                    if matches!(record.phase.as_str(), "dispatching" | "started")
                        && !terminal(&record.last_observed_status)
                    {
                        record.phase = "unknown".into();
                    }
                    records.insert(record.response_id.clone(), record);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        file.sync_all()?;
        File::open(root)?.sync_all()?;
        Ok(Self {
            inner: Mutex::new(Inner {
                file,
                database,
                records,
                failed: false,
            }),
        })
    }

    pub fn records(&self) -> std::io::Result<Vec<Execution>> {
        let inner = self.inner.lock().unwrap();
        if inner.failed {
            return Err(std::io::Error::other("control store is unavailable"));
        }
        Ok(inner.records.values().cloned().collect())
    }
    pub fn get(&self, id: &str) -> std::io::Result<Option<Execution>> {
        Ok(self.records()?.into_iter().find(|r| r.response_id == id))
    }
    pub fn by_turn(&self, id: &str) -> std::io::Result<Option<Execution>> {
        Ok(self
            .records()?
            .into_iter()
            .find(|r| r.turn_id.as_deref() == Some(id)))
    }
    pub fn by_request(&self, id: &str) -> std::io::Result<Option<Execution>> {
        Ok(self
            .records()?
            .into_iter()
            .find(|r| r.client_request_id.as_deref() == Some(id)))
    }
    /// Returns existing record on duplicate; caller compares fingerprints.
    pub fn reserve(&self, mut record: Execution) -> std::io::Result<Option<Execution>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.failed {
            return Err(std::io::Error::other("control store is unavailable"));
        }
        if let Some(key) = &record.client_request_id
            && let Some(existing) = inner
                .records
                .values()
                .find(|r| r.client_request_id.as_ref() == Some(key))
        {
            return Ok(Some(existing.clone()));
        }
        record.sequence = inner
            .records
            .values()
            .map(|r| r.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("sequence exhausted"))?;
        Self::append(&mut inner, record)?;
        Ok(None)
    }
    pub fn update(&self, id: &str, f: impl FnOnce(&mut Execution)) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.failed {
            return Err(std::io::Error::other("control store is unavailable"));
        }
        let mut record = inner
            .records
            .get(id)
            .cloned()
            .ok_or_else(|| std::io::Error::other("execution missing"))?;
        f(&mut record);
        Self::append(&mut inner, record)
    }
    fn append(inner: &mut Inner, record: Execution) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        let result = crate::control_db::put(
            &inner.database,
            "execution",
            &record.response_id,
            &serde_json::to_value(&record)?,
        )
        .and_then(|()| inner.file.write_all(&bytes))
        .and_then(|()| inner.file.sync_all());
        if let Err(e) = result {
            inner.failed = true;
            return Err(e);
        }
        inner.records.insert(record.response_id.clone(), record);
        Ok(())
    }
    pub fn persisted_mappings(
        &self,
    ) -> std::io::Result<Option<Vec<crate::store::ResponseMapping>>> {
        let inner = self.inner.lock().unwrap();
        Ok(Some(
            crate::control_db::read(&inner.database, "mapping")?
                .into_iter()
                .map(|v| serde_json::from_value(v).map_err(std::io::Error::other))
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }
    pub fn persist_mapping(
        &self,
        mapping: &crate::store::ResponseMapping,
    ) -> std::io::Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        if inner.failed {
            return Err(std::io::Error::other("control store is unavailable"));
        }
        if let Err(e) = crate::control_db::put(
            &inner.database,
            "mapping",
            &mapping.response_id,
            &serde_json::to_value(mapping)?,
        ) {
            inner.failed = true;
            return Err(e);
        }
        Ok(true)
    }
    pub fn observe(&self, turn: &str, status: &str) -> std::io::Result<()> {
        if let Some(r) = self.by_turn(turn)? {
            self.update(&r.response_id, |r| {
                r.last_observed_status = status.into();
                r.last_observed_at_ms = crate::journal::now_ms();
                if terminal(status) {
                    r.phase = "finished".into();
                }
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("control-store-{}", uuid::Uuid::new_v4()))
    }
    #[test]
    fn dispatch_boundary_reopens_unknown_and_received_is_not_sent() {
        let root = root();
        let store = ControlStore::open(&root).unwrap();
        for id in ["received", "dispatching"] {
            store
                .reserve(Execution::new(
                    id.into(),
                    Some(id.into()),
                    Some("hash".into()),
                ))
                .unwrap();
        }
        store
            .update("dispatching", |r| r.phase = "dispatching".into())
            .unwrap();
        drop(store);
        let store = ControlStore::open(&root).unwrap();
        assert_eq!(
            store.by_request("received").unwrap().unwrap().phase,
            "received"
        );
        assert_eq!(
            store.by_request("dispatching").unwrap().unwrap().phase,
            "unknown"
        );
        let duplicate = store
            .reserve(Execution::new(
                "new".into(),
                Some("dispatching".into()),
                Some("hash".into()),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(duplicate.response_id, "dispatching");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn failed_sync_write_disables_reads_and_further_dispatch() {
        let root = root();
        let store = ControlStore::open(&root).unwrap();
        store
            .reserve(Execution::new("first".into(), Some("key".into()), None))
            .unwrap();
        store.inner.lock().unwrap().file =
            OpenOptions::new().write(true).open("/dev/full").unwrap();
        assert!(
            store
                .update("first", |r| r.phase = "dispatching".into())
                .is_err()
        );
        assert!(store.by_request("key").is_err());
        assert!(
            store
                .reserve(Execution::new("second".into(), None, None))
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sqlite_state_survives_corrupt_legacy_mirror_and_fingerprints_are_canonical() {
        let root = root();
        let store = ControlStore::open(&root).unwrap();
        store
            .reserve(Execution::new("one".into(), None, None))
            .unwrap();
        drop(store);
        OpenOptions::new()
            .append(true)
            .open(root.join("executions.jsonl"))
            .unwrap()
            .write_all(b"{broken")
            .unwrap();
        let restored = ControlStore::open(&root).unwrap();
        assert_eq!(restored.get("one").unwrap().unwrap().response_id, "one");
        let a: Value = serde_json::from_str(r#"{"b":2,"a":{"z":3,"y":1}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"y":1,"z":3},"b":2}"#).unwrap();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        std::fs::remove_dir_all(root).unwrap();
    }
}
