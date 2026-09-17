use super::{
    Error, Result, engine,
    service::{Service, stop_status, string},
    store,
};
use crate::http::AppState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(
    State(state): State<AppState>,
    Path(path): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(service) = state.v2.clone() else {
        return Error::code(404, "v2_not_enabled").into_response();
    };
    let result = checked(state, service.clone(), path, method, uri, headers, body).await;
    let mut response = result.unwrap_or_else(IntoResponse::into_response);
    response.headers_mut().insert(
        "x-proxy-instance-id",
        service.store.instance.parse().unwrap(),
    );
    response.headers_mut().insert(
        "x-proxy-recovery-generation",
        service.store.generation.parse().unwrap(),
    );
    response
}
async fn checked(
    state: AppState,
    s: Arc<Service>,
    path: String,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    if state.api_key.is_none() {
        return Err(Error::code(401, "api_key_required"));
    }
    if path == "capabilities" && method == Method::GET {
        let mut limits = serde_json::to_value(&s.limits)?;
        for (k, v) in [
            ("auth_scope", json!("shared_operator")),
            ("workspace_isolation", json!("operational_separation")),
            ("execution_disconnect_interrupts", json!(false)),
            ("event_replay", json!(false)),
            (
                "operations_retention",
                json!("until_explicit_state_retirement"),
            ),
        ] {
            limits[k] = v;
        }
        let capability = json!({
            "contract_version":"2.0",
            "implementation_status":"acceptance_pending",
            "recovery_state":s.store.metadata("recovery_state")?,
            "instance_id":s.store.instance,
            "recovery_generation":s.store.generation,
            "features":{ "managed_conversations":true,"durable_execution":true,"stop_by_request":true,"stop_before_acceptance":true,"administrative_hold_release":"local_operator","workspace_selection":true,"artifact_capture":true,"artifact_listing":true,"artifact_range_download":true,"retention_leases":true,"artifact_registration_tool":true,"response_output_retrieval":true,"generated_image_artifacts":true,"response_generated_images":true,"interaction_relay":true},
            "limits":limits,
            "interaction_kinds":super::interactions::KINDS,
            "mcp_inline_approval":super::presentations::capability(s.limits.mcp_turn_approval_enabled),
            "mcp_approval_v06":super::approval_admission::capability(s.limits.mcp_turn_approval_enabled)?,
            "mcp_operation_details":{"enabled":s.limits.mcp_turn_approval_enabled,"profile":"native-item-id-v1","max_argument_bytes":65536,"disclosure":"requester_only"},
            "mcp_turn_approval":{"enabled":s.limits.mcp_turn_approval_enabled,"profile":"native-item-id-v1","max_grants":16,"ttl_seconds":600,"max_records":super::mcp_grants::MAX_RECORDS},
            "interaction_limits":{"max_count":super::interactions::MAX_COUNT,"max_bytes":super::interactions::MAX_BYTES,"timeout_seconds":super::interactions::TIMEOUT_MS/1000,"schema_profile":"flat-primitives-v1","permission_profile":"whole-category-v1"},
            "registration_models":["chatgpt/gpt-5.6-luna","chatgpt/gpt-5.6-terra"],
            "server_time":super::retention::wire(json!({ "server_at_ms":super::now()} ))["server_at"]
        });
        return Ok(Json(super::approval_admission::bounded(capability, 1048576)?).into_response());
    }
    for (name, expected) in [
        ("x-proxy-instance-id", &s.store.instance),
        ("x-proxy-recovery-generation", &s.store.generation),
    ] {
        let value = headers
            .get(name)
            .ok_or_else(|| Error::code(428, "instance_precondition_required"))?;
        if value.to_str().ok() != Some(expected) {
            return Err(Error::code(
                409,
                if name == "x-proxy-instance-id" {
                    "instance_mismatch"
                } else {
                    "recovery_generation_mismatch"
                },
            ));
        }
    }
    let key = if method == Method::POST {
        let key = headers
            .get("idempotency-key")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| Error::code(400, "invalid_request_id"))?;
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(Error::code(400, "invalid_request_id"));
        }
        key.to_owned()
    } else {
        String::new()
    };
    let body: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|_| Error::code(400, "invalid_argument"))?
    };
    let parts: Vec<&str> = path.split('/').collect();
    if method == Method::GET && parts.as_slice() == ["capacity"] {
        return blocking(move || s.capacity())
            .await
            .map(|v| Json(v).into_response());
    }
    if method == Method::POST && parts.as_slice() == ["conversations"] {
        fields(&body, &["workspace", "model"])?;
        if body.get("model").is_some_and(|m| !m.is_string()) {
            return Err(Error::code(400, "invalid_argument"));
        }
        fields(&body["workspace"], &["mode", "workspace_id"])?;
        if body["workspace"]["mode"] == "automatic"
            && body["workspace"].get("workspace_id").is_some()
        {
            return Err(Error::code(400, "invalid_argument"));
        }
        if let Some(op) = replay(&s, &key, "conversation.create", &body)? {
            return Ok((StatusCode::ACCEPTED, Json(super::retention::wire(op))).into_response());
        }
        let mut body = body;
        let model = state
            .catalog
            .resolve(body["model"].as_str(), None)
            .await
            .map_err(|_| Error::code(400, "model_unavailable"))?;
        if body["model"].is_null() {
            body["_resolved_model"] = json!(model.public_model_id);
        }
        return blocking(move || s.conversation(&key, &body))
            .await
            .map(|v| (StatusCode::ACCEPTED, Json(super::retention::wire(v))).into_response());
    }
    if method == Method::POST
        && parts.len() == 3
        && parts[0] == "conversations"
        && parts[2] == "responses"
    {
        fields(
            &body,
            &[
                "input",
                "model",
                "metadata",
                "reasoning",
                "text",
                "interaction_capabilities",
                "approval_context",
                "approval_presentation",
                "approval_policy",
            ],
        )?;
        let body = super::approval_admission::canonical(&body);
        super::interactions::validate_capabilities(body.get("interaction_capabilities"))?;
        if let Some(op) = replay(
            &s,
            &key,
            "response.create",
            &json!({"conversation_id":parts[1],"request":body}),
        )? {
            return Ok((StatusCode::ACCEPTED, Json(super::retention::wire(op))).into_response());
        }
        if body.get("model").is_some_and(|m| !m.is_string()) {
            return Err(Error::code(400, "invalid_argument"));
        }
        crate::http::normalize_input(&body["input"])
            .map_err(|_| Error::code(400, "invalid_argument"))?;
        crate::http::parse_output_schema(body.pointer("/text/format"))
            .map_err(|_| Error::code(400, "invalid_argument"))?;
        if let Some(m) = body.get("metadata") {
            if !m.is_object() {
                return Err(Error::code(400, "invalid_argument"));
            }
            if m.get("codex.auto_approve_workspace")
                .is_some_and(|v| !matches!(v.as_str(), Some("true" | "false")))
                || m.get("codex.approval_capability")
                    .is_some_and(|v| !matches!(v.as_str(), Some("none" | "interactive")))
            {
                return Err(Error::code(400, "invalid_argument"));
            }
        }
        if body.pointer("/metadata/codex.cwd").is_some() {
            return Err(Error::code(422, "unknown_field"));
        }
        let cid = parts[1].to_owned();
        let svc = s.clone();
        let c = s.store.get("conversation", &cid)?;
        let provider = c["model"]
            .as_str()
            .and_then(|m| m.split('/').next())
            .ok_or_else(|| Error::code(503, "store_corrupt"))?;
        let limit = state
            .catalog
            .provider_limits()
            .get(provider)
            .copied()
            .unwrap_or(1);
        let (v, rid) =
            blocking(move || svc.accept_with_provider_limit(&cid, &key, &body, Some(limit)))
                .await?;
        if let Some(rid) = rid {
            tokio::spawn(engine::run(state, s, rid));
        }
        return Ok((StatusCode::ACCEPTED, Json(super::retention::wire(v))).into_response());
    }
    if method == Method::POST && parts.as_slice() == ["stops"] {
        fields(&body, &["target"])?;
        fields(
            &body["target"],
            &["conversation_id", "request_key", "response_id"],
        )?;
        let target = &body["target"];
        if target.get("response_id").is_some()
            && (target.get("request_key").is_some() || target.get("conversation_id").is_some())
        {
            return Err(Error::code(400, "invalid_argument"));
        }
        let v = blocking(move || s.stop(&key, &body)).await?;
        let status = if matches!(
            v["stop_status"].as_str(),
            Some("cancelled_before_start" | "already_terminal")
        ) {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        };
        return Ok((status, Json(super::retention::wire(v))).into_response());
    }
    if method == Method::POST
        && parts.len() == 3
        && parts[0] == "conversations"
        && parts[2] == "artifacts"
    {
        fields(&body, &["path", "display_name", "response_id"])?;
        let cid = parts[1].to_owned();
        let root = s.workspace_path(&cid)?;
        state
            .cwd_policy
            .validate(root)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        if let Some(op) = replay(
            &s,
            &key,
            "artifact.create",
            &json!({"conversation_id":cid,"request":body}),
        )? {
            return Ok((StatusCode::ACCEPTED, Json(super::retention::wire(op))).into_response());
        }
        let permit = s
            .copies
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::code(429, "capture_capacity_busy"))?;
        let svc = s.clone();
        let capture_key = key.clone();
        let (op, fresh) = blocking(move || svc.reserve_capture(&cid, &capture_key, &body)).await?;
        if fresh {
            let aid = string(&op["resource"], "id")?.to_owned();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                if let Err(error) = s.finish_capture(&aid, &key) {
                    tracing::error!(code = error.code, "artifact completion persistence failed");
                }
            });
        }
        return Ok((StatusCode::ACCEPTED, Json(super::retention::wire(op))).into_response());
    }
    if method == Method::POST && parts.first() == Some(&"leases") {
        let resource = if parts.len() == 1 {
            body["resource"].clone()
        } else {
            s.store.get("lease", parts[1])?["resource"].clone()
        };
        let kind = match resource["type"].as_str() {
            Some("artifact") => "artifact",
            Some("response_output") => "response",
            _ => return Err(Error::code(400, "invalid_argument")),
        };
        let record = s.store.get(kind, string(&resource, "id")?)?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        let path = parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        let status = if path.len() == 1 {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        return blocking(move || super::retention::lease(&s, &path, &key, &body))
            .await
            .map(|v| (status, Json(super::retention::wire(v))).into_response());
    }
    if method == Method::GET && parts.len() == 3 && parts[0] == "responses" && parts[2] == "events"
    {
        return super::events::stream(state, s, parts[1].to_owned());
    }
    if method == Method::GET
        && parts.len() == 3
        && parts[0] == "interactions"
        && parts[2] == "presentation"
    {
        let record = s.store.get("interaction", parts[1]).map_err(|e| {
            if e.status == 404 && !s.limits.mcp_turn_approval_enabled {
                Error::code(503, "inline_approval_disabled")
            } else {
                e
            }
        })?;
        let response = s.store.get("response", string(&record, "response_id")?)?;
        if super::approval_v06::selected(&response) {
            state
                .cwd_policy
                .validate(s.workspace_path(string(&record, "conversation_id")?)?)
                .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
            refresh_v06_catalog(&s, &state, &response).await?;
            let (audience, page, presentation_id) = super::approval_v06::query(uri.query())?;
            let iid = parts[1].to_owned();
            return blocking(move || {
                super::approval_v06::get(&s, &iid, &audience, page, presentation_id.as_deref())
            })
            .await
            .map(|v| Json(v).into_response());
        }
        if !s.limits.mcp_turn_approval_enabled {
            return Err(Error::code(503, "inline_approval_disabled"));
        }
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        super::presentations::refresh_catalog(&s, &state.runtime).await;
        let iid = parts[1].to_owned();
        return blocking(move || super::presentations::get(&s, &iid))
            .await
            .map(|v| Json(v).into_response());
    }
    if method == Method::GET
        && parts.len() == 3
        && parts[0] == "interactions"
        && parts[2] == "operation"
    {
        let record = s.store.get("interaction", parts[1])?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        let response = s.store.get("response", string(&record, "response_id")?)?;
        let value = if super::approval_v06::selected(&response) {
            refresh_v06_catalog(&s, &state, &response).await?;
            super::approval_v06::operation_details(&s, parts[1])?
        } else {
            super::mcp_grants::operation(&s, parts[1])?
        };
        return Ok(Json(value).into_response());
    }
    if method == Method::GET
        && parts.len() == 3
        && parts[0] == "responses"
        && parts[2] == "mcp-grants"
    {
        let record = s.store.get("response", parts[1])?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        refresh_v06_catalog(&s, &state, &record).await?;
        return Ok(Json(super::retention::wire(super::mcp_grants::list(
            &s, parts[1],
        )?))
        .into_response());
    }
    if method == Method::POST
        && parts.len() == 3
        && parts[0] == "mcp-grants"
        && parts[2] == "revoke"
    {
        fields(&body, &[])?;
        let record = s.store.get("mcp_grant", parts[1])?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record["scope"], "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(super::retention::wire(super::mcp_grants::revoke(
                &s, parts[1], &key,
            )?)),
        )
            .into_response());
    }
    if (parts.len() == 2 || parts.len() == 3) && parts[0] == "interactions" {
        let iid = parts[1].to_owned();
        let record = s.store.get("interaction", &iid)?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        if method == Method::GET && parts.len() == 2 {
            return blocking(move || super::interactions::read(&s, &iid))
                .await
                .map(|v| Json(super::retention::wire(v)).into_response());
        }
        if method == Method::POST && parts.len() == 3 && parts[2] == "reply" {
            // Own the send independently of the HTTP observer. An aborted HTTP
            // future must not discard a durably accepted reply.
            let result = tokio::spawn(async move {
                if record["state"] == "pending" && body["response"]["action"] == "accept" {
                    let response = s.store.get("response", string(&record, "response_id")?)?;
                    refresh_v06_catalog(&s, &state, &response).await?;
                }
                super::interactions::reply(&s, &state.runtime, &iid, &key, &body).await
            })
            .await
            .map_err(|_| Error::code(503, "store_unavailable"))??;
            return Ok((StatusCode::ACCEPTED, Json(super::retention::wire(result))).into_response());
        }
    }
    if method == Method::GET
        && parts.len() == 3
        && parts[0] == "responses"
        && parts[2] == "interactions"
    {
        let rid = parts[1].to_owned();
        let record = s.store.get("response", &rid)?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&record, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        return blocking(move || super::interactions::list(&s, &rid))
            .await
            .map(|v| Json(super::retention::wire(v)).into_response());
    }
    if method != Method::GET {
        return Err(Error::code(404, "resource_not_found"));
    }
    if parts.len() == 3 && parts[0] == "responses" && parts[2] == "generated-images" {
        let rid = parts[1].to_owned();
        let policy = state.cwd_policy.clone();
        let v = blocking(move || {
            let r = s.store.get("response", &rid)?;
            policy
                .validate(s.workspace_path(string(&r, "conversation_id")?)?)
                .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
            super::images::read(&s, &rid)
        })
        .await?;
        return Ok(Json(super::retention::wire(v)).into_response());
    }
    let owned = parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    if parts.len() == 3
        && ((parts[0] == "artifacts" && parts[2] == "content")
            || (parts[0] == "responses" && parts[2] == "output"))
    {
        let kind = if parts[0] == "artifacts" {
            "artifact"
        } else {
            "response"
        };
        let rid = parts[1].to_owned();
        let root = s.workspace_path(string(&s.store.get(kind, &rid)?, "conversation_id")?)?;
        state
            .cwd_policy
            .validate(root)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
        return super::download::content(s, kind, rid, headers).await;
    }
    if parts.as_slice() == ["workspaces"]
        || parts.len() == 3
            && matches!(parts[0], "conversations" | "workspaces")
            && parts[2] == "artifacts"
    {
        let query = uri.query().map(str::to_owned);
        let policy = state.cwd_policy.clone();
        let v = blocking(move || {
            super::listing::list_with_policy(&s, &owned, query.as_deref(), Some(&policy))
        })
        .await?;
        return Ok(Json(super::retention::wire(v)).into_response());
    }
    if parts.len() == 2 && parts[0] == "artifacts" {
        let a = s.store.get("artifact", parts[1])?;
        state
            .cwd_policy
            .validate(s.workspace_path(string(&a, "conversation_id")?)?)
            .map_err(|_| Error::code(403, "workspace_access_revoked"))?;
    }
    let v = blocking(move || read(&s, &owned)).await?;
    Ok(Json(super::retention::wire(v)).into_response())
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::code(503, "store_unavailable"))?
}
fn fields(v: &Value, allowed: &[&str]) -> Result<()> {
    let o = v
        .as_object()
        .ok_or_else(|| Error::code(400, "invalid_argument"))?;
    if o.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::code(422, "unknown_field"));
    }
    Ok(())
}
fn read(s: &Service, p: &[String]) -> Result<Value> {
    match p.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["operations", "by-key", key] => s.store.transaction(|tx| {
            store::operation(tx, key)?
                .map(store::public_operation)
                .ok_or_else(|| Error::code(404, "operation_not_found"))
        }),
        ["operations", id] => s.store.transaction(|tx| {
            let raw: String = tx
                .query_row("SELECT value FROM operations WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .map_err(|_| Error::code(404, "operation_not_found"))?;
            Ok(store::public_operation(serde_json::from_str(&raw)?))
        }),
        ["conversations", id] => {
            let mut c = s.store.get("conversation", id)?;
            c.as_object_mut().unwrap().remove("thread_id");
            c.as_object_mut().unwrap().remove("operation_key");
            c.as_object_mut()
                .unwrap()
                .retain(|key, _| !key.starts_with('_'));
            Ok(c)
        }
        ["responses", id] => super::approval_admission::bounded(
            super::approval_admission::public_response(s.store.get("response", id)?),
            262144,
        ),
        ["stops", id] => {
            let mut st = s.store.get("stop", id)?;
            if let Some(rid) = st["response_id"].as_str() {
                let r = s.store.get("response", rid)?;
                st["stop_status"] = json!(stop_status(&r));
                st["execution_status"] = r["execution_status"].clone();
                st["interrupt_delivery"] = r["interrupt_delivery"].clone();
            }
            Ok(st)
        }
        ["artifacts", id] => {
            let mut a = s.store.get("artifact", id)?;
            s.workspace_path(string(&a, "conversation_id")?)?;
            a.as_object_mut().unwrap().remove("source_path");
            Ok(a)
        }
        ["conversations", id, "artifacts"] => {
            s.store.get("conversation", id)?;
            let data: Vec<_> = s
                .store
                .list("artifact")?
                .into_iter()
                .filter(|r| r["conversation_id"] == *id)
                .map(|mut r| {
                    r.as_object_mut().unwrap().remove("source_path");
                    r
                })
                .collect();
            Ok(json!({ "data":data,"next_cursor":null}))
        }
        ["workspaces", id] => {
            let mut w = s.store.get("workspace", id)?;
            for k in ["path", "device", "inode"] {
                w.as_object_mut().unwrap().remove(k);
            }
            Ok(w)
        }
        ["leases", id] => s.store.get("lease", id),
        _ => Err(Error::code(404, "resource_not_found")),
    }
}

