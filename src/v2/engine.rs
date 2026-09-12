use super::{
    Error, Result, now,
    service::{Service, string},
    store,
};
use crate::{approval_manager::ApprovalCapability, http::AppState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Write, os::unix::fs::OpenOptionsExt, sync::Arc, time::Duration};

struct WorkerGuard {
    service: Arc<Service>,
    rid: String,
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Ok(mut workers) = self.service.workers.lock() {
            workers.remove(&self.rid);
        }
    }
}
pub async fn run(state: AppState, service: Arc<Service>, rid: String) {
    if !service.workers.lock().unwrap().insert(rid.clone()) {
        return;
    }
    let _worker = WorkerGuard {
        service: service.clone(),
        rid: rid.clone(),
    };
    let handle = tokio::runtime::Handle::current();
    let backup_service = service.clone();
    let backup_rid = rid.clone();
    if tokio::task::spawn_blocking(move || handle.block_on(run_inner(state, service, rid)))
        .await
        .is_err()
    {
        let _ = backup_service.store.update("response", &backup_rid, |r| {
            r["phase"] = json!("unknown");
            r["execution_status"] = json!("unknown");
            r["stop_requested"] = json!(true);
            Ok(())
        });
    }
}
async fn run_inner(state: AppState, service: Arc<Service>, rid: String) {
    let outcome = execute(&state, &service, &rid).await;
    if let Err(error) = outcome {
        let result = service.store.update("response", &rid, |r| {
            if r["phase"] == "accepted" || error.code == "execution_rejected" {
                r["phase"] = json!("rejected");
                r["hold_state"] = json!("released");
                r["execution_status"] = json!("not_started");
            } else if r["phase"] != "cancelled" && r["phase"] != "finished" {
                r["phase"] = json!("unknown");
                r["execution_status"] = json!("unknown");
            }
            if r["output"]["state"] == "saving" {
                r["output"]["state"] = json!("failed");
            }
            r["error"] = json!({ "code":error.code});
            if r["phase"] == "unknown" {
                r["stop_requested"] = json!(true);
            }
            if r["phase"] == "rejected" {
                r["input"] = Value::Null;
                r["output"] = json!({ "state":"unavailable"});
            }

            Ok(())
        });
        if let Ok(r) = &result {
            let _ = service.store.transaction(|tx| {
                if let Some(mut op) = store::operation(tx, string(r, "request_key")?)? {
                    if r["phase"] == "rejected" {
                        op["state"] = json!("failed");
                        op["error"] = r["error"].clone();
                    } else if r["phase"] == "unknown" {
                        op["state"] = json!("unknown");
                    }
                    store::save_operation(tx, &op)?;
                }
                Ok(())
            });
        }
        if result.is_err() {
            tracing::error!("v2 execution state persistence failed");
        }
    }
}
async fn execute(state: &AppState, s: &Arc<Service>, rid: &str) -> Result<()> {
    let r = s.store.get("response", rid)?;
    if r["phase"] != "accepted" {
        return Ok(());
    }
    let output_limit = r["output_policy"]["max_bytes"]
        .as_u64()
        .unwrap_or(s.limits.output_max_bytes) as usize;
    let cid = string(&r, "conversation_id")?;
    let cwd = s.workspace_path(cid)?;
    state
        .cwd_policy
        .validate(&cwd)
        .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
    let body = r["input"].clone();
    let input = crate::http::normalize_input(&body["input"])
        .map_err(|_| Error::code(400, "invalid_argument"))?;
    let schema = crate::http::parse_output_schema(body.pointer("/text/format"))
        .map_err(|_| Error::code(400, "invalid_argument"))?;
    let model = state
        .catalog
        .resolve(
            r["model"].as_str(),
            body.pointer("/reasoning/effort").and_then(Value::as_str),
        )
        .await
        .map_err(|_| Error::code(400, "model_unavailable"))?;
    let provider_limit = state
        .catalog
        .provider_limits()
        .get(&model.public_provider_id)
        .copied()
        .unwrap_or(1);
    let occupied = s
        .store
        .list("response")?
        .iter()
        .filter(|r| {
            r["hold_state"] == "held"
                && r["model"]
                    .as_str()
                    .is_some_and(|m| m.split('/').next() == Some(model.public_provider_id.as_str()))
        })
        .count();
    if occupied > provider_limit {
        return Err(Error::code(409, "provider_busy"));
    }
    let _permit = state
        .permits
        .acquire(&model.public_provider_id)
        .await
        .map_err(|_| Error::code(503, "provider_unavailable"))?;
    let dispatch = s.store.update("response", rid, |v| {
        if v["phase"] == "accepted" && v["input_expires_at_ms"].as_u64().unwrap_or(0) <= now() {
            v["phase"] = json!("rejected");
            v["hold_state"] = json!("released");
            v["input"] = Value::Null;
            v["output"] = json!({ "state":"unavailable"});
        }
        if v["phase"] == "accepted" && !v["stop_requested"].as_bool().unwrap_or(false) {
            v["phase"] = json!("dispatching");
            v["execution_status"] = json!("unknown");
        }
        Ok(())
    })?;
    if dispatch["phase"] != "dispatching" {
        if dispatch["phase"] == "rejected" {
            s.store.transaction(|tx| {
                if let Some(mut op) = store::operation(tx, string(&dispatch, "request_key")?)? {
                    op["state"] = json!("failed");
                    op["error"] = json!({ "code":"input_expired"});
                    store::save_operation(tx, &op)?;
                }
                Ok(())
            })?;
        }
        return Ok(());
    }
    let mut events = state.runtime.subscribe();
    let c = s.store.get("conversation", cid)?;
    let thread = if let Some(t) = c["thread_id"].as_str() {
        state
            .runtime
            .request(
                "thread/resume",
                json!({
                    "threadId":t,
                    "cwd":cwd,
                    "model":model.upstream_model_id,
                    "modelProvider":model.codex_provider_id
                }),
            )
            .await
            .map_err(rpc_error)?;
        t.to_owned()
    } else {
        let result = state
            .runtime
            .request("thread/start", json!({
                "cwd":cwd,
                "model":model.upstream_model_id,
                "modelProvider":model.codex_provider_id,
                "approvalPolicy":"on-request",
                "sandbox":state.sandbox_mode,
                "dynamicTools":[{ "type":"function","name":"hoshikage_publish_artifact","description":"Publish a completed file as an immutable downloadable artifact. Close the file first. Registration does not send it to the user.","inputSchema":{ "type":"object","properties":{ "path":{ "type":"string"} ,"display_name":{ "type":"string"} } ,"required":["path"],"additionalProperties":false} } ]
            }))
            .await
            .map_err(rpc_error)?;
        result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::code(502, "execution_unknown"))?
            .to_owned()
    };
    s.store.update("conversation", cid, |c| {
        c["thread_id"] = json!(thread);
        Ok(())
    })?;
    let r = s.store.update("response", rid, |r| {
        r["thread_id"] = json!(thread);
        if r["stop_requested"] == true {
            r["phase"] = json!("cancelled");
            r["execution_status"] = json!("not_started");
            r["hold_state"] = json!("released");
            r["dispatch_eligible"] = json!(false);
            r["input"] = Value::Null;
            r["output"] = json!({ "state":"unavailable"});
        }
        Ok(())
    })?;
    if r["phase"] == "cancelled" {
        s.store.transaction(|tx| {
            let mut op = store::operation(tx, string(&r, "request_key")?)?
                .ok_or_else(|| Error::code(503, "store_corrupt"))?;
            op["state"] = json!("failed");
            op["error"] = json!({ "code":"execution_cancelled"});
            store::save_operation(tx, &op)?;
            Ok(())
        })?;
        return Ok(());
    }
    let suppression = c["suppress_auto_approval"].as_bool().unwrap_or(false)
        || body.pointer("/metadata/codex.auto_approve_workspace") == Some(&json!("false"));
    s.store.update("conversation", cid, |c| {
        c["suppress_auto_approval"] = json!(suppression);
        Ok(())
    })?;
    let capability =
        if body.pointer("/metadata/codex.approval_capability") == Some(&json!("interactive")) {
            ApprovalCapability::Interactive
        } else {
            ApprovalCapability::None
        };
    state
        .approvals
        .register_turn(&thread, capability, &cwd, suppression)
        .await;
    let result = state
        .runtime
        .request(
            "turn/start",
            json!({
                "threadId":thread,
                "cwd":cwd,
                "model":model.upstream_model_id,
                "input":input,
                "outputSchema":schema,
                "approvalPolicy":"on-request",
                "effort":model.reasoning_effort.map(crate::http::reasoning_name)
            }),
        )
        .await
        .map_err(rpc_error)?;
    let turn = result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::code(502, "execution_unknown"))?
        .to_owned();
    state.approvals.bind_turn(&thread, &turn).await;
    s.store.update("response", rid, |r| {
        r["turn_id"] = json!(turn);
        r["phase"] = json!("started");
        r["execution_status"] = json!("in_progress");
        r["input"] = Value::Null;
        Ok(())
    })?;
    s.store.update("conversation", cid, |c| {
        c["model"] = json!(model.public_model_id);
        c["artifact_registration"] = json!(if super::service::registration_supported(
            &model.public_model_id
        ) {
            "available"
        } else {
            "unavailable"
        });
        Ok(())
    })?;
    // Legacy control routes can address v2 turns, while v2 remains the lifecycle owner.
    let mut execution = crate::control::Execution::new(rid.into(), None, None);
    execution.thread_id = Some(thread.clone());
    execution.turn_id = Some(turn.clone());
    execution.model_id = Some(model.public_model_id.clone());
    execution.phase = "started".into();
    execution.last_observed_status = "inProgress".into();
    state
        .responses
        .control
        .reserve(execution)
        .map_err(|_| Error::code(503, "store_unavailable"))?;
    s.store.transaction(|tx| {
        let r = store::get(tx, "response", rid)?;
        if let Some(mut op) = store::operation(tx, string(&r, "request_key")?)? {
            op["state"] = json!("succeeded");
            store::save_operation(tx, &op)?;
        }
        Ok(())
    })?;
    let mut text = String::new();
    let mut final_text: Option<String> = None;
    let mut output_overflow = false;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::select! {
                 _=tick.tick()=>{
                  let r=s.store.get("response",rid)?;
                  if r["stop_requested"]==true&&r["interrupt_delivery"]=="not_sent" {
                   s.store.update("response",rid,|r|{
        r["interrupt_delivery"]=json!("dispatching");
        Ok(())}
        )?;
                   let result=crate::http::request_interrupt(&state.runtime,&thread,&turn).await;
                   s.store.update("response",rid,|r|{
        r["interrupt_delivery"]=json!(if result.is_ok(){ "accepted"} else{ "unknown"});
        Ok(())}
        )?;
                  }
                  if last.elapsed()>state.turn_idle_timeout{
        return Err(Error::code(504,"execution_unknown"));
        }
                 }
        ,
                 event=events.recv()=>{
                  let event=event.map_err(|_|Error::code(502,"execution_unknown"))?;
                  if event["kind"]=="transport_closed"{
        return Err(Error::code(502,"execution_unknown"));
        }
                  if let Some(code)=crate::http::interaction_error(&event,&thread)
                    && event["turnId"].as_str().is_none_or(|id|id==turn) {
                    s.store.update("response",rid,|r|{
        r["stop_requested"]=json!(true);
        r["error"]=json!({ "code":code});
        Ok(())}
        )?;
                    continue;
                  }
                  let p=&event["params"];
                  if p["threadId"]!=thread {
        continue;
        }
                  if p.get("turnId").or_else(||p.pointer("/turn/id")).is_some_and(|v|v!=&turn){
        continue;
        }
                  last=tokio::time::Instant::now();
                  if event["method"]=="item/tool/call"&&p["tool"]=="hoshikage_publish_artifact"{
                   if !p["arguments"].is_object(){
                     state.runtime.respond_to_server_request(event["rpc_id"].clone(),json!({
                         "success":false,
                         "contentItems":[{ "type":"inputText","text":"invalid_argument"} ]
                     })).await.map_err(rpc_error)?;
                     continue;
                   }
                   let mut args=p["arguments"].clone();
        args["response_id"]=json!(rid);
                   let key=format!("tool-{}",crate::control::fingerprint(&json!([thread,turn,p["callId"]])));
                   let svc=s.clone();
        let cid=cid.to_owned();
        let runtime=state.runtime.clone();
        let rpc_id=event["rpc_id"].clone();
                   // A file copy must not hold up interrupt delivery or terminal observations.
                   tokio::spawn(async move {
                     let outcome = match svc.copies.clone().try_acquire_owned() {
                       Ok(permit) => tokio::task::spawn_blocking(move || {
        let _permit=permit;
        svc.capture(&cid,&key,&args)}
        ).await.unwrap_or_else(|_|Err(Error::code(503,"store_unavailable"))),
                       Err(_) => Err(Error::code(429,"capture_capacity_busy")),
                     }
        ;
                     let success=outcome.as_ref().is_ok_and(|v|v["state"]=="succeeded");
                     let content=outcome.unwrap_or_else(|e|json!({ "error":{ "code":e.code} }));
                     if runtime.respond_to_server_request(rpc_id,json!({
                         "success":success,
                         "contentItems":[{ "type":"inputText","text":content.to_string()} ]
                     })).await.is_err(){
        tracing::warn!("artifact tool result delivery unknown");
        }
                   }
        );
                  }
                  if event["method"]=="item/completed"&&p["item"]["type"]=="agentMessage"&&p["item"]["phase"]!="commentary"
                   && let Some(value)=p["item"]["text"].as_str(){
        if value.len()>output_limit{
        output_overflow=true;
        }
        else{
        final_text=Some(value.to_owned());
        }
        }
                  if event["method"]=="item/agentMessage/delta"
                   && let Some(delta)=p["delta"].as_str(){
        if text.len().saturating_add(delta.len())>output_limit{
        output_overflow=true;
        }
        else if !output_overflow{
        text.push_str(delta);
        }
        }
                  if event["method"]=="turn/completed"{
                   let status=p.pointer("/turn/status").and_then(Value::as_str).unwrap_or("unknown");
                   if !matches!(status,"completed"|"failed"|"interrupted"){
        return Err(Error::code(502,"execution_unknown"));
        }
                   s.store.update("response",rid,|r|{
        r["phase"]=json!("finished");
        r["execution_status"]=json!(status);
        r["last_observed_status"]=json!(status);
        r["hold_state"]=json!("released");
        r["output"]=json!({ "state":if status=="completed"{ "saving"} else{ "unavailable"} });
        Ok(())}
        )?;
                   s.store.update("conversation",cid,|c|{
        if c["active_response_id"]==rid{
        c["active_response_id"]=Value::Null;
        }
        Ok(())}
        )?;
                   if status=="completed" && output_overflow {
                    s.store.update("response",rid,|r|{
        r["output"]=json!({ "state":"failed","error":{ "code":"output_too_large"} });
        Ok(())}
        )?;
                   }
         else if status=="completed"{
                    let content=json!({
                        "response_id":rid,
                        "model":model.public_model_id,
                        "output":[{ "type":"message","role":"assistant","content":[{ "type":"output_text","text":final_text.as_deref().unwrap_or(&text)} ]} ]
                    });
                    let svc=s.clone();
        let rid=rid.to_owned();
                    tokio::task::spawn_blocking(move||save_output(&svc,&rid,&content)).await.map_err(|_|Error::code(503,"store_unavailable"))??;
                   }
                   state.approvals.invalidate_turn(&thread,&turn).await;
                   return Ok(());
                  }
                 }
                }
    }
}
pub(crate) fn save_output(s: &Service, rid: &str, content: &Value) -> Result<()> {
    let policy = s.store.get("response", rid)?["output_policy"].clone();
    let data = serde_json::to_vec(content)?;
    if data.len()
        > policy["max_bytes"]
            .as_u64()
            .unwrap_or(s.limits.output_max_bytes) as usize
    {
        return Err(Error::code(413, "output_too_large"));
    }
    let staging = s.store.root.join("staging").join(rid);
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&staging)?;
    f.write_all(&data)?;
    f.sync_all()?;
    let metadata = json!({
        "state":"ready",
        "ready_at_ms":now(),
        "size_bytes":data.len(),
        "sha256":format!("{:x}",Sha256::digest(&data)),
        "expires_at_ms":now()+policy["retention_seconds"].as_u64().unwrap_or(s.limits.output_retention_seconds)*1000,
        "max_hold_until_ms":now()+policy["lease_max_lifetime_seconds"].as_u64().unwrap_or(s.limits.lease_max_lifetime_seconds)*1000
    });
    super::files::publish(&staging, &s.store.root.join("blobs"), rid, &metadata)?;
    s.store.update("response", rid, |r| {
        r["output"] = metadata;
        Ok(())
    })?;
    Ok(())
}

