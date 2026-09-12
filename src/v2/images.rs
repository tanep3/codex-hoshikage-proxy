//! Durable image inventory, sourced exclusively from the exact App Server Turn.
//! Never infer ownership from prose, filesystem timestamps, or adjacent Turns.
use super::{
    Error, Result,
    limits::Limits,
    now,
    service::{Service, string},
};
use crate::http::AppState;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};

pub fn initial(limits: &Limits) -> Value {
    json!({"revision":1,"state":"pending","items":[],"error":null,
        "expires_at_ms":null,"settle_until_ms":null,"next_attempt_at_ms":0,
        "policy":{"max_count":limits.generated_images_max_count,
        "max_bytes":limits.generated_image_max_bytes,
        "settle_seconds":limits.generated_images_settle_seconds,
        "retention_seconds":limits.artifact_retention_seconds}})
}

pub fn read(s: &Service, rid: &str) -> Result<Value> {
    let r = s.store.get("response", rid)?;
    s.workspace_path(string(&r, "conversation_id")?)?;
    let mut m = r["generated_images"].clone();
    if !m.is_object() {
        return Err(Error::code(409, "generated_images_not_tracked"));
    }
    if m["expires_at_ms"].as_u64().is_some_and(|t| t <= now()) {
        return Err(Error::code(410, "generated_images_expired"));
    }
    for key in ["policy", "next_attempt_at_ms", "settle_until_ms"] {
        m.as_object_mut().unwrap().remove(key);
    }
    for key in ["response_id", "conversation_id", "workspace_id"] {
        m[key] = r[key].clone();
    }
    Ok(m)
}

fn update(s: &Service, rid: &str, f: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
    s.store.update("response", rid, |r| {
        let m = &mut r["generated_images"];
        let before = m.clone();
        f(m)?;
        if *m != before {
            m["revision"] = json!(before["revision"].as_u64().unwrap_or(0) + 1);
        }
        Ok(())
    })?;
    Ok(())
}

fn uncertain(s: &Service, rid: &str, code: &'static str) -> Result<()> {
    update(s, rid, |m| {
        if m["expires_at_ms"].is_null() {
            m["expires_at_ms"] =
                json!(now() + m["policy"]["retention_seconds"].as_u64().unwrap_or(604800) * 1000);
            m["settle_until_ms"] =
                json!(now() + m["policy"]["settle_seconds"].as_u64().unwrap_or(600) * 1000);
        }
        let transient = matches!(
            code,
            "image_inventory_unavailable" | "image_turn_unresolved" | "image_turn_not_terminal"
        );
        m["state"] = json!(if transient
            && m["state"] != "unknown"
            && m["settle_until_ms"].as_u64().unwrap_or(0) > now()
        {
            "pending"
        } else {
            "unknown"
        });
        m["error"] = json!({"code":code});
        m["next_attempt_at_ms"] = json!(now() + 60_000);
        Ok(())
    })
}

struct Guard {
    s: Arc<Service>,
    rid: String,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.s.image_workers.lock().unwrap().remove(&self.rid);
    }
}

