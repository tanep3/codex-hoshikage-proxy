use super::{Error, Result, service::Service, store};
use serde_json::{Value, json};
use std::path::Path;
pub fn overlapping(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
pub fn check(tx: &rusqlite::Transaction<'_>, path: &Path, exclude: Option<&str>) -> Result<()> {
    for r in store::list(tx, "response")? {
        if r["hold_state"] == "held" && r["response_id"].as_str() != exclude {
            let w = store::get(
                tx,
                "workspace",
                r["workspace_id"]
                    .as_str()
                    .ok_or_else(|| Error::code(503, "store_corrupt"))?,
            )?;
            if overlapping(
                path,
                Path::new(
                    w["path"]
                        .as_str()
                        .ok_or_else(|| Error::code(503, "store_corrupt"))?,
                ),
            ) {
                return Err(Error::code(409, "workspace_busy"));
            }
        }
    }
    for r in store::list(tx, "legacy_hold")? {
        if r["state"] == "held"
            && r["response_id"].as_str() != exclude
            && overlapping(
                path,
                Path::new(
                    r["path"]
                        .as_str()
                        .ok_or_else(|| Error::code(503, "store_corrupt"))?,
                ),
            )
        {
            return Err(Error::code(409, "workspace_busy"));
        }
    }
    Ok(())
}
pub struct LegacyReservation {
    service: std::sync::Arc<Service>,
    rid: String,
    dispatched: bool,
}
impl LegacyReservation {
    pub fn dispatched(&mut self) {
        self.dispatched = true;
    }
}
impl Drop for LegacyReservation {
    fn drop(&mut self) {
        if !self.dispatched {
            let _ = self.service.store.update("legacy_hold", &self.rid, |r| {
                r["state"] = json!("released");
                Ok(())
            });
        }
    }
}
pub fn reserve_legacy(
    s: std::sync::Arc<Service>,
    path: &Path,
    rid: &str,
    provider: &str,
    limit: usize,
) -> Result<LegacyReservation> {
    s.store.transaction(|tx| {
        super::service::ensure_ready(tx)?;
        if overlapping(path, s.protected_root()) {
            return Err(Error::code(403, "protected_workspace"));
        }
        check(tx, path, Some(rid))?;
        let occupied = store::list(tx, "response")?
            .iter()
            .filter(|r| {
                r["hold_state"] == "held"
                    && r["model"]
                        .as_str()
                        .is_some_and(|m| m.split('/').next() == Some(provider))
            })
            .count();
        let legacy = store::list(tx, "legacy_hold")?
            .iter()
            .filter(|r| r["state"] == "held" && r["provider"] == provider)
            .count();
        if occupied + legacy >= limit {
            return Err(Error::code(409, "provider_busy"));
        }
        store::put(
            tx,
            "legacy_hold",
            rid,
            &json!({
                "response_id":rid,
                "path":path,
                "state":"held",
                "provider":provider
            }),
        )
    })?;
    Ok(LegacyReservation {
        service: s,
        rid: rid.into(),
        dispatched: false,
    })
}
pub async fn reconcile(state: &crate::http::AppState, s: &Service) -> Result<()> {
    for mut r in s.store.list("response")? {
        if matches!(r["phase"].as_str(), Some("dispatching" | "started"))
            && let Some(rid) = r["response_id"].as_str()
            && !s.workers.lock().unwrap().contains(rid)
        {
            r = s.store.update("response", rid, |r| {
                if !matches!(r["phase"].as_str(), Some("dispatching" | "started")) {
                    return Ok(());
                }
                r["phase"] = json!("unknown");
                r["execution_status"] = json!("unknown");
                r["stop_requested"] = json!(true);
                r["hold_revision"] = json!(r["hold_revision"].as_u64().unwrap_or(0) + 1);
                Ok(())
            })?;
        }
        if r["phase"] != "unknown" && r["hold_state"] != "administratively_released" {
            continue;
        }
        let (Some(thread), Some(turn), Some(rid)) = (
            r["thread_id"].as_str(),
            r["turn_id"].as_str(),
            r["response_id"].as_str(),
        ) else {
            continue;
        };
        let Ok(value) = state
            .runtime
            .request(
                "thread/read",
                json!({ "threadId":thread,"includeTurns":true}),
            )
            .await
        else {
            continue;
        };
        let Some(found) = value
            .pointer("/thread/turns")
            .and_then(Value::as_array)
            .and_then(|ts| ts.iter().find(|t| t["id"] == turn))
        else {
            continue;
        };
        let status = found["status"].as_str().unwrap_or("unknown");
        if status == "unknown" {
            continue;
        }
        let refreshed = s.store.update("response", rid, |current| {
            if !matches!(current["phase"].as_str(), Some("unknown"))
                && current["hold_state"] != "administratively_released"
            {
                return Ok(());
            }
            current["last_observed_status"] = json!(status);
            current["hold_revision"] = json!(current["hold_revision"].as_u64().unwrap_or(0) + 1);
            if matches!(status, "completed" | "failed" | "interrupted") {
                current["execution_status"] = json!(status);
                current["phase"] = json!("finished");
                current["hold_state"] = json!("released");
                if current["output"]["state"] == "pending" {
                    current["output"] = json!({ "state":"unavailable"});
                }
            } else {
                current["execution_status"] = json!("in_progress");
                current["hold_state"] = json!("held");
            }
            Ok(())
        })?;
        if matches!(status, "completed" | "failed" | "interrupted") {
            let cid = super::service::string(&refreshed, "conversation_id")?;
            s.store.update("conversation", cid, |c| {
                if c["active_response_id"] == rid {
                    c["active_response_id"] = Value::Null;
                }
                Ok(())
            })?;
            if status == "completed" && refreshed["output"]["state"] == "unavailable" {
                // Only the exact recorded Turn can repair a missing final output.
                if let Some(items) = found["items"].as_array() {
                    let messages: Vec<_> = items
                        .iter()
                        .filter(|i| i["type"] == "agentMessage" && i["phase"] != "commentary")
                        .filter_map(|i| i["text"].as_str())
                        .collect();
                    if !messages.is_empty() {
                        s.store.update("response", rid, |r| {
                            r["output"] = json!({ "state":"saving"});
                            Ok(())
                        })?;
                        let content = json!({
                            "response_id":rid,
                            "model":r["model"],
                            "output":[{ "type":"message","role":"assistant","content":[{ "type":"output_text","text":messages.join("\n")} ]} ]
                        });
                        if super::engine::save_output(s, rid, &content).is_err() {
                            s.store.update("response", rid, |r| {
                                r["output"] = json!({ "state":"failed"});
                                Ok(())
                            })?;
                        }
                    }
                }
            }
        }
        if status == "inProgress"
            && refreshed["stop_requested"] == true
            && refreshed["interrupt_delivery"] == "not_sent"
        {
            s.store.update("response", rid, |r| {
                r["interrupt_delivery"] = json!("dispatching");
                Ok(())
            })?;
            let outcome = crate::http::request_interrupt(&state.runtime, thread, turn).await;
            s.store.update("response", rid, |r| {
                r["interrupt_delivery"] = json!(if outcome.is_ok() {
                    "accepted"
                } else {
                    "unknown"
                });
                Ok(())
            })?;
        }
    }
    for h in s.store.list("legacy_hold")? {
        if !matches!(
            h["state"].as_str(),
            Some("held" | "administratively_released")
        ) {
            continue;
        }
        let rid = super::service::string(&h, "response_id")?;
        let Some(r) = state
            .responses
            .control
            .get(rid)
            .map_err(|_| Error::code(503, "store_unavailable"))?
        else {
            continue;
        };
        let mut status = if r.phase == "finished"
            || r.phase == "rejected"
            || r.phase == "received" && h["restart_hold"] == true
        {
            "terminal".to_owned()
        } else {
            "unknown".to_owned()
        };
        if status == "unknown"
            && let (Some(thread), Some(turn)) = (&r.thread_id, &r.turn_id)
            && let Ok(found) = state
                .runtime
                .request(
                    "thread/read",
                    json!({ "threadId":thread,"includeTurns":true}),
                )
                .await
            && let Some(turn) = found
                .pointer("/thread/turns")
                .and_then(Value::as_array)
                .and_then(|ts| ts.iter().find(|t| t["id"] == *turn))
        {
            status = turn["status"].as_str().unwrap_or("unknown").to_owned();
        }
        s.store.update("legacy_hold", rid, |h| {
            let before = h.clone();
            h["thread_id"] = json!(r.thread_id);
            h["turn_id"] = json!(r.turn_id);
            h["phase"] = json!("unknown");
            h["execution_status"] = json!("unknown");
            if matches!(
                status.as_str(),
                "terminal" | "completed" | "failed" | "interrupted"
            ) {
                h["state"] = json!("released");
                h["phase"] = json!("finished");
            } else if status == "inProgress" {
                h["state"] = json!("held");
                h["execution_status"] = json!("in_progress");
            }
            h["hold_state"] = h["state"].clone();
            h["last_observed_status"] = json!(status);
            if *h != before {
                h["hold_revision"] = json!(h["hold_revision"].as_u64().unwrap_or(0) + 1);
            }
            Ok(())
        })?;
    }
    Ok(())
}