fn rpc_error(error: crate::runtime::RuntimeError) -> Error {
    if matches!(&error,crate::runtime::RuntimeError::Protocol(s) if s.contains("(-32602)")) {
        Error::code(409, "execution_rejected")
    } else {
        Error::code(502, "execution_unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_output_reservation_survives_lowered_limits() {
        let root = std::env::temp_dir().join(format!("v2-limit-reload-{}", uuid::Uuid::new_v4()));
        let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
        let op = s
            .conversation(
                "c",
                &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
            )
            .unwrap();
        let cid = op["resource"]["id"].as_str().unwrap();
        let (_, rid) = s.accept(cid, "run", &json!({"input":"test"})).unwrap();
        let rid = rid.unwrap();
        let reserved = s.limits.output_max_bytes;
        drop(s);
        let limits = super::super::limits::Limits {
            output_max_bytes: 1024,
            ..Default::default()
        };
        let s = Service::open_with_limits(&root.join("state"), &root.join("work"), limits).unwrap();
        assert_eq!(s.capacity().unwrap()["outputs"]["reserved_bytes"], reserved);
        save_output(&s, &rid, &json!({"text":"x".repeat(4096)})).unwrap();
        assert_eq!(
            s.store.get("response", &rid).unwrap()["output"]["state"],
            "ready"
        );
    }
}