pub(crate) fn valid_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err(Error::code(400, "invalid_request_id"));
    }
    Ok(())
}

fn replay(s: &Service, key: &str, kind: &str, body: &Value) -> Result<Option<Value>> {
    s.store.transaction(|tx| {
        let Some(op) = store::operation(tx, key)? else {
            return Ok(None);
        };
        if op["fingerprint"] != crate::control::fingerprint(&json!({"kind":kind,"body":body})) {
            return Err(Error::code(409, "idempotency_conflict"));
        }
        Ok(Some(store::public_operation(op)))
    })
}

async fn refresh_v06_catalog(s: &Service, state: &AppState, response: &Value) -> Result<()> {
    if super::approval_v06::selected(response)
        && response["phase"] == "started"
        && response["stop_requested"] != true
        && let Some(key) = super::approval_v06::catalog_key(s, response)
    {
        let receiver = s
            .catalog
            .request(key.clone(), state.runtime.clone())
            .map_err(|code| Error::code(503, code))?;
        // The shared catalog retains Loading/Ready/Failed. Presentation/reply
        // re-read it inside their transaction, so a later failure or expiry
        // cannot be hidden by pinning an earlier successful wait result.
        super::catalog::Manager::view(receiver).await;
        if let Some(rid) = response["response_id"].as_str()
            && let Ok(current) = s.store.get("response", rid)
            && (current["phase"] != "started" || current["stop_requested"] == true)
        {
            s.catalog.release_thread(&key.runtime, &key.thread);
        }
    }
    Ok(())
}
