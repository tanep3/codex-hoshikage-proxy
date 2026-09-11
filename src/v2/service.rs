use super::{
    Error, Result, files, id, now,
    store::{self, Store},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Service {
    pub store: Store,
    pub limits: super::limits::Limits,
    pub work_root: PathBuf,
    pub copies: Arc<tokio::sync::Semaphore>,
    pub downloads: Arc<tokio::sync::Semaphore>,
    pub workers: std::sync::Mutex<std::collections::HashSet<String>>,
}
impl Service {
    pub fn open(root: &Path, work_root: &Path) -> Result<Self> {
        Self::open_with_limits(root, work_root, Default::default())
    }
    pub fn open_with_limits(
        root: &Path,
        work_root: &Path,
        limits: super::limits::Limits,
    ) -> Result<Self> {
        limits.validate()?;
        if let Some(parent) = root.parent()
            && parent.join("v2-restore-pending.json").exists()
            && !root.join("metadata.sqlite3").exists()
        {
            return Err(Error::code(503, "recovery_blocked"));
        }
        std::fs::create_dir_all(work_root)?;
        let work_root = std::fs::canonicalize(work_root)?;
        std::fs::create_dir_all(root)?;
        let canonical_store = std::fs::canonicalize(root)?;
        if super::coordination::overlapping(&canonical_store, &work_root) {
            return Err(Error::code(400, "unsafe_store_location"));
        }
        std::fs::set_permissions(&work_root, std::fs::Permissions::from_mode(0o700))?;
        let service = Self {
            store: Store::open(root)?,
            work_root,
            copies: Arc::new(tokio::sync::Semaphore::new(limits.capture_concurrency)),
            downloads: Arc::new(tokio::sync::Semaphore::new(limits.download_concurrency)),
            limits,
            workers: Default::default(),
        };
        if super::coordination::overlapping(service.protected_root(), &service.work_root) {
            return Err(Error::code(400, "unsafe_store_location"));
        }
        for c in service.store.list("conversation")? {
            if c["state"] == "creating" {
                service.finish_conversation(string(&c, "conversation_id")?)?;
            }
        }
        let marker = service
            .store
            .root
            .parent()
            .unwrap()
            .join("v2-restore-pending.json");
        if marker.exists() {
            let m: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
            if service.store.metadata("restore_complete").ok().as_deref()
                != m["restore_id"].as_str()
            {
                return Err(Error::code(503, "restore_incomplete"));
            }
            if service.store.metadata("restore_released").ok().as_deref()
                == m["restore_id"].as_str()
            {
                std::fs::remove_file(&marker)?;
                files::sync_directory(marker.parent().unwrap())?;
            } else {
                service
                    .store
                    .set_metadata("recovery_state", "recovery_blocked")?;
            }
        }
        super::recovery::recover(&service)?;
        Ok(service)
    }
    pub fn conversation(&self, key: &str, body: &Value) -> Result<Value> {
        if !body.is_object() {
            return Err(Error::code(400, "invalid_argument"));
        }
        let op = self.store.transaction(|tx| {
            let mut fingerprint_body = body.clone();
            fingerprint_body
                .as_object_mut()
                .unwrap()
                .remove("_resolved_model");
            let (mut op, fresh) =
                store::reserve(tx, key, "conversation.create", &fingerprint_body)?;
            if !fresh {
                return Ok(op);
            }
            ensure_ready(tx)?;
            let cid = id("conv");
            let workspace = match body["workspace"]["mode"].as_str() {
                Some("automatic") => {
                    let wid = id("ws");
                    json!({
                        "workspace_id":wid,
                        "path":self.work_root.join(&wid),
                        "state":"creating",
                        "mode":"automatic",
                        "display_name":cid
                    })
                }
                Some("shared") => {
                    let w =
                        store::get(tx, "workspace", string(&body["workspace"], "workspace_id")?)?;
                    if w["mode"] != "shared" || w["state"] != "ready" {
                        return Err(Error::code(403, "resource_access_denied"));
                    }
                    w
                }
                _ => return Err(Error::code(400, "invalid_argument")),
            };
            store::put(
                tx,
                "workspace",
                string(&workspace, "workspace_id")?,
                &workspace,
            )?;
            let conv = json!({
                "conversation_id":cid,
                "operation_key":key,
                "workspace_id":workspace["workspace_id"],
                "workspace_mode":workspace["mode"],
                "state":"creating",
                "model":body.get("model").unwrap_or(&body["_resolved_model"]),
                "created_at_ms":now(),
                "thread_id":null,
                "active_response_id":null,
                "last_response_id":null,
                "artifact_registration":if registration_supported(body.get("model").unwrap_or(&body["_resolved_model"]).as_str().unwrap_or("")){ "available"} else{ "unavailable"}
            });
            store::put(tx, "conversation", &cid, &conv)?;
            op["resource"] = json!({ "type":"conversation","id":cid});
            store::save_operation(tx, &op)?;
            Ok(op)
        })?;
        let cid = string(&op["resource"], "id")?;
        if op["state"] == "accepted" {
            self.finish_conversation(cid)?;
        }
        self.store.transaction(|tx| {
            Ok(store::public_operation(
                store::operation(tx, key)?.ok_or_else(|| Error::code(503, "store_corrupt"))?,
            ))
        })
    }
    pub fn finish_conversation(&self, cid: &str) -> Result<()> {
        self.store.transaction(|tx| {
            let mut c = store::get(tx, "conversation", cid)?;
            if c["state"] != "creating" {
                return Ok(());
            }
            let wid = string(&c, "workspace_id")?.to_owned();
            let mut w = store::get(tx, "workspace", &wid)?;
            if w["state"] == "creating" {
                let path = PathBuf::from(string(&w, "path")?);
                let marker = path.join(".hoshikage-workspace");
                if path.exists() {
                    if std::fs::read_to_string(&marker).ok().as_deref() != Some(&wid) {
                        c["state"] = json!("creation_failed");
                        store::put(tx, "conversation", cid, &c)?;
                        let mut op = store::operation(tx, string(&c, "operation_key")?)?
                            .ok_or_else(|| Error::code(503, "store_corrupt"))?;
                        op["state"] = json!("unknown");
                        op["error"] = json!({ "code":"workspace_identity_mismatch"});
                        store::save_operation(tx, &op)?;
                        return Ok(());
                    }
                } else {
                    std::fs::create_dir(&path)?;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
                    std::fs::write(&marker, &wid)?;
                    std::fs::File::open(&marker)?.sync_all()?;
                    files::sync_directory(&path)?;
                    files::sync_directory(&self.work_root)?;
                }
                let m = std::fs::symlink_metadata(&path)?;
                if !m.is_dir() {
                    return Err(Error::code(409, "workspace_identity_mismatch"));
                }
                w["device"] = json!(m.dev());
                w["inode"] = json!(m.ino());
                w["state"] = json!("ready");
                store::put(tx, "workspace", &wid, &w)?;
            }
            c["state"] = json!("ready");
            store::put(tx, "conversation", cid, &c)?;
            let mut op = store::operation(tx, string(&c, "operation_key")?)?
                .ok_or_else(|| Error::code(503, "store_corrupt"))?;
            op["state"] = json!("succeeded");
            store::save_operation(tx, &op)?;
            Ok(())
        })
    }
    pub fn protected_root(&self) -> &Path {
        if self.store.root.file_name().is_some_and(|n| n == "v2")
            && self
                .store
                .root
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "state")
        {
            self.store.root.parent().unwrap().parent().unwrap()
        } else {
            &self.store.root
        }
    }
    pub fn workspace_path(&self, cid: &str) -> Result<PathBuf> {
        let c = self.store.get("conversation", cid)?;
        let w = self.store.get("workspace", string(&c, "workspace_id")?)?;
        if w["state"] != "ready" {
            return Err(Error::code(403, "workspace_access_revoked"));
        }
        let path = PathBuf::from(string(&w, "path")?);
        let m = std::fs::symlink_metadata(&path)?;
        if !m.is_dir()
            || m.dev() != w["device"].as_u64().unwrap_or(0)
            || m.ino() != w["inode"].as_u64().unwrap_or(0)
        {
            return Err(Error::code(409, "workspace_identity_mismatch"));
        }
        Ok(path)
    }
    pub fn accept(&self, cid: &str, key: &str, body: &Value) -> Result<(Value, Option<String>)> {
        self.accept_with_provider_limit(cid, key, body, None)
    }
    pub fn accept_with_provider_limit(
        &self,
        cid: &str,
        key: &str,
        body: &Value,
        provider_limit: Option<usize>,
    ) -> Result<(Value, Option<String>)> {
        self.store.transaction(|tx| {
            let (mut op, fresh) =
                store::reserve(tx, key, "response.create", &json!({ "conversation_id":cid,"request":body}))?;
            if !fresh {
                return Ok((store::public_operation(op), None));
            }
            ensure_ready(tx)?;
            let mut c = store::get(tx, "conversation", cid)?;
            if c["state"] != "ready" {
                return Err(Error::code(409, "conversation_unavailable"));
            }
            let fenced = store::get(tx, "cancel_fence", key).ok();
            if fenced.as_ref().is_some_and(|f| f["conversation_id"] != cid) {
                return Err(Error::code(409, "target_mismatch"));
            }
            let cancelled = fenced.is_some();
            if !cancelled {
                if let Some(limit) = provider_limit {
                    let provider = c["model"].as_str().and_then(|m| m.split('/').next());
                    let active = store::list(tx, "response")?
                        .iter()
                        .filter(|r| {
                            r["hold_state"] == "held"
                                && r["model"].as_str().and_then(|m| m.split('/').next()) == provider
                        })
                        .count();
                    let legacy = store::list(tx, "legacy_hold")?
                        .iter()
                        .filter(|r| r["state"] == "held" && r["provider"].as_str() == provider)
                        .count();
                    if active + legacy >= limit {
                        return Err(Error::code(409, "provider_busy"));
                    }
                }
                let w = store::get(tx, "workspace", string(&c, "workspace_id")?)?;
                super::coordination::check(tx, Path::new(string(&w, "path")?), None)?;
            }
            let input_bytes = serde_json::to_vec(body)?.len() as u64;
            if input_bytes > self.limits.execution_input_max_bytes {
                return Err(Error::code(413, "input_too_large"));
            }
            if !cancelled {
                files::require_free(
                    &self.store.root,
                    input_bytes.saturating_add(self.limits.output_max_bytes),
                    self.limits.disk_free_floor_bytes,
                )?;
                let records = store::list(tx, "response")?;
                let input_used: u64 = records
                    .iter()
                    .filter(|r| !r["input"].is_null())
                    .map(|r| r["input_bytes"].as_u64().unwrap_or(0))
                    .sum();
                let output_used: u64 = records
                    .iter()
                    .map(|r| match r["output"]["state"].as_str() {
                        Some("pending" | "saving") => r["output_policy"]["max_bytes"]
                            .as_u64()
                            .unwrap_or(self.limits.output_max_bytes),
                        Some("ready") => r["output"]["size_bytes"].as_u64().unwrap_or(0),
                        _ => 0,
                    })
                    .sum();
                if input_used.saturating_add(input_bytes)
                    > self.limits.execution_input_store_max_bytes
                    || output_used.saturating_add(self.limits.output_max_bytes)
                        > self.limits.output_store_max_bytes
                {
                    return Err(Error::code(507, "storage_capacity_exceeded"));
                }
            }
            let rid = id("resp");
            let model = body
                .get("model")
                .cloned()
                .unwrap_or_else(|| c["model"].clone());
            if model
                .as_str()
                .and_then(|m| m.split_once('/'))
                .map(|(p, _)| p)
                != c["model"]
                    .as_str()
                    .and_then(|m| m.split_once('/'))
                    .map(|(p, _)| p)
            {
                return Err(Error::code(409, "cross_provider_model_change_unsupported"));
            }
            let record = json!({
                "response_id":rid,
                "conversation_id":cid,
                "workspace_id":c["workspace_id"],
                "request_key":key,
                "operation_id":op["operation_id"],
                "model":model,
                "output_policy":{"max_bytes":self.limits.output_max_bytes,"retention_seconds":self.limits.output_retention_seconds,"lease_max_lifetime_seconds":self.limits.lease_max_lifetime_seconds},
                "phase":if cancelled{ "cancelled"} else{ "accepted"},
                "execution_status":"not_started",
                "last_observed_status":"not_started",
                "input":if cancelled{ Value::Null} else{ body.clone()},
                "input_bytes":input_bytes,
                "input_expires_at_ms":now()+self.limits.unresolved_input_retention_seconds*1000,
                "created_at_ms":now(),
                "stop_requested":cancelled,
                "interrupt_delivery":"not_sent",
                "thread_id":c["thread_id"],
                "turn_id":null,
                "hold_state":if cancelled{ "released"} else{ "held"},
                "hold_revision":1,
                "dispatch_eligible":!cancelled,
                "output":{ "state":if cancelled{ "unavailable"} else{ "pending"} }
            });
            store::put(tx, "response", &rid, &record)?;
            if !cancelled {
                c["active_response_id"] = json!(rid);
            }
            c["last_response_id"] = json!(rid);
            store::put(tx, "conversation", cid, &c)?;
            op["resource"] = json!({ "type":"response","id":rid});
            if cancelled {
                op["state"] = json!("failed");
                op["error"] = json!({ "code":"execution_cancelled"});
            }
            store::save_operation(tx, &op)?;
            Ok((store::public_operation(op), (!cancelled).then_some(rid)))
        })
    }
    pub fn stop(&self, key: &str, body: &Value) -> Result<Value> {
        self.store.transaction(|tx| {
            let (mut op, fresh) = store::reserve(tx, key, "response.stop", body)?;
            if !fresh {
                return store::get(tx, "stop", string(&op["resource"], "id")?);
            }
            let target = &body["target"];
            let mut record = if let Some(rid) = target["response_id"].as_str() {
                Some(store::get(tx, "response", rid)?)
            } else {
                let cid = string(target, "conversation_id")?;
                store::get(tx, "conversation", cid)?;
                let runkey = string(target, "request_key")?;
                super::api::valid_key(runkey)?;
                if let Some(run) = store::operation(tx, runkey)? {
                    if run["kind"] != "response.create" {
                        return Err(Error::code(409, "target_mismatch"));
                    }
                    let r = store::get(tx, "response", string(&run["resource"], "id")?)?;
                    if r["conversation_id"] != cid {
                        return Err(Error::code(409, "target_mismatch"));
                    }
                    Some(r)
                } else {
                    if let Ok(f) = store::get(tx, "cancel_fence", runkey)
                        && f["conversation_id"] != cid
                    {
                        return Err(Error::code(409, "target_mismatch"));
                    }
                    store::put(tx, "cancel_fence", runkey, &json!({ "conversation_id":cid}))?;
                    None
                }
            };
            if let Some(r) = record.as_mut() {
                r["stop_requested"] = json!(true);
                if r["phase"] == "accepted" {
                    r["phase"] = json!("cancelled");
                    r["hold_state"] = json!("released");
                    r["dispatch_eligible"] = json!(false);
                    r["input"] = Value::Null;
                    r["output"] = json!({ "state":"unavailable"});
                    if let Some(mut runop) = store::operation(tx, string(r, "request_key")?)? {
                        runop["state"] = json!("failed");
                        runop["error"] = json!({ "code":"execution_cancelled"});
                        store::save_operation(tx, &runop)?;
                    }
                }
                store::put(tx, "response", string(r, "response_id")?, r)?;
            }
            let sid = id("stop");
            let v = json!({
                "stop_id":sid,
                "operation_id":op["operation_id"],
                "response_id":record.as_ref().map(|r|r["response_id"].clone()),
                "intent_state":"recorded",
                "stop_status":record.as_ref().map(stop_status).unwrap_or("cancelled_before_start"),
                "execution_status":record.as_ref().map(|r|r["execution_status"].clone()).unwrap_or(json!("not_started"))
            });
            store::put(tx, "stop", &sid, &v)?;
            op["state"] = json!("succeeded");
            op["resource"] = json!({ "type":"stop","id":sid});
            store::save_operation(tx, &op)?;
            Ok(v)
        })
    }
    pub fn capture(&self, cid: &str, key: &str, body: &Value) -> Result<Value> {
        let (op, fresh) = self.reserve_capture(cid, key, body)?;
        if fresh {
            self.finish_capture(string(&op["resource"], "id")?, key)
        } else {
            Ok(op)
        }
    }
    pub fn reserve_capture(&self, cid: &str, key: &str, body: &Value) -> Result<(Value, bool)> {
        self.workspace_path(cid)?;
        if body.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !["path", "display_name", "response_id"].contains(&k.as_str()))
        }) {
            return Err(Error::code(400, "invalid_argument"));
        }
        let (op, aid, fresh) = self.store.transaction(|tx| {
            let (mut op, fresh) = store::reserve(
                tx,
                key,
                "artifact.create",
                &json!({ "conversation_id":cid,"request":body}),
            )?;
            if !fresh {
                return Ok((op.clone(), string(&op["resource"], "id")?.to_owned(), false));
            }
            ensure_ready(tx)?;
            let c = store::get(tx, "conversation", cid)?;
            if let Some(rid) = body["response_id"].as_str() {
                let r = store::get(tx, "response", rid)?;
                if r["conversation_id"] != cid {
                    return Err(Error::code(409, "target_mismatch"));
                }
            }
            files::require_free(
                &self.store.root,
                self.limits.artifact_max_bytes,
                self.limits.disk_free_floor_bytes,
            )?;
            let path = string(body, "path")?;
            let name = body["display_name"]
                .as_str()
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path));
            if name.is_empty()
                || name
                    .chars()
                    .any(|c| c.is_control() || c == '/' || c == '\\')
            {
                return Err(Error::code(400, "invalid_argument"));
            }
            let all = store::list(tx, "artifact")?;
            let used: u64 = all
                .iter()
                .filter(|a| a["state"] == "ready" || a["state"] == "creating")
                .map(|a| {
                    if a["state"] == "creating" {
                        self.limits.artifact_max_bytes
                    } else {
                        a["size_bytes"].as_u64().unwrap_or(0)
                    }
                })
                .sum();
            if used + self.limits.artifact_max_bytes > self.limits.artifact_store_max_bytes {
                return Err(Error::code(507, "storage_capacity_exceeded"));
            }
            let version = 1 + all
                .iter()
                .filter(|a| a["conversation_id"] == cid && a["source_path"] == path)
                .filter_map(|a| a["version"].as_u64())
                .max()
                .unwrap_or(0);
            let aid = id("art");
            let a = json!({
                "artifact_id":aid,
                "conversation_id":cid,
                "workspace_id":c["workspace_id"],
                "response_id":body["response_id"],
                "source_path":path,
                "display_name":name,
                "version":version,
                "state":"creating",
                "created_at_ms":now(),
                "expires_at_ms":now()+self.limits.artifact_retention_seconds*1000,
                "media_type":"application/octet-stream"
            });
            store::put(tx, "artifact", &aid, &a)?;
            op["resource"] = json!({ "type":"artifact","id":aid});
            op["state"] = json!("running");
            store::save_operation(tx, &op)?;
            Ok((op, aid, true))
        })?;
        let _ = aid;
        Ok((store::public_operation(op), fresh))
    }
    pub fn finish_capture(&self, aid: &str, key: &str) -> Result<Value> {
        let a = self.store.get("artifact", aid)?;
        let cid = string(&a, "conversation_id")?;
        let op = self.store.transaction(|tx| {
            store::operation(tx, key)?.ok_or_else(|| Error::code(503, "store_corrupt"))
        })?;
        if a["state"] != "creating" {
            return Ok(store::public_operation(op));
        }
        let outcome = (|| {
            let root = self.workspace_path(cid)?;
            let c = self.store.get("conversation", cid)?;
            let w = self.store.get("workspace", string(&c, "workspace_id")?)?;
            let mut source = files::open_source_at(
                &root,
                string(&a, "source_path")?,
                (
                    w["device"]
                        .as_u64()
                        .ok_or_else(|| Error::code(503, "store_corrupt"))?,
                    w["inode"]
                        .as_u64()
                        .ok_or_else(|| Error::code(503, "store_corrupt"))?,
                ),
            )?;
            let staging = self.store.root.join("staging").join(aid);
            let (size, hash) = files::copy_with_timeout(
                &mut source,
                &staging,
                self.limits.artifact_max_bytes,
                self.limits.capture_timeout_seconds,
            )?;
            let metadata = json!({
                "state":"ready",
                "ready_at_ms":now(),
                "expires_at_ms":now()+self.limits.artifact_retention_seconds*1000,
                "max_hold_until_ms":now()+self.limits.lease_max_lifetime_seconds*1000,
                "size_bytes":size,
                "sha256":hash
            });
            self.workspace_path(cid)?;
            files::publish(&staging, &self.store.root.join("blobs"), aid, &metadata)?;
            Ok((size, hash))
        })();
        self.store.transaction(|tx| {
            let mut op = op;
            let mut a = store::get(tx, "artifact", aid)?;
            match outcome {
                Ok((size, hash)) => {
                    a["state"] = json!("ready");
                    a["ready_at_ms"] = json!(now());
                    a["max_hold_until_ms"] =
                        json!(now() + self.limits.lease_max_lifetime_seconds * 1000);
                    a["expires_at_ms"] =
                        json!(now() + self.limits.artifact_retention_seconds * 1000);
                    a["size_bytes"] = json!(size);
                    a["sha256"] = json!(hash);
                    op["state"] = json!("succeeded");
                }
                Err(e) => {
                    let e: Error = e;
                    a["state"] = json!("failed");
                    op["state"] = json!("failed");
                    op["error"] = json!({ "code":e.code});
                }
            }
            store::put(tx, "artifact", aid, &a)?;
            store::save_operation(tx, &op)?;
            Ok(store::public_operation(op))
        })
    }
}
pub fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| Error::code(400, "invalid_argument"))
}
pub fn stop_status(r: &Value) -> &'static str {
    match r["phase"].as_str() {
        Some("cancelled") => "cancelled_before_start",
        Some("finished" | "rejected") => {
            if r["execution_status"] == "interrupted" {
                "interrupted"
            } else {
                "already_terminal"
            }
        }
        Some("unknown") => "unknown",
        Some("started") => "interrupt_pending",
        _ => "waiting_for_start",
    }
}

