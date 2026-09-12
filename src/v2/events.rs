use super::{Result, service::Service};
use crate::http::AppState;
use axum::response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
};
use serde_json::{Value, json};
use std::sync::Arc;
pub fn stream(state: AppState, s: Arc<Service>, rid: String) -> Result<Response> {
    s.store.get("response", &rid)?;
    let mut upstream = state.runtime.subscribe();
    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::result::Result<Event, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        let mut last = Value::Null;
        let mut reported_artifacts = std::collections::HashSet::<String>::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            tokio::select! {
                         _=tx.closed()=>return,
                         _=interval.tick()=>{
                          let service=s.clone();
            let id=rid.clone();
            let result=tokio::task::spawn_blocking(move||service.store.get("response",&id)).await;
                          let Ok(Ok(mut r))=result else{
            let _=tx.try_send(Ok(event("gap",json!({ "response_id":rid,"reason":"store_unavailable"}))));
            return;
            }
            ;
                          r.as_object_mut().unwrap().remove("input");
            r.as_object_mut().unwrap().remove("request_key");
                          if r!=last {
            if !send(&tx,"snapshot",&r){
            return;
            }
                           if last["phase"]!=r["phase"]&&r["phase"]=="finished"&& !send(&tx,"response.execution_terminal",&r){
            return;
            }
                           if last["output"]["state"]!=r["output"]["state"] {
            let name=match r["output"]["state"].as_str(){
            Some("ready")=>Some("response.output_ready"),Some("failed"|"unavailable")=>Some("response.output_failed"),_=>None}
            ;
            if let Some(name)=name&& !send(&tx,name,&r){
            return;
            }
            }
                           if last["generated_images"]["revision"] != r["generated_images"]["revision"]
                             && !send(&tx,"response.generated_images_changed",&json!({"response_id":rid,"revision":r["generated_images"]["revision"]})) { return; }
                           last=r;
                          }
                          let svc=s.clone();
            let response_id=rid.clone();
                          if let Ok(Ok(artifacts))=tokio::task::spawn_blocking(move||svc.store.list("artifact")).await {
                            for a in artifacts {
            if a["response_id"]==response_id&&a["state"]=="ready"&&let Some(aid)=a["artifact_id"].as_str()&&reported_artifacts.insert(aid.to_owned())&& !send(&tx,"artifact.ready",&json!({ "response_id":rid,"artifact_id":aid})){
            return;
            }
            }
                          }
                         }
            ,
                         notification=upstream.recv()=>{
                          let Ok(n)=notification else{
            let _=tx.try_send(Ok(event("gap",json!({ "response_id":rid,"reason":"notification_loss"}))));
            return;
            }
            ;
                          if n["kind"]=="transport_closed"{
            let _=tx.try_send(Ok(event("gap",json!({ "response_id":rid,"reason":"transport_closed"}))));
            return;
            }
                          let p=&n["params"];
                          if !last["turn_id"].is_string() && n["method"]=="item/agentMessage/delta" {
                            // Live deltas have no replay guarantee; make an early subscription gap explicit.
                            let _=send(&tx,"gap",&json!({ "response_id":rid,"reason":"turn_binding_pending"}));
            return;
                          }
                          if last["turn_id"].is_string()&&p["turnId"]==last["turn_id"]&&p["threadId"]==last["thread_id"]&&n["method"]=="item/agentMessage/delta"
                           && !send(&tx,"response.delta",&json!({ "response_id":rid,"delta":p["delta"]})){
            return;
            }
                         }
                        }
        }
    });
    Ok(Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response())
}
fn event(name: &str, v: Value) -> Event {
    Event::default()
        .event(name)
        .json_data(super::retention::wire(v))
        .unwrap_or_default()
}
fn send(
    tx: &tokio::sync::mpsc::Sender<std::result::Result<Event, std::convert::Infallible>>,
    name: &str,
    v: &Value,
) -> bool {
    if tx.capacity() < 2 {
        let _ = tx.try_send(Ok(event(
            "gap",
            json!({ "response_id":v["response_id"],"reason":"slow_observer"}),
        )));
        return false;
    }
    tx.try_send(Ok(event(name, v.clone()))).is_ok()
}
pub fn start_maintenance(state: AppState, s: Arc<Service>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            if state.runtime.snapshot().await == crate::domain::RuntimeState::Stopped {
                return;
            }
            if let Ok(records) = s.store.list("response") {
                for r in records {
                    super::images::schedule(state.clone(), s.clone(), &r);
                    let Some(rid) = r["response_id"].as_str() else {
                        continue;
                    };
                    if r["phase"] == "accepted" && !s.workers.lock().unwrap().contains(rid) {
                        tokio::spawn(super::engine::run(state.clone(), s.clone(), rid.to_owned()));
                    }
                }
            }
            if let Err(e) = super::coordination::reconcile(&state, &s).await {
                tracing::error!(code = e.code, "v2 reconciliation failed");
            }
            let svc = s.clone();
            let result = tokio::task::spawn_blocking(move || super::retention::gc(&svc)).await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::error!("v2 retention maintenance failed");
            }
        }
    })
}
