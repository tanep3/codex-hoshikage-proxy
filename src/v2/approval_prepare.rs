//! Durable execution setup. No network await is performed inside a store transaction.
use super::{
    Error, Result, approval_admission, approval_config, approval_policy as policy, approval_v06,
    catalog, mcp_grants,
    service::{Service, string},
    store,
};
use crate::{http::AppState, runtime::CodexRuntime};
use serde_json::{Value, json};
use std::sync::Arc;
fn rpc(_: crate::runtime::RuntimeError) -> Error {
    Error::code(503, "policy_setup_unknown")
}
fn process_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
fn alive(private: &Value) -> bool {
    let Some(pid) = private["runtime_pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
    else {
        return true;
    };
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Ok(stat) => stat
            .rsplit_once(") ")
            .and_then(|(_, tail)| tail.split_whitespace().nth(19))
            .is_none_or(|start| private["runtime_start"] == start),
        _ => true,
    }
}
pub fn remaining(r: &Value) -> Result<std::time::Duration> {
    let clock = policy::Clock::read()?;
    if policy::expired(&r["_approval_prepare"], &clock) {
        return Err(Error::code(409, "policy_setup_timeout"));
    }
    Ok(std::time::Duration::from_millis(
        (r["_approval_prepare"]["utc_deadline"].as_u64().unwrap() - clock.utc_ms)
            .min(r["_approval_prepare"]["boot_deadline"].as_u64().unwrap() - clock.boot_ms),
    ))
}
pub fn failure(s: &Service, rid: &str, code: &'static str) -> Result<bool> {
    s.store.transaction(|tx| {
        let mut r = store::get(tx, "response", rid)?;
        if !approval_v06::selected(&r)
            || r["approval_policy"]["preparation"]["turn_start_status"] != "not_sent"
        {
            return Ok(false);
        }
        let isolated = r["_approval_prepare"]["configuration_intent"] != true
            || r["_approval_prepare"]["configuration_confirmed"] == true;
        let reason = if !isolated {
            "policy_setup_unknown"
        } else if code == "policy_setup_timeout" {
            "policy_setup_timeout"
        } else if code == "policy_configuration_conflict" {
            "policy_configuration_conflict"
        } else {
            "policy_setup_failed"
        };
        let mut p = r["approval_policy"].clone();
        if r["stop_requested"] == true {
            policy::stop(&mut p, &r["_approval_prepare"]);
        }
        policy::failure(&mut p, reason, isolated)?;
        r["approval_policy"] = p;
        finish_preparation(tx, &mut r, isolated)?;
        Ok(true)
    })
}
fn finish_preparation(tx: &rusqlite::Transaction<'_>, r: &mut Value, isolated: bool) -> Result<()> {
    let cancelled = r["approval_policy"]["state"] == "closed";
    r["phase"] = json!(if !isolated {
        "unknown"
    } else if cancelled {
        "cancelled"
    } else {
        "rejected"
    });
    r["execution_status"] = json!("not_started");
    r["dispatch_eligible"] = json!(false);
    r["hold_state"] = json!(if isolated { "released" } else { "held" });
    r["error"] = json!({"code":if cancelled {json!("execution_cancelled")}else{r["approval_policy"]["reason"].clone()}});
    if isolated {
        r["input"] = Value::Null;
        r["output"] = json!({"state":"unavailable"});
    }
    store::put(tx, "response", string(r, "response_id")?, r)?;
    if let Some(mut op) = store::operation(tx, string(r, "request_key")?)? {
        op["state"] = json!(if isolated { "failed" } else { "unknown" });
        op["error"] = r["error"].clone();
        store::save_operation(tx, &op)?;
    }
    Ok(())
}
/// Prior settings must be removed even when the next execution uses a legacy profile.
async fn clear_previous(runtime: &CodexRuntime, s: &Service, cid: &str, rid: &str) -> Result<()> {
    let c = s.store.get("conversation", cid)?;
    let owner = &c["_approval_owner"];
    let Some(binding) = owner["binding"].as_str() else {
        return Ok(());
    };
    let Some(thread) = c["thread_id"].as_str() else {
        return Err(Error::code(409, "policy_configuration_conflict"));
    };
    if owner["runtime"] != runtime.id() {
        runtime.refresh_configuration().await.map_err(rpc)?;
        runtime
            .bind_policy_thread(thread, binding)
            .await
            .map_err(rpc)?;
    }
    let read = runtime
        .request(
            "thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await
        .map_err(rpc)?;
    if !matches!(
        read.pointer("/thread/status/type").and_then(Value::as_str),
        Some("idle" | "notLoaded")
    ) {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    let response = s.store.get("response", rid)?;
    if approval_v06::selected(&response) {
        record_intent(runtime, s, rid)?;
    }
    runtime
        .restore_policy_thread(binding, "thread/unsubscribe", json!({"threadId":thread}))
        .await
        .map_err(rpc)?;
    let empty = c["_turn_ever_sent"] == false
        && read
            .pointer("/thread/turns")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
    if !empty {
        let resumed=runtime.restore_policy_thread(binding,"thread/resume",json!({"threadId":thread,"config":{},"approvalPolicy":"on-request","approvalsReviewer":"user"})).await.map_err(rpc)?;
        if resumed.pointer("/thread/id") != Some(&json!(thread)) {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
    }
    s.store.update("conversation", cid, |c| {
        c["_approval_owner"] = Value::Null;
        if empty {
            c["thread_id"] = Value::Null;
        }
        Ok(())
    })?;
    runtime
        .release_policy_thread(thread, binding)
        .map_err(rpc)?;
    if approval_v06::selected(&response) {
        s.store.update("response", rid, |r| {
            r["_approval_prepare"]["configuration_confirmed"] = json!(true);
            r["_approval_prepare"]["configuration_intent"] = json!(false);
            r["approval_policy"]["preparation"]["configuration_isolation"] = json!("confirmed");
            Ok(())
        })?;
    }
    Ok(())
}
fn record_intent(runtime: &CodexRuntime, s: &Service, rid: &str) -> Result<()> {
    // process identity is already read before the first mutating request.
    s.store.update("response", rid, |r| {
        if r["stop_requested"] == true {
            return Err(Error::code(409, "execution_cancelled"));
        }
        let mut public = r["approval_policy"].clone();
        let mut private = r["_approval_prepare"].clone();
        policy::configuration_intent(
            &mut public,
            &mut private,
            runtime.id(),
            &policy::Clock::read()?,
        )?;
        private["configuration_confirmed"] = json!(false);
        r["approval_policy"] = public;
        r["_approval_prepare"] = private;
        Ok(())
    })?;
    Ok(())
}
pub async fn prepare(
    state: &AppState,
    s: &Arc<Service>,
    rid: &str,
    start: &Value,
) -> Result<Option<String>> {
    let r = s.store.get("response", rid)?;
    let cid = string(&r, "conversation_id")?.to_owned();
    if !approval_v06::selected(&r) {
        clear_previous(&state.runtime, s, &cid, rid).await?;
        return Ok(None);
    }
    let budget = remaining(&r)?;
    tokio::time::timeout(budget, async {
        let pid = state
            .runtime
            .process_id()
            .await
            .ok_or_else(|| Error::code(503, "policy_setup_failed"))?;
        let birth = process_start(pid).ok_or_else(|| Error::code(503, "policy_setup_failed"))?;
        s.store.update("response", rid, |r| {
            r["_approval_prepare"]["runtime_pid"] = json!(pid);
            r["_approval_prepare"]["runtime_start"] = json!(birth);
            Ok(())
        })?;
        clear_previous(&state.runtime, s, &cid, rid).await?;
        prepare_inner(state, s, rid, start).await
    })
    .await
    .map_err(|_| Error::code(409, "policy_setup_timeout"))?
}
struct Bindings<'a> {
    runtime: &'a CodexRuntime,
    service: &'a Service,
    rid: &'a str,
    binding: String,
    threads: Vec<String>,
}
impl Drop for Bindings<'_> {
    fn drop(&mut self) {
        let Ok(r) = self.service.store.get("response", self.rid) else {
            return;
        };
        let sent = r["_approval_prepare"]["configuration_intent"] == true;
        let confirmed = r["_approval_prepare"]["configuration_confirmed"] == true;
        for thread in &self.threads {
            if !sent || (confirmed && thread.starts_with("prepare:")) {
                let _ = self.runtime.release_policy_thread(thread, &self.binding);
            }
        }
    }
}
struct CatalogLease<'a> {
    manager: &'a catalog::Manager,
    key: catalog::Key,
}
impl Drop for CatalogLease<'_> {
    fn drop(&mut self) {
        self.manager.release(&self.key);
    }
}
async fn prepare_inner(
    state: &AppState,
    s: &Arc<Service>,
    rid: &str,
    start: &Value,
) -> Result<Option<String>> {
    let r = s.store.get("response", rid)?;
    let cid = string(&r, "conversation_id")?;
    let binding = string(&r["approval_policy"], "binding_id")?.to_owned();
    if r["stop_requested"] == true {
        return Err(Error::code(409, "execution_cancelled"));
    }
    state.runtime.refresh_configuration().await.map_err(rpc)?;
    let generation = mcp_grants::generation(s)?;
    let config = state
        .runtime
        .request(
            "config/read",
            json!({"cwd":start["cwd"],"includeLayers":false}),
        )
        .await
        .map_err(rpc)?;
    if !config["config"].is_object() {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    let guard = r["approval_policy"]["selection"]["id"] == "evaluated-turn-notion-guard";
    let mut target = None;
    let overrides = if guard {
        let requirements = state
            .runtime
            .request("configRequirements/read", Value::Null)
            .await
            .map_err(rpc)?;
        if requirements.get("requirements").is_none() {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
        let key = catalog::Key {
            instance: s.store.instance.clone(),
            recovery: s.store.generation.clone(),
            runtime: state.runtime.id().into(),
            thread: "__process__".into(),
            config: generation.clone(),
        };
        let _lease = CatalogLease {
            manager: &s.catalog,
            key: key.clone(),
        };
        let mut receiver = s
            .catalog
            .request(key.clone(), state.runtime.clone())
            .map_err(|_| Error::code(503, "policy_setup_failed"))?;
        let snapshot = loop {
            let status = receiver.borrow_and_update().clone();
            match status {
                catalog::Status::Ready { snapshot, .. } => break snapshot,
                catalog::Status::Failed(_) => {
                    return Err(Error::code(409, "policy_configuration_conflict"));
                }
                _ => receiver
                    .changed()
                    .await
                    .map_err(|_| Error::code(503, "policy_setup_failed"))?,
            }
        };
        let t = snapshot
            .guard_target
            .clone()
            .ok_or_else(|| Error::code(409, "policy_configuration_conflict"))?;
        if snapshot.servers.get("codex_apps").is_none_or(|server| {
            matches!(server.auth_status.as_str(), "unknown" | "notLoggedIn")
                || !matches!(
                    server.runtime_status.as_str(),
                    "notStarted" | "starting" | "connected"
                )
        }) {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
        let overrides =
            approval_config::guard_overrides(&config["config"], &requirements["requirements"], &t)?;
        target = Some(t);
        s.catalog.release(&key);
        overrides
    } else {
        approval_config::base_overrides()
    };
    let mut bindings = Bindings {
        runtime: &state.runtime,
        service: s,
        rid,
        binding: binding.clone(),
        threads: Vec::new(),
    };
    let provisional = format!("prepare:{binding}");
    state
        .runtime
        .bind_policy_thread(&provisional, &binding)
        .await
        .map_err(rpc)?;
    bindings.threads.push(provisional.clone());
    let pid = state
        .runtime
        .process_id()
        .await
        .ok_or_else(|| Error::code(503, "policy_setup_failed"))?;
    let birth = process_start(pid).ok_or_else(|| Error::code(503, "policy_setup_failed"))?;
    let c = s.store.get("conversation", cid)?;
    let existing = c["thread_id"].as_str();
    if let Some(thread) = existing {
        state
            .runtime
            .bind_policy_thread(thread, &binding)
            .await
            .map_err(rpc)?;
        bindings.threads.push(thread.into());
        let read = state
            .runtime
            .request(
                "thread/read",
                json!({"threadId":thread,"includeTurns":true}),
            )
            .await
            .map_err(rpc)?;
        if !matches!(
            read.pointer("/thread/status/type").and_then(Value::as_str),
            Some("idle" | "notLoaded")
        ) {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
    }
    s.store.update("response", rid, |r| {
        if r["stop_requested"] == true {
            return Err(Error::code(409, "execution_cancelled"));
        }
        let mut p = r["approval_policy"].clone();
        let mut private = r["_approval_prepare"].clone();
        policy::configuration_intent(
            &mut p,
            &mut private,
            state.runtime.id(),
            &policy::Clock::read()?,
        )?;
        private["configuration_confirmed"] = json!(false);
        private["runtime_pid"] = json!(pid);
        private["runtime_start"] = json!(birth);
        private["config_generation"] = json!(generation);
        private["guard_definition"] = target
            .as_ref()
            .map(|t| json!(t.definition))
            .unwrap_or(Value::Null);
        r["approval_policy"] = p;
        r["_approval_prepare"] = private;
        r["_approval_runtime"] = json!(state.runtime.id());
        Ok(())
    })?;
    if let Some(thread) = existing {
        s.store.update("conversation", cid, |c| {
            c["_approval_owner"] = json!({"binding":binding,"runtime":state.runtime.id()});
            c["thread_id"] = json!(thread);
            Ok(())
        })?;
    }
    let mut params = start.clone();
    params["config"] = overrides;
    params["approvalsReviewer"] = json!("user");
    let method = if let Some(thread) = existing {
        state
            .runtime
            .request_scoped(&binding, "thread/unsubscribe", json!({"threadId":thread}))
            .await
            .map_err(rpc)?;
        params["threadId"] = json!(thread);
        params.as_object_mut().unwrap().remove("dynamicTools");
        "thread/resume"
    } else {
        "thread/start"
    };
    let result = state
        .runtime
        .request_scoped(&binding, method, params)
        .await
        .map_err(rpc)?;
    let thread = result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::code(503, "policy_setup_unknown"))?
        .to_owned();
    if existing.is_some_and(|old| old != thread)
        || result["approvalPolicy"] != "on-request"
        || result["approvalsReviewer"] != "user"
    {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    state
        .runtime
        .bind_policy_thread(&thread, &binding)
        .await
        .map_err(rpc)?;
    s.store.update("conversation", cid, |c| {
        c["thread_id"] = json!(thread);
        c["_approval_owner"] = json!({"binding":binding,"runtime":state.runtime.id()});
        Ok(())
    })?;
    state
        .runtime
        .release_policy_thread(&provisional, &binding)
        .map_err(rpc)?;
    // The response acknowledges the only mutating RPC. A stop still wins before readiness.
    s.store.update("response", rid, |r| {
        r["thread_id"] = json!(thread);
        r["_approval_prepare"]["configuration_confirmed"] = json!(true);
        Ok(())
    })?;
    if let Some(target) = target {
        let key = catalog::Key {
            instance: s.store.instance.clone(),
            recovery: s.store.generation.clone(),
            runtime: state.runtime.id().into(),
            thread: thread.clone(),
            config: generation.clone(),
        };
        let mut receiver = s
            .catalog
            .request(key.clone(), state.runtime.clone())
            .map_err(|_| Error::code(503, "policy_setup_failed"))?;
        loop {
            let status = receiver.borrow_and_update().clone();
            match status {
                catalog::Status::Ready { snapshot, .. } => {
                    if snapshot.servers.get("codex_apps").is_some_and(|server| {
                        matches!(server.runtime_status.as_str(), "notStarted" | "starting")
                    }) {
                        if s.store.get("response", rid)?["stop_requested"] == true {
                            return Err(Error::code(409, "execution_cancelled"));
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        s.catalog.release(&key);
                        receiver = s
                            .catalog
                            .request(key.clone(), state.runtime.clone())
                            .map_err(|_| Error::code(503, "policy_setup_failed"))?;
                        continue;
                    }
                    if snapshot.guard_target.as_ref() != Some(&target)
                        || snapshot
                            .servers
                            .get("codex_apps")
                            .is_none_or(|server| server.available().is_err())
                    {
                        return Err(Error::code(409, "policy_configuration_conflict"));
                    }
                    break;
                }
                catalog::Status::Failed(_) => {
                    return Err(Error::code(409, "policy_configuration_conflict"));
                }
                _ => receiver
                    .changed()
                    .await
                    .map_err(|_| Error::code(503, "policy_setup_failed"))?,
            }
        }
    }
    s.store.update("response", rid, |r| {
        if r["stop_requested"] == true {
            return Err(Error::code(409, "execution_cancelled"));
        }
        if mcp_grants::generation(s)? != generation {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
        let mut p = r["approval_policy"].clone();
        let mut private = r["_approval_prepare"].clone();
        // The opaque runtime+binding proof contains neither config nor argument hashes.
        policy::ready(
            &mut p,
            &mut private,
            &format!("{}:{binding}", state.runtime.id()),
            &policy::Clock::read()?,
        )?;
        r["approval_policy"] = p;
        r["_approval_prepare"] = private;
        approval_admission::reserve_response(r)?;
        Ok(())
    })?;
    Ok(Some(thread))
}
pub fn turn_intent(s: &Service, rid: &str) -> Result<Option<String>> {
    s.store.transaction(|tx| {
        let mut r = store::get(tx, "response", rid)?;
        let binding = if approval_v06::selected(&r) {
            let mut p = r["approval_policy"].clone();
            policy::turn_intent(
                &mut p,
                &r["_approval_prepare"],
                r["stop_requested"] == true,
                &policy::Clock::read()?,
            )?;
            if r["_approval_prepare"]["config_generation"] != mcp_grants::generation(s)? {
                return Err(Error::code(409, "policy_configuration_conflict"));
            }
            r["approval_policy"] = p;
            Some(string(&r["approval_policy"], "binding_id")?.to_owned())
        } else {
            None
        };
        if r["stop_requested"] == true {
            return Err(Error::code(409, "execution_cancelled"));
        }
        r["execution_status"] = json!("unknown");
        store::put(tx, "response", rid, &r)?;
        let cid = string(&r, "conversation_id")?;
        let mut c = store::get(tx, "conversation", cid)?;
        c["_turn_ever_sent"] = json!(true);
        store::put(tx, "conversation", cid, &c)?;
        Ok(binding)
    })
}
pub fn close(r: &mut Value) {
    if approval_v06::selected(r) && r["approval_policy"]["state"] == "ready" {
        r["approval_policy"]["state"] = json!("closed");
        r["approval_policy"]["reason"] = json!("scope_ended");
    }
}
pub fn reconcile(s: &Service, r: &Value) -> Result<bool> {
    if !approval_v06::selected(r)
        || r["execution_status"] != "not_started"
        || r["approval_policy"]["preparation"]["configuration_isolation"] != "pending"
    {
        return Ok(false);
    }
    if alive(&r["_approval_prepare"]) {
        return Ok(true);
    }
    s.store.transaction(|tx| {
        let mut current = store::get(tx, "response", string(r, "response_id")?)?;
        if current["execution_status"] != "not_started" {
            return Ok(());
        }
        current["approval_policy"]["preparation"]["configuration_isolation"] = json!("confirmed");
        current["approval_policy"]["preparation"]["recovery_state"] = json!("fenced");
        finish_preparation(tx, &mut current, true)
    })?;
    Ok(true)
}
