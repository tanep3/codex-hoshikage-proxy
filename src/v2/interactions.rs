//! Durable, opt-in relay of App Server questions and permission requests.
//! Reply intent is committed before any transport write; unknown replies are never replayed.
use super::{
    Error, Result, now,
    service::{Service, ensure_ready, string},
    store,
};
use crate::runtime::CodexRuntime;
use rusqlite::Transaction;
use serde_json::{Value, json};
use std::collections::HashSet;

pub const MAX_BYTES: usize = 65536;
pub const MAX_COUNT: usize = 16;
pub const TIMEOUT_MS: u64 = 600_000;
pub const KINDS: [&str; 4] = ["user_input", "mcp_form", "mcp_url", "permissions"];

pub fn validate_capabilities(value: Option<&Value>) -> Result<()> {
    if let Some(value) = value {
        let a = value.as_array().ok_or_else(invalid)?;
        let mut seen = HashSet::new();
        if a.len() > KINDS.len() {
            return Err(invalid());
        }
        for v in a {
            let kind = v.as_str().ok_or_else(invalid)?;
            if !KINDS.contains(&kind) || !seen.insert(kind) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn invalid() -> Error {
    Error::code(400, "invalid_interaction_response")
}
fn unsupported() -> Error {
    Error::code(422, "unsupported_interaction_schema")
}
fn bounded(v: &Value) -> bool {
    serde_json::to_vec(v).is_ok_and(|b| b.len() <= MAX_BYTES)
}
fn fields(v: &Value, allowed: &[&str]) -> Result<()> {
    if v.as_object()
        .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn text(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 8192)
}

pub fn kind(method: &str, p: &Value) -> Option<&'static str> {
    match method {
        "item/tool/requestUserInput" | "tool/requestUserInput" => Some("user_input"),
        "item/permissions/requestApproval" => Some("permissions"),
        "mcpServer/elicitation/request" if p["mode"] == "form" => Some("mcp_form"),
        "mcpServer/elicitation/request" if p["mode"] == "url" => Some("mcp_url"),
        _ => None,
    }
}

fn validate_request(kind: &str, p: &Value) -> Result<()> {
    if !bounded(p) {
        return Err(unsupported());
    }
    match kind {
        "user_input" => {
            let questions = p["questions"].as_array().ok_or_else(unsupported)?;
            if questions.is_empty() || questions.len() > 3 {
                return Err(unsupported());
            }
            let mut ids = HashSet::new();
            for q in questions {
                if !text(&q["id"])
                    || !text(&q["question"])
                    || !q["header"].is_string()
                    || !ids.insert(q["id"].as_str().unwrap())
                {
                    return Err(unsupported());
                }
                for flag in ["isOther", "isSecret"] {
                    if q.get(flag).is_some_and(|v| !v.is_boolean()) {
                        return Err(unsupported());
                    }
                }
                if let Some(options) = q.get("options").filter(|v| !v.is_null()) {
                    let options = options.as_array().ok_or_else(unsupported)?;
                    let mut labels = HashSet::new();
                    if options.len() > 32 {
                        return Err(unsupported());
                    }
                    for o in options {
                        if !text(&o["label"]) || !labels.insert(o["label"].as_str().unwrap()) {
                            return Err(unsupported());
                        }
                    }
                }
            }
        }
        "mcp_form" => {
            if !text(&p["message"]) || !text(&p["serverName"]) {
                return Err(unsupported());
            }
            validate_form_schema(&p["requestedSchema"])?;
        }
        "mcp_url" => {
            if !text(&p["message"])
                || !text(&p["serverName"])
                || !text(&p["elicitationId"])
                || !p["url"].as_str().is_some_and(valid_url)
            {
                return Err(unsupported());
            }
        }
        "permissions" => {
            let permissions = &p["permissions"];
            fields(permissions, &["network", "fileSystem"]).map_err(|_| unsupported())?;
            for (key, allowed) in [
                ("network", &["enabled"][..]),
                (
                    "fileSystem",
                    &["read", "write", "entries", "globScanMaxDepth"][..],
                ),
            ] {
                if let Some(v) = permissions.get(key).filter(|v| !v.is_null()) {
                    fields(v, allowed).map_err(|_| unsupported())?;
                    if key == "network"
                        && v.get("enabled")
                            .is_some_and(|b| !b.is_null() && !b.is_boolean())
                    {
                        return Err(unsupported());
                    }
                    if key == "fileSystem" {
                        for k in ["read", "write"] {
                            if let Some(paths) = v.get(k).filter(|v| !v.is_null())
                                && paths.as_array().is_none_or(|a| {
                                    a.len() > 64
                                        || a.iter().any(|p| {
                                            !p.as_str().is_some_and(|s| {
                                                s.starts_with('/') && !s.contains('\0')
                                            })
                                        })
                                })
                            {
                                return Err(unsupported());
                            }
                        }
                        if let Some(entries) = v.get("entries").filter(|v| !v.is_null()) {
                            let entries = entries.as_array().ok_or_else(unsupported)?;
                            if entries.len() > 64 {
                                return Err(unsupported());
                            }
                            for entry in entries {
                                fields(entry, &["access", "path"]).map_err(|_| unsupported())?;
                                if !matches!(
                                    entry["access"].as_str(),
                                    Some("read" | "write" | "deny")
                                ) {
                                    return Err(unsupported());
                                }
                                validate_fs_path(&entry["path"])?;
                            }
                        }
                        if v.get("globScanMaxDepth").is_some_and(|v| {
                            !v.is_null() && v.as_u64().is_none_or(|n| n == 0 || n > 256)
                        }) {
                            return Err(unsupported());
                        }
                    }
                }
            }
        }
        _ => return Err(unsupported()),
    }
    Ok(())
}
fn validate_fs_path(p: &Value) -> Result<()> {
    match p["type"].as_str() {
        Some("path") => {
            fields(p, &["type", "path"]).map_err(|_| unsupported())?;
            if !p["path"]
                .as_str()
                .is_some_and(|s| s.starts_with('/') && !s.contains('\0'))
            {
                return Err(unsupported());
            }
        }
        Some("glob_pattern") => {
            fields(p, &["type", "pattern"]).map_err(|_| unsupported())?;
            if !text(&p["pattern"]) || p["pattern"].as_str().unwrap().contains('\0') {
                return Err(unsupported());
            }
        }
        Some("special") => {
            fields(p, &["type", "value"]).map_err(|_| unsupported())?;
            let v = &p["value"];
            if v["kind"] == "project_roots" {
                fields(v, &["kind", "subpath"]).map_err(|_| unsupported())?;
                if v.get("subpath")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                {
                    return Err(unsupported());
                }
            } else {
                fields(v, &["kind"]).map_err(|_| unsupported())?;
                if !matches!(
                    v["kind"].as_str(),
                    Some("root" | "minimal" | "tmpdir" | "slash_tmp")
                ) {
                    return Err(unsupported());
                }
            }
        }
        _ => return Err(unsupported()),
    }
    Ok(())
}
fn valid_url(s: &str) -> bool {
    reqwest::Url::parse(s).is_ok_and(|u| {
        matches!(u.scheme(), "https" | "http")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

// Bounded flat MCP form subset. Reject unknown validation keywords before
// advertising an interactive prompt; never fetch a remote $ref.
fn validate_form_schema(s: &Value) -> Result<()> {
    fields(
        s,
        &[
            "type",
            "properties",
            "required",
            "additionalProperties",
            "title",
            "description",
        ],
    )
    .map_err(|_| unsupported())?;
    if s["type"] != "object" || s.get("additionalProperties").is_some_and(|v| v != false) {
        return Err(unsupported());
    }
    let props = s["properties"].as_object().ok_or_else(unsupported)?;
    if props.len() > 32 {
        return Err(unsupported());
    }
    if let Some(required) = s.get("required") {
        let a = required.as_array().ok_or_else(unsupported)?;
        let mut seen = HashSet::new();
        for key in a {
            let key = key.as_str().ok_or_else(unsupported)?;
            if !props.contains_key(key) || !seen.insert(key) {
                return Err(unsupported());
            }
        }
    }
    for schema in props.values() {
        validate_scalar_schema(schema)?;
    }
    Ok(())
}
fn validate_scalar_schema(s: &Value) -> Result<()> {
    fields(
        s,
        &[
            "type",
            "title",
            "description",
            "default",
            "enum",
            "enumNames",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
        ],
    )
    .map_err(|_| unsupported())?;
    if !matches!(
        s["type"].as_str(),
        Some("string" | "integer" | "number" | "boolean")
    ) {
        return Err(unsupported());
    }
    for k in ["minimum", "maximum"] {
        if let Some(v) = s.get(k)
            && (!matches!(s["type"].as_str(), Some("integer" | "number"))
                || !v
                    .as_f64()
                    .is_some_and(|n| n.abs() <= 9_007_199_254_740_991.0))
        {
            return Err(unsupported());
        }
    }
    for k in ["minLength", "maxLength"] {
        if let Some(v) = s.get(k)
            && (s["type"] != "string" || v.as_u64().is_none_or(|n| n > 8192))
        {
            return Err(unsupported());
        }
    }
    if let Some(e) = s.get("enum")
        && e.as_array()
            .is_none_or(|a| a.is_empty() || a.len() > 64 || a.iter().any(|v| !scalar_type(s, v)))
    {
        return Err(unsupported());
    }
    if s.get("enumNames").is_some_and(|v| {
        v.as_array().is_none_or(|a| {
            a.len() != s["enum"].as_array().map_or(0, Vec::len) || a.iter().any(|v| !v.is_string())
        })
    }) {
        return Err(unsupported());
    }
    if s["minimum"]
        .as_f64()
        .zip(s["maximum"].as_f64())
        .is_some_and(|(a, b)| a > b)
        || s["minLength"]
            .as_u64()
            .zip(s["maxLength"].as_u64())
            .is_some_and(|(a, b)| a > b)
    {
        return Err(unsupported());
    }
    Ok(())
}
fn scalar_type(s: &Value, v: &Value) -> bool {
    if v.is_number()
        && !v
            .as_f64()
            .is_some_and(|n| n.abs() <= 9_007_199_254_740_991.0)
    {
        return false;
    }
    match s["type"].as_str() {
        Some("string") => v.is_string(),
        Some("integer") => v.is_i64() || v.is_u64(),
        Some("number") => v.is_number(),
        Some("boolean") => v.is_boolean(),
        _ => false,
    }
}
fn validate_scalar(s: &Value, v: &Value) -> Result<()> {
    if !scalar_type(s, v) || s["enum"].as_array().is_some_and(|a| !a.contains(v)) {
        return Err(invalid());
    }
    if let Some(n) = v.as_f64()
        && (s["minimum"].as_f64().is_some_and(|min| n < min)
            || s["maximum"].as_f64().is_some_and(|max| n > max))
    {
        return Err(invalid());
    }
    if let Some(v) = v.as_str() {
        let len = v.chars().count() as u64;
        if v.len() > 8192
            || s["minLength"].as_u64().is_some_and(|n| len < n)
            || s["maxLength"].as_u64().is_some_and(|n| len > n)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn validate_reply(i: &Value, reply: &Value) -> Result<()> {
    if !bounded(reply) {
        return Err(invalid());
    }
    let p = &i["request"];
    match i["kind"].as_str() {
        Some("user_input") => {
            fields(reply, &["answers"])?;
            let answers = reply["answers"].as_object().ok_or_else(invalid)?;
            let questions = p["questions"].as_array().ok_or_else(invalid)?;
            if answers.len() != questions.len() {
                return Err(invalid());
            }
            for q in questions {
                let answer = answers
                    .get(q["id"].as_str().ok_or_else(invalid)?)
                    .ok_or_else(invalid)?;
                fields(answer, &["answers"])?;
                let values = answer["answers"].as_array().ok_or_else(invalid)?;
                if values.len() != 1 || !text(&values[0]) {
                    return Err(invalid());
                }
                if q["isOther"] != true
                    && q["options"].as_array().is_some_and(|a| {
                        !a.is_empty() && !a.iter().any(|o| o["label"] == values[0])
                    })
                {
                    return Err(invalid());
                }
            }
        }
        Some("mcp_form" | "mcp_url") => {
            fields(reply, &["action", "content"])?;
            match reply["action"].as_str() {
                Some("cancel" | "decline") if reply.get("content").is_some_and(Value::is_null) => {}
                Some("accept")
                    if i["kind"] == "mcp_url"
                        && reply.get("content").is_some_and(Value::is_null) => {}
                Some("accept") if i["kind"] == "mcp_form" => {
                    let content = reply["content"].as_object().ok_or_else(invalid)?;
                    let schema = &p["requestedSchema"];
                    let props = schema["properties"].as_object().ok_or_else(invalid)?;
                    for (k, v) in content {
                        validate_scalar(props.get(k).ok_or_else(invalid)?, v)?;
                    }
                    if let Some(required) = schema["required"].as_array()
                        && required
                            .iter()
                            .any(|k| !content.contains_key(k.as_str().unwrap_or("")))
                    {
                        return Err(invalid());
                    }
                }
                _ => return Err(invalid()),
            }
        }
        Some("permissions") => {
            fields(reply, &["permissions", "scope", "strictAutoReview"])?;
            if reply
                .get("scope")
                .is_some_and(|v| v != "turn" && v != "session")
                || reply.get("strictAutoReview").is_some_and(|v| v != true)
            {
                return Err(invalid());
            }
            fields(&reply["permissions"], &["network", "fileSystem"])?;
            for key in ["network", "fileSystem"] {
                if let Some(grant) = reply["permissions"].get(key).filter(|v| !v.is_null()) {
                    // Whole-category selection preserves all requested constraints.
                    if grant != &p["permissions"][key] {
                        return Err(invalid());
                    }
                }
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

/// Returns false for requests not owned by this relay (legacy clients included).
pub fn receive(s: &Service, rpc_id: &Value, method: &str, p: &Value) -> Result<bool> {
    let Some(kind) = kind(method, p) else {
        return Ok(false);
    };
    let Some(thread) = p["threadId"].as_str() else {
        return Ok(false);
    };
    s.store.transaction(|tx| {
        let matches: Vec<_> = store::list(tx,"response")?.into_iter().filter(|r| r["thread_id"]==thread && matches!(r["phase"].as_str(),Some("dispatching" | "started"))).collect();
        if matches.len()!=1 { return Ok(false); }
        let r = &matches[0];
        if !r["interaction_capabilities"].as_array().is_some_and(|a|a.iter().any(|v|v==kind)) { return Ok(false); }
        ensure_ready(tx)?;
        if r["stop_requested"]==true { return Err(Error::code(409,"interaction_closed")); }
        let turn = p["turnId"].as_str().or_else(||r["turn_id"].as_str()).ok_or_else(||Error::code(409,"interaction_binding_pending"))?;
        if r["turn_id"].as_str().is_some_and(|id|id!=turn) { return Err(Error::code(409,"target_mismatch")); }
        validate_request(kind,p)?;
        let rid = string(r,"response_id")?;
        let iid = format!("int_{}",crate::control::fingerprint(&json!([rid,turn,rpc_id])));
        let fingerprint=crate::control::fingerprint(&json!([method,p]));
        if let Ok(existing)=store::get(tx,"interaction",&iid) {
            if existing["request_fingerprint"]!=fingerprint {return Err(Error::code(409,"interaction_request_conflict"));}
            if existing["state"]!="pending" {return Err(Error::code(409,"interaction_closed"));}
            return Ok(true);
        }
        if store::list(tx,"interaction")?.iter().filter(|i| i["response_id"]==rid).count()>=MAX_COUNT { return Err(Error::code(429,"interaction_limit_exceeded")); }
        let mut request=p.clone();
        for k in ["threadId","turnId"] { request.as_object_mut().unwrap().remove(k); }
        let i=json!({"interaction_id":iid,"response_id":rid,"conversation_id":r["conversation_id"],"workspace_id":r["workspace_id"],
            "thread_id":thread,"turn_id":turn,"rpc_id":rpc_id,"request_fingerprint":fingerprint,"kind":kind,"state":"pending","revision":1,
            "created_at_ms":now(),"expires_at_ms":now()+TIMEOUT_MS,"request":request,"reply_status":"not_sent","error":null});
        store::put(tx,"interaction",&iid,&i)?;
        update_wait(tx, rid)?;
        Ok(true)
    })
}
fn public(mut i: Value) -> Value {
    for key in [
        "rpc_id",
        "thread_id",
        "turn_id",
        "reply_key",
        "request_fingerprint",
    ] {
        i.as_object_mut().unwrap().remove(key);
    }
    if i["state"] != "pending" || i["expires_at_ms"].as_u64().is_some_and(|t| t <= now()) {
        i["request"] = Value::Null;
    }
    i
}
pub fn read(s: &Service, iid: &str) -> Result<Value> {
    refresh(s)?;
    let i = s.store.get("interaction", iid)?;
    s.workspace_path(string(&i, "conversation_id")?)?;
    Ok(public(i))
}
pub fn list(s: &Service, rid: &str) -> Result<Value> {
    refresh(s)?;
    let r = s.store.get("response", rid)?;
    s.workspace_path(string(&r, "conversation_id")?)?;
    let mut data = s
        .store
        .list("interaction")?
        .into_iter()
        .filter(|i| i["response_id"] == rid)
        .map(public)
        .collect::<Vec<_>>();
    data.sort_by_key(|i| {
        (
            i["created_at_ms"].as_u64().unwrap_or(0),
            i["interaction_id"].as_str().unwrap_or("").to_owned(),
        )
    });
    Ok(json!({"response_id":rid,"data":data}))
}
fn change(tx: &Transaction<'_>, i: &mut Value, state: &str, reason: &str) -> Result<()> {
    i["state"] = json!(state);
    i["revision"] = json!(i["revision"].as_u64().unwrap_or(0) + 1);
    i["request"] = Value::Null;
    i["resolution_reason"] = json!(reason);
    i["error"] = if matches!(state, "sending" | "submitted" | "resolved") {
        Value::Null
    } else {
        json!({"code":reason})
    };
    store::put(tx, "interaction", string(i, "interaction_id")?, i)?;
    update_wait(tx, string(i, "response_id")?)
}

fn update_wait(tx: &Transaction<'_>, rid: &str) -> Result<()> {
    let until = store::list(tx, "interaction")?
        .iter()
        .filter(|i| i["response_id"] == rid && i["state"] == "pending")
        .filter_map(|i| i["expires_at_ms"].as_u64())
        .max()
        .unwrap_or(0);
    let mut r = store::get(tx, "response", rid)?;
    r["interaction_wait_until_ms"] = json!(until);
    r["interactions_revision"] = json!(r["interactions_revision"].as_u64().unwrap_or(0) + 1);
    store::put(tx, "response", rid, &r)
}
/// Refresh applies expiry and authoritative stop/terminal state in the same DB
/// transaction as replies use. It never treats a submitted reply as execution success.
pub fn refresh(s: &Service) -> Result<()> {
    s.store.transaction(|tx| {
        for mut i in store::list(tx, "interaction")? {
            if i["state"] != "pending" {
                continue;
            }
            let mut r = store::get(tx, "response", string(&i, "response_id")?)?;
            if r["stop_requested"] == true
                || !matches!(r["phase"].as_str(), Some("dispatching" | "started"))
            {
                change(tx, &mut i, "cancelled", "interaction_closed")?;
            } else if i["expires_at_ms"].as_u64().unwrap_or(0) <= now() {
                change(tx, &mut i, "expired", "interaction_expired")?;
                r = store::get(tx, "response", string(&i, "response_id")?)?;
                r["stop_requested"] = json!(true);
                r["error"] = json!({"code":"interaction_expired"});
                store::put(tx, "response", string(&r, "response_id")?, &r)?;
            }
        }
        Ok(())
    })
}

pub fn event_loss(s: &Service) -> Result<()> {
    s.store.transaction(|tx| {
        for mut r in store::list(tx, "response")? {
            if matches!(r["phase"].as_str(), Some("dispatching" | "started"))
                && r["interaction_capabilities"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
            {
                r["stop_requested"] = json!(true);
                r["error"] = json!({"code":"interaction_events_lost"});
                store::put(tx, "response", string(&r, "response_id")?, &r)?;
            }
        }
        Ok(())
    })?;
    refresh(s)
}
pub fn observe(s: &Service, event: &Value) -> Result<()> {
    let closed = event["kind"] == "transport_closed";
    if !closed && event["method"] != "serverRequest/resolved" && event["method"] != "turn/completed"
    {
        return Ok(());
    }
    s.store.transaction(|tx| {
        for mut i in store::list(tx, "interaction")? {
            if !matches!(
                i["state"].as_str(),
                Some("pending" | "sending" | "submitted")
            ) {
                continue;
            }
            let p = &event["params"];
            let matches = closed
                || (p["threadId"] == i["thread_id"]
                    && ((event["method"] == "serverRequest/resolved"
                        && p["requestId"] == i["rpc_id"])
                        || (event["method"] == "turn/completed"
                            && p.get("turnId").or_else(|| p.pointer("/turn/id"))
                                == Some(&i["turn_id"]))));
            if matches {
                let uncertain = closed && i["reply_status"] != "not_sent";
                if uncertain {
                    i["reply_status"] = json!("unknown");
                }
                change(
                    tx,
                    &mut i,
                    if uncertain {
                        "unknown"
                    } else if closed {
                        "cancelled"
                    } else {
                        "resolved"
                    },
                    if closed {
                        "interaction_connection_lost"
                    } else {
                        "interaction_resolved"
                    },
                )?;
            }
        }
        Ok(())
    })
}
pub fn recover(tx: &Transaction<'_>) -> Result<()> {
    for mut i in store::list(tx, "interaction")? {
        if matches!(
            i["state"].as_str(),
            Some("pending" | "sending" | "submitted")
        ) {
            let uncertain = i["reply_status"] != "not_sent";
            if uncertain {
                i["reply_status"] = json!("unknown");
            }
            change(
                tx,
                &mut i,
                if uncertain { "unknown" } else { "cancelled" },
                "interaction_connection_lost",
            )?;
        }
        if let Some(key) = i["reply_key"].as_str()
            && let Some(mut op) = store::operation(tx, key)?
            && op["state"] == "accepted"
        {
            op["state"] = json!("unknown");
            store::save_operation(tx, &op)?;
        }
    }
    Ok(())
}

pub async fn reply(
    s: &Service,
    runtime: &CodexRuntime,
    iid: &str,
    key: &str,
    body: &Value,
) -> Result<Value> {
    fields(body, &["expected_revision", "response"])?;
    if !bounded(body) {
        return Err(invalid());
    }
    refresh(s)?;
    let (op, fresh, rpc) = s.store.transaction(|tx| {
        let i = store::get(tx, "interaction", iid)?;
        let (mut op, fresh) = store::reserve(
            tx,
            key,
            "interaction.reply",
            &json!({"interaction_id":iid,"request":body}),
        )?;
        if !fresh {
            return Ok((op, false, Value::Null));
        }
        ensure_ready(tx)?;
        let r = store::get(tx, "response", string(&i, "response_id")?)?;
        super::retention::check_access(tx, &i)?;
        let c = store::get(tx, "conversation", string(&i, "conversation_id")?)?;
        if c["state"] != "ready"
            || c["active_response_id"] != i["response_id"]
            || r["stop_requested"] == true
            || i["state"] != "pending"
            || i["expires_at_ms"].as_u64().unwrap_or(0) <= now()
        {
            return Err(Error::code(409, "interaction_closed"));
        }
        if r["phase"] != "started" || r["turn_id"] != i["turn_id"] {
            return Err(Error::code(409, "interaction_binding_pending"));
        }
        if body["expected_revision"].as_u64().is_none()
            || body["expected_revision"] != i["revision"]
        {
            return Err(Error::code(409, "revision_conflict"));
        }
        validate_reply(&i, &body["response"])?;
        let rpc = i["rpc_id"].clone();
        let mut i = i;
        i["reply_key"] = json!(key);
        i["reply_status"] = json!("unknown");
        change(tx, &mut i, "sending", "interaction_reply_pending")?;
        op["resource"] = json!({"type":"interaction","id":iid});
        store::save_operation(tx, &op)?;
        Ok((op, true, rpc))
    })?;
    if !fresh {
        return Ok(store::public_operation(op));
    }
    // Cancellation of the HTTP future must not cancel an accepted reply. The API
    // runs this function in an owned task. No retry after any write attempt.
    let written = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        runtime.respond_to_server_request(rpc, body["response"].clone()),
    )
    .await
    .is_ok_and(|r| r.is_ok());
    s.store.transaction(|tx| {
        let mut op = store::operation(tx, key)?.ok_or_else(|| Error::code(503, "store_corrupt"))?;
        let mut i = store::get(tx, "interaction", iid)?;
        i["reply_status"] = json!(if written { "written" } else { "unknown" });
        if i["state"] == "sending" {
            change(
                tx,
                &mut i,
                if written { "submitted" } else { "unknown" },
                if written {
                    "interaction_reply_written"
                } else {
                    "interaction_reply_unknown"
                },
            )?;
        } else {
            i["revision"] = json!(i["revision"].as_u64().unwrap_or(0) + 1);
            store::put(tx, "interaction", iid, &i)?;
            update_wait(tx, string(&i, "response_id")?)?;
        }
        op["state"] = json!(if written { "succeeded" } else { "unknown" });
        store::save_operation(tx, &op)?;
        Ok(store::public_operation(op))
    })
}