/// Called by maintenance; no HTTP client connection owns this work.
pub fn schedule(state: AppState, s: Arc<Service>, r: &Value) {
    let Some(rid) = r["response_id"].as_str() else {
        return;
    };
    let m = &r["generated_images"];
    if !m.is_object()
        || m["state"] == "complete"
        || m["expires_at_ms"].as_u64().is_some_and(|t| t <= now())
        || m["next_attempt_at_ms"].as_u64().unwrap_or(0) > now()
        || !matches!(
            r["phase"].as_str(),
            Some("finished" | "unknown" | "cancelled" | "rejected")
        )
    {
        return;
    }
    // Start the finite clock even when all copy slots are busy.
    if m["expires_at_ms"].is_null()
        && update(&s, rid, |m| {
            m["expires_at_ms"] =
                json!(now() + m["policy"]["retention_seconds"].as_u64().unwrap_or(604800) * 1000);
            m["settle_until_ms"] =
                json!(now() + m["policy"]["settle_seconds"].as_u64().unwrap_or(600) * 1000);
            Ok(())
        })
        .is_err()
    {
        return;
    }
    if m["state"] == "pending"
        && m["settle_until_ms"].as_u64().is_some_and(|t| t <= now())
        && uncertain(&s, rid, "image_settle_timeout").is_err()
    {
        return;
    }
    let Ok(permit) = s.copies.clone().try_acquire_owned() else {
        return;
    };
    {
        let mut workers = s.image_workers.lock().unwrap();
        if !workers.insert(rid.to_owned()) {
            return;
        }
    }
    let rid = rid.to_owned();
    tokio::spawn(async move {
        let _guard = Guard {
            s: s.clone(),
            rid: rid.clone(),
        };
        let _permit = permit;
        if let Err(e) = refresh(&state, &s, &rid).await
            && uncertain(&s, &rid, e.code).is_err()
        {
            tracing::error!("generated image status persistence failed");
        }
    });
}