pub(crate) fn ensure_ready(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let state: String = tx.query_row(
        "SELECT value FROM metadata WHERE key='recovery_state'",
        [],
        |r| r.get(0),
    )?;
    if state != "ready" {
        return Err(Error::code(503, "recovery_blocked"));
    }
    Ok(())
}

impl Service {
    pub fn capacity(&self) -> Result<Value> {
        self.store.transaction(|tx| {
            let artifacts = store::list(tx, "artifact")?;
            let responses = store::list(tx, "response")?;
            let used = artifacts
                .iter()
                .filter(|r| r["state"] == "ready")
                .fold(0u64, |a, r| {
                    a.saturating_add(r["size_bytes"].as_u64().unwrap_or(0))
                });
            let reserved = (artifacts
                .iter()
                .filter(|r| r["state"] == "creating")
                .count() as u64)
                .saturating_mul(self.limits.artifact_max_bytes);
            let output_used = responses
                .iter()
                .filter(|r| r["output"]["state"] == "ready")
                .fold(0u64, |a, r| {
                    a.saturating_add(r["output"]["size_bytes"].as_u64().unwrap_or(0))
                });
            let output_reserved = responses
                .iter()
                .filter(|r| matches!(r["output"]["state"].as_str(), Some("pending" | "saving")))
                .fold(0u64, |sum, r| {
                    sum.saturating_add(
                        r["output_policy"]["max_bytes"]
                            .as_u64()
                            .unwrap_or(self.limits.output_max_bytes),
                    )
                });
            let inputs = responses
                .iter()
                .filter(|r| !r["input"].is_null())
                .fold(0u64, |a, r| {
                    a.saturating_add(r["input_bytes"].as_u64().unwrap_or(0))
                });
            let ready = ensure_ready(tx).is_ok();
            Ok(json!({
                "artifacts":{ "used_bytes":used,"reserved_bytes":reserved,"available_bytes":self.limits.artifact_store_max_bytes.saturating_sub(used.saturating_add(reserved))},
                "outputs":{ "used_bytes":output_used,"reserved_bytes":output_reserved,"available_bytes":self.limits.output_store_max_bytes.saturating_sub(output_used.saturating_add(output_reserved))},
                "inputs":{ "used_bytes":inputs,"available_bytes":self.limits.execution_input_store_max_bytes.saturating_sub(inputs)},
                "accepting":ready && files::require_free(&self.store.root,self.limits.output_max_bytes,self.limits.disk_free_floor_bytes).is_ok()
            }))
        })
    }
}

pub fn registration_supported(model: &str) -> bool {
    matches!(model, "chatgpt/gpt-5.6-luna" | "chatgpt/gpt-5.6-terra")
}