async fn refresh(state: &AppState, s: &Arc<Service>, rid: &str) -> Result<()> {
    if s.store.metadata("recovery_state")? != "ready" {
        return Err(Error::code(503, "recovery_blocked"));
    }
    let r = s.store.get("response", rid)?;
    state
        .cwd_policy
        .validate(s.workspace_path(string(&r, "conversation_id")?)?)
        .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
    update(s, rid, |m| {
        if m["expires_at_ms"].is_null() {
            m["expires_at_ms"] =
                json!(now() + m["policy"]["retention_seconds"].as_u64().unwrap_or(604800) * 1000);
            m["settle_until_ms"] =
                json!(now() + m["policy"]["settle_seconds"].as_u64().unwrap_or(600) * 1000);
        }
        m["next_attempt_at_ms"] = json!(now() + 5_000);
        Ok(())
    })?;
    if matches!(r["phase"].as_str(), Some("cancelled" | "rejected"))
        && r["execution_status"] == "not_started"
    {
        return update(s, rid, |m| {
            m["state"] = json!("complete");
            m["error"] = Value::Null;
            Ok(())
        });
    }
    let (Some(thread), Some(turn)) = (r["thread_id"].as_str(), r["turn_id"].as_str()) else {
        return Err(Error::code(409, "image_turn_unresolved"));
    };
    let value = tokio::time::timeout(
        Duration::from_secs(30),
        state.runtime.request(
            "thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        ),
    )
    .await
    .map_err(|_| Error::code(504, "image_inventory_unavailable"))?
    .map_err(|_| Error::code(502, "image_inventory_unavailable"))?;
    if value.pointer("/thread/id").and_then(Value::as_str) != Some(thread) {
        return Err(Error::code(409, "image_turn_mismatch"));
    }
    let found = value
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .and_then(|ts| ts.iter().find(|t| t["id"] == turn))
        .ok_or(Error::code(409, "image_turn_unresolved"))?;
    if !matches!(
        found["status"].as_str(),
        Some("completed" | "failed" | "interrupted")
    ) {
        return Err(Error::code(409, "image_turn_not_terminal"));
    }
    let found = found.clone();
    let svc = s.clone();
    let id = rid.to_owned();
    tokio::task::spawn_blocking(move || ingest(&svc, &id, &found))
        .await
        .map_err(|_| Error::code(503, "image_capture_unknown"))?
}

/// Ingest an authoritative, complete terminal Turn snapshot (not an event fragment).
/// Calls are serialized per response by schedule; also useful for offline acceptance.
pub fn ingest(s: &Service, rid: &str, turn: &Value) -> Result<()> {
    let r = s.store.get("response", rid)?;
    if turn["id"] != r["turn_id"]
        || !r["turn_id"].is_string()
        || !matches!(
            turn["status"].as_str(),
            Some("completed" | "failed" | "interrupted")
        )
    {
        return Err(Error::code(409, "image_turn_mismatch"));
    }
    if r["generated_images"]["state"] == "complete" {
        return Ok(());
    }
    s.workspace_path(string(&r, "conversation_id")?)?;
    if turn["itemsView"] != "full" {
        return Err(Error::code(502, "image_inventory_incomplete"));
    }
    let items = turn["items"]
        .as_array()
        .ok_or(Error::code(502, "image_inventory_unavailable"))?;
    let images: Vec<_> = items
        .iter()
        .filter(|i| i["type"] == "imageGeneration")
        .collect();
    let max_count = r["generated_images"]["policy"]["max_count"]
        .as_u64()
        .unwrap_or(16) as usize;
    if images.len() > max_count {
        return uncertain(s, rid, "generated_images_limit_exceeded");
    }
    let mut seen = HashSet::new();
    for item in &images {
        let id = string(item, "id")?;
        if id.is_empty() || id.len() > 256 || !seen.insert(id) {
            return uncertain(s, rid, "image_identity_invalid");
        }
    }
    update(s, rid, |m| {
        if m["expires_at_ms"].is_null() {
            m["expires_at_ms"] =
                json!(now() + m["policy"]["retention_seconds"].as_u64().unwrap_or(604800) * 1000);
        }
        Ok(())
    })?;
    // Persist identities before copying; neither bytes nor local paths enter the inventory.
    update(s, rid, |m| {
        let old = m["items"]
            .as_array()
            .ok_or(Error::code(503, "store_corrupt"))?;
        let mut inventory = Vec::new();
        for (ordinal, item) in images.iter().enumerate() {
            let image_id = image_id(rid, string(item, "id")?);
            inventory.push(old.iter().find(|v|v["image_id"]==image_id).cloned().unwrap_or_else(||
                json!({"image_id":image_id,"ordinal":ordinal,"state":"creating","artifact_id":null,"error":null})));
        }
        if old
            .iter()
            .any(|v| !inventory.iter().any(|i| i["image_id"] == v["image_id"]))
        {
            return Err(Error::code(409, "image_inventory_changed"));
        }
        m["items"] = json!(inventory);
        Ok(())
    })?;
    let started = std::time::Instant::now();
    for item in images {
        if started.elapsed().as_secs() >= s.limits.generated_images_settle_seconds {
            return uncertain(s, rid, "image_capture_timeout");
        }
        let iid = image_id(rid, string(item, "id")?);
        let m = s.store.get("response", rid)?["generated_images"].clone();
        let entry = m["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["image_id"] == iid)
            .unwrap();
        if matches!(
            entry["state"].as_str(),
            Some("ready" | "failed" | "unknown")
        ) {
            continue;
        }
        let result = capture(s, &r, &iid, item, entry["ordinal"].as_u64().unwrap_or(0));
        update(s, rid, |m| {
            let entry = m["items"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|i| i["image_id"] == iid)
                .unwrap();
            match result {
                Ok((aid, state, error)) => {
                    entry["artifact_id"] = json!(aid);
                    entry["state"] = json!(state);
                    entry["error"] = error;
                }
                Err(e) => {
                    entry["state"] = json!(if e.status >= 500 { "unknown" } else { "failed" });
                    entry["error"] = json!({"code":e.code});
                }
            }
            Ok(())
        })?;
    }
    update(s, rid, |m| {
        m["state"] = json!("complete");
        m["error"] = Value::Null;
        Ok(())
    })
}
fn image_id(rid: &str, item: &str) -> String {
    format!("img_{}", crate::control::fingerprint(&json!([rid, item])))
}
fn capture(
    s: &Service,
    r: &Value,
    iid: &str,
    item: &Value,
    ordinal: u64,
) -> Result<(String, &'static str, Value)> {
    if item["status"] != "completed" {
        return Err(
            if matches!(item["status"].as_str(), Some("failed" | "cancelled")) {
                Error::code(422, "image_generation_failed")
            } else {
                Error::code(502, "image_generation_unknown")
            },
        );
    }
    let encoded = item["result"]
        .as_str()
        .ok_or(Error::code(422, "image_result_missing"))?;
    let limit = r["generated_images"]["policy"]["max_bytes"]
        .as_u64()
        .unwrap_or(10485760)
        .min(s.limits.artifact_max_bytes);
    if encoded.len() as u64 > limit.div_ceil(3) * 4 {
        return Err(Error::code(413, "generated_image_too_large"));
    }
    let data = STANDARD
        .decode(encoded)
        .map_err(|_| Error::code(415, "invalid_generated_image"))?;
    if data.len() as u64 > limit {
        return Err(Error::code(413, "generated_image_too_large"));
    }
    // PNG signature + mandatory IHDR, dimensions must be nonzero. Decoding/rendering is a client concern.
    if data.len() < 33
        || !data.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")
        || data[16..20] == [0; 4]
        || data[20..24] == [0; 4]
    {
        return Err(Error::code(415, "invalid_generated_image"));
    }
    let key = format!("generated-{iid}");
    let (op,fresh) = s.reserve_capture(string(r,"conversation_id")?, &key,
        &json!({"path":format!("generated/{iid}.png"),"display_name":format!("generated-image-{}.png",ordinal+1),"response_id":r["response_id"]}))?;
    let aid = string(&op["resource"], "id")?.to_owned();
    let op = if fresh {
        s.finish_capture_data(&aid, &key, Some(&data))?
    } else {
        op
    };
    // Never re-copy an already reserved/unknown artifact, even when the source is still available.
    let a = s.store.get("artifact", &aid)?;
    if matches!(a["state"].as_str(), Some("ready" | "expired" | "corrupt")) {
        use sha2::{Digest, Sha256};
        if a["sha256"] != format!("{:x}", Sha256::digest(&data))
            || a["size_bytes"] != data.len() as u64
        {
            return Err(Error::code(409, "image_capture_identity_mismatch"));
        }
    }
    let state = match a["state"].as_str() {
        Some("ready" | "expired" | "corrupt") => "ready",
        Some("failed") => "failed",
        _ => "unknown",
    };
    Ok((
        aid,
        state,
        if state == "ready" {
            Value::Null
        } else {
            op.get("error")
                .cloned()
                .unwrap_or(json!({"code":"image_capture_unknown"}))
        },
    ))
}

/// Repair inventory receipts only from already durable artifacts; never read the original image.
pub fn recover(s: &Service) -> Result<()> {
    for r in s.store.list("response")? {
        let Some(items) = r["generated_images"]["items"].as_array() else {
            continue;
        };
        let rid = string(&r, "response_id")?;
        for item in items {
            if !matches!(
                item["state"].as_str(),
                Some("creating" | "unknown" | "failed")
            ) {
                continue;
            }
            let iid = string(item, "image_id")?;
            let op = s
                .store
                .transaction(|tx| super::store::operation(tx, &format!("generated-{iid}")))?;
            let Some(aid) = op.as_ref().and_then(|v| v["resource"]["id"].as_str()) else {
                continue;
            };
            let a = s.store.get("artifact", aid)?;
            if a["response_id"] != rid || a["conversation_id"] != r["conversation_id"] {
                return Err(Error::code(503, "store_corrupt"));
            }
            let status = match a["state"].as_str() {
                Some("ready" | "expired" | "corrupt") => "ready",
                Some("failed") => "failed",
                _ => "unknown",
            };
            update(s, rid, |m| {
                let entry = m["items"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|i| i["image_id"] == iid)
                    .unwrap();
                entry["artifact_id"] = json!(aid);
                entry["state"] = json!(status);
                entry["error"] = if status == "ready" {
                    Value::Null
                } else {
                    json!({"code":"image_capture_unknown"})
                };
                Ok(())
            })?;
        }
    }
    Ok(())
}
