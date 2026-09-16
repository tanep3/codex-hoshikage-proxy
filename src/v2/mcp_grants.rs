//! Run-bound, explicit MCP tool grants. Arguments are held only in bounded memory.
use super::{
    Error, Result, now,
    service::{Service, string},
    store,
};
use rusqlite::Transaction;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub const MAX_RECORDS: usize = 256;
pub const SINGLE_BYTES: usize = 262_144;
pub const GRANTS_BYTES: usize = 1_048_576;
pub const LIST_BYTES: usize = 67_108_864;
pub fn bounded_response(value: Value, limit: usize) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() > limit {
        return Err(Error::code(503, "store_corrupt"));
    }
    Ok(value)
}
fn identifier(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 8192)
}
pub const TTL_MS: u64 = 600_000;
struct LeaseClock {
    wall: u64,
    tick: tokio::time::Instant,
}
impl LeaseClock {
    fn read(&mut self, wall: u64, tick: tokio::time::Instant) -> u64 {
        let elapsed = tick
            .saturating_duration_since(self.tick)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let projected = self.wall.saturating_add(elapsed);
        if wall > projected {
            self.wall = wall;
            self.tick = tick;
            wall
        } else {
            projected
        }
    }
}
pub struct State {
    lease_clock: LeaseClock,
    pub config_paths: Vec<PathBuf>,
    digest: String,
    generation: String,
    calls: BTreeMap<String, (u64, Value)>,
    poisoned: std::collections::BTreeSet<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            lease_clock: LeaseClock {
                wall: now(),
                tick: tokio::time::Instant::now(),
            },
            config_paths: vec![],
            digest: String::new(),
            generation: uuid::Uuid::new_v4().to_string(),
            calls: BTreeMap::new(),
            poisoned: Default::default(),
        }
    }
}
pub fn validate_context(v: Option<&Value>) -> Result<()> {
    let Some(v) = v else { return Ok(()) };
    if v.as_object().is_none_or(|o| o.len() != 3)
        || ["principal_id", "channel_id", "run_id"]
            .iter()
            .any(|k| v[k].as_str().is_none_or(|s| s.is_empty() || s.len() > 128))
    {
        return Err(Error::code(400, "invalid_approval_context"));
    }
    Ok(())
}
fn generation(s: &Service) -> Result<String> {
    let mut m = s.mcp.lock().unwrap();
    let mut parts = vec![];
    for p in &m.config_paths {
        let text =
            std::fs::read_to_string(p).map_err(|_| Error::code(503, "mcp_config_unavailable"))?;
        let v: toml::Value =
            toml::from_str(&text).map_err(|_| Error::code(503, "mcp_config_unavailable"))?;
        parts.push(v);
    }
    let hash = crate::control::fingerprint(&json!(parts));
    if m.digest != hash {
        m.digest = hash;
        m.generation = uuid::Uuid::new_v4().to_string();
    }
    Ok(m.generation.clone())
}
fn lease_now(s: &Service) -> u64 {
    s.mcp
        .lock()
        .unwrap()
        .lease_clock
        .read(now(), tokio::time::Instant::now())
}
fn call_key(thread: &Value, turn: &Value, item: &Value) -> String {
    crate::control::fingerprint(&json!([thread, turn, item]))
}
pub fn observe(s: &Service, e: &Value) -> Result<()> {
    if !s.limits.mcp_turn_approval_enabled {
        return Ok(());
    }
    let p = &e["params"];
    if e["method"] == "item/started" && p["item"]["type"] == "mcpToolCall" {
        let mut item = p["item"].clone();
        let key = call_key(&p["threadId"], &p["turnId"], &item["id"]);
        if serde_json::to_vec(&item)?.len() > 65536
            || !item["arguments"].is_object()
            || ["id", "server", "tool"]
                .iter()
                .any(|k| !identifier(&item[k]))
            || !identifier(&p["threadId"])
            || !identifier(&p["turnId"])
        {
            let mut m = s.mcp.lock().unwrap();
            m.calls.remove(&key);
            if m.poisoned.len() < MAX_RECORDS {
                m.poisoned.insert(key);
            } else {
                m.calls.clear();
            }
            return Ok(());
        }
        item["thread_id"] = p["threadId"].clone();
        item["turn_id"] = p["turnId"].clone();
        item["config_generation"] = json!(generation(s)?);
        let rows = s.store.list("response")?;
        let run = rows.iter().find(|r| {
            r["thread_id"] == p["threadId"]
                && r["turn_id"] == p["turnId"]
                && r["phase"] == "started"
        });
        item["input_generation"] = json!(
            run.map(|r| r["input_generation"].as_u64().unwrap_or(0))
                .unwrap_or(0)
        );
        item["binding_nonce"] = json!(uuid::Uuid::new_v4().to_string());
        let mut m = s.mcp.lock().unwrap();
        m.calls
            .retain(|_, (t, _)| now().saturating_sub(*t) < TTL_MS);

        if m.poisoned.contains(&key) {
            return Ok(());
        }
        if let Some((_, old)) = m.calls.get(&key) {
            if old["arguments"] != item["arguments"]
                || old["server"] != item["server"]
                || old["tool"] != item["tool"]
            {
                m.calls.remove(&key);
                if m.poisoned.len() < MAX_RECORDS {
                    m.poisoned.insert(key);
                } else {
                    m.calls.clear();
                }
            }
            return Ok(());
        }
        if m.calls.len() >= MAX_RECORDS || m.poisoned.len() >= MAX_RECORDS {
            return Ok(());
        }
        let nonce = item["binding_nonce"].clone();
        m.calls.insert(key.clone(), (now(), item));
        // Erase at the deadline even when this connection has no further events.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let cache = Arc::downgrade(&s.mcp);
            runtime.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(TTL_MS)).await;
                if let Some(cache) = cache.upgrade() {
                    let mut m = cache.lock().unwrap();
                    if m.calls
                        .get(&key)
                        .is_some_and(|(_, v)| v["binding_nonce"] == nonce)
                    {
                        m.calls.remove(&key);
                    }
                }
            });
        }
    }
    if e["method"] == "turn/completed" || e["kind"] == "transport_closed" {
        let closed = e["kind"] == "transport_closed";
        let turn = p
            .get("turnId")
            .or_else(|| p.pointer("/turn/id"))
            .unwrap_or(&Value::Null);
        s.mcp.lock().unwrap().calls.retain(|_, (_, v)| {
            !closed && !(v["thread_id"] == p["threadId"] && v["turn_id"] == *turn)
        });
        s.store.transaction(|tx| {
            for mut g in store::list(tx, "mcp_grant")? {
                let r = store::get(tx, "response", string(&g["scope"], "response_id")?)?;
                if (closed || (r["thread_id"] == p["threadId"] && g["scope"]["turn_id"] == *turn))
                    && matches!(
                        g["state"].as_str(),
                        Some("active" | "pending" | "suspended")
                    )
                {
                    g["state"] = json!("expired");
                    g["reason"] = json!("run_ended");
                    store::put(tx, "mcp_grant", string(&g, "grant_id")?, &g)?;
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}
/// Only the pinned native user-input adapter carries an authoritative item ID.
/// Arbitrary MCP form metadata cannot opt itself into this adapter.
pub fn adapt(s: &Service, method: &str, p: &Value) -> Result<Option<Value>> {
    if !s.limits.mcp_turn_approval_enabled || method != "item/tool/requestUserInput" {
        return Ok(None);
    }
    let key = call_key(&p["threadId"], &p["turnId"], &p["itemId"]);
    let item = s
        .mcp
        .lock()
        .unwrap()
        .calls
        .get(&key)
        .map(|(_, v)| v.clone());
    let Some(item) = item else { return Ok(None) };
    let Some(id) = p["itemId"].as_str() else {
        return Ok(None);
    };
    let q = &p["questions"];
    if q.as_array().is_none_or(|a| a.len() != 1)
        || q[0]["id"] != format!("mcp_tool_call_approval_{id}")
        || q[0]["isOther"] != false
        || q[0]["isSecret"] != false
    {
        return Ok(None);
    }
    let labels: Vec<_> = q[0]["options"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v["label"].as_str()).collect())
        .unwrap_or_default();
    if !labels.contains(&"Allow") || !labels.contains(&"Cancel") {
        return Ok(None);
    }
    Ok(Some(
        json!({"threadId":p["threadId"],"turnId":p["turnId"],"serverName":item["server"],"mode":"form","message":format!("Allow the {} MCP server to run tool \"{}\"?",item["server"].as_str().unwrap_or(""),item["tool"].as_str().unwrap_or("")),"requestedSchema":{"type":"object","properties":{}},"_meta":{"codex_approval_kind":"mcp_tool_call"},"native_question_id":q[0]["id"],"native_call_id":id,"native_config_generation":item["config_generation"]}),
    ))
}
fn unsafe_tool(tool: &str) -> bool {
    let name = tool.to_ascii_lowercase();
    name.contains("unsafe")
        || name.contains("evaluate")
        || name.contains("run_code")
        || name.contains("execute")
        || name.contains("eval")
}
fn scope(s: &Service, r: &Value, i: &Value) -> Value {
    json!({"instance_id":s.store.instance,"recovery_generation":s.store.generation,"context":r["approval_context"],"response_id":r["response_id"],"conversation_id":r["conversation_id"],"workspace_id":r["workspace_id"],"turn_id":i["turn_id"],"input_generation":r["input_generation"].as_u64().unwrap_or(0),"config_generation":i["operation"]["config_generation"],"server":i["operation"]["server"],"tool":i["operation"]["tool"]})
}
fn redact(v: &mut Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            for (k, v) in o {
                let p = format!("{path}/{}", k.replace('~', "~0").replace('/', "~1"));
                let lower = k.to_ascii_lowercase();
                if [
                    "token",
                    "secret",
                    "password",
                    "authorization",
                    "cookie",
                    "api_key",
                ]
                .iter()
                .any(|s| lower.contains(s))
                {
                    *v = json!("[REDACTED]");
                    out.push(p);
                } else {
                    redact(v, &p, out);
                }
            }
        }
        Value::Array(a) => {
            for (n, v) in a.iter_mut().enumerate() {
                redact(v, &format!("{path}/{n}"), out);
            }
        }
        _ => {}
    }
}
pub fn attach(s: &Service, r: &Value, i: &mut Value, p: &Value) -> Result<()> {
    if p["native_call_id"].is_null() {
        return Ok(());
    }
    let key = call_key(&i["thread_id"], &i["turn_id"], &p["native_call_id"]);
    let item = s
        .mcp
        .lock()
        .unwrap()
        .calls
        .get(&key)
        .map(|(_, v)| v.clone());
    let Some(item) = item else { return Ok(()) };
    let server = item["server"].as_str().unwrap_or("");
    let tool = item["tool"].as_str().unwrap_or("");
    let mut args = item["arguments"].clone();
    let mut redacted = vec![];
    redact(&mut args, "", &mut redacted);
    let reason = if unsafe_tool(tool) {
        Some("high_risk_tool")
    } else if !redacted.is_empty() {
        Some("redacted_arguments")
    } else if r["approval_context"].is_null() {
        Some("approval_context_required")
    } else if !s
        .limits
        .mcp_turn_grant_tools
        .get(server)
        .is_some_and(|tools| tools.iter().any(|t| t == tool))
    {
        Some("tool_not_allowlisted")
    } else {
        None
    };
    i["native_question_id"] = p["native_question_id"].clone();
    i["operation"] = json!({"binding_status":"verified","call_id":item["id"],"server":server,"tool":tool,"config_generation":p["native_config_generation"],"turn_grant_eligible":reason.is_none(),"ineligible_reason":reason,"disclosure":"requester_only","redacted_paths":redacted});
    let scope = scope(s, r, i);
    i["operation"]["scope"] = scope.clone();
    i["operation"]["scope"]["input_generation"] = item["input_generation"].clone();
    i["operation"]["scope_fingerprint"] = item["binding_nonce"].clone();
    // No arguments or native adapter fields in durable/public request payload.
    for key in [
        "native_question_id",
        "native_call_id",
        "native_config_generation",
    ] {
        i["request"].as_object_mut().unwrap().remove(key);
    }
    Ok(())
}
pub fn operation(s: &Service, iid: &str) -> Result<Value> {
    if !s.limits.mcp_turn_approval_enabled {
        return Err(Error::code(503, "operation_details_disabled"));
    }
    super::interactions::refresh(s)?;
    let i = s.store.get("interaction", iid)?;
    s.workspace_path(string(&i, "conversation_id")?)?;
    let mut out = json!({"interaction_id":iid,"response_id":i["response_id"],"turn_id":i["turn_id"],"revision":i["revision"],"call_id":null,"server":null,"tool":null,"binding_status":"unavailable","unavailable_reason":"stable_call_id_unavailable","config_generation":null,"input_generation":null,"scope":null,"scope_fingerprint":null,"turn_grant_eligible":false,"ineligible_reason":"binding_unavailable","arguments":null,"redacted_paths":[],"disclosure":"requester_only"});
    let Some(operation) = i["operation"].as_object() else {
        return Ok(out);
    };
    for (key, value) in operation {
        out[key] = value.clone();
    }
    out["input_generation"] = i["operation"]["scope"]["input_generation"].clone();
    out["unavailable_reason"] = Value::Null;
    if i["state"] == "pending" && i["expires_at_ms"].as_u64().unwrap_or(0) > now() {
        let key = call_key(&i["thread_id"], &i["turn_id"], &out["call_id"]);
        if let Some((_, item)) = s.mcp.lock().unwrap().calls.get(&key).filter(|(t, v)| {
            now().saturating_sub(*t) < TTL_MS && v["binding_nonce"] == out["scope_fingerprint"]
        }) {
            let mut args = item["arguments"].clone();
            let mut paths = vec![];
            redact(&mut args, "", &mut paths);
            out["arguments"] = args;
        }
    }
    if out["arguments"].is_null() {
        out["turn_grant_eligible"] = json!(false);
        out["unavailable_reason"] = json!("operation_details_expired");
    }
    if generation(s)? != out["config_generation"] {
        out["turn_grant_eligible"] = json!(false);
        out["ineligible_reason"] = json!("config_changed");
    }
    let r = s.store.get("response", string(&i, "response_id")?)?;
    if scope(s, &r, &i) != i["operation"]["scope"] {
        out["turn_grant_eligible"] = json!(false);
        out["ineligible_reason"] = json!("input_changed");
    }
    bounded_response(out, SINGLE_BYTES)
}
fn valid(s: &Service, g: &Value, r: &Value, generation: &str) -> bool {
    g["expires_at_ms"].as_u64().unwrap_or(0) > lease_now(s)
        && g["scope"]["config_generation"] == generation
        && r["phase"] == "started"
        && r["stop_requested"] != true
        && g["scope"]["response_id"] == r["response_id"]
        && g["scope"]["conversation_id"] == r["conversation_id"]
        && g["scope"]["workspace_id"] == r["workspace_id"]
        && g["scope"]["turn_id"] == r["turn_id"]
        && g["scope"]["input_generation"].as_u64().unwrap_or(0)
            == r["input_generation"].as_u64().unwrap_or(0)
        && g["scope"]["context"] == r["approval_context"]
}
pub fn refresh(s: &Service) -> Result<()> {
    let generation = generation(s).unwrap_or_default();
    s.store.transaction(|tx| {
        for mut g in store::list(tx, "mcp_grant")? {
            if matches!(
                g["state"].as_str(),
                Some("active" | "pending" | "suspended")
            ) {
                let r = store::get(tx, "response", string(&g["scope"], "response_id")?)?;
                if !valid(s, &g, &r, &generation) {
                    g["state"] = json!("expired");
                    g["reason"] = json!("scope_ended");
                    store::put(tx, "mcp_grant", string(&g, "grant_id")?, &g)?;
                }
            }
        }
        Ok(())
    })
}
pub fn create(
    s: &Service,
    tx: &Transaction<'_>,
    r: &Value,
    i: &mut Value,
    body: &Value,
) -> Result<()> {
    if body.get("grant_scope").is_none() {
        return Ok(());
    }
    if !s.limits.mcp_turn_approval_enabled {
        return Err(Error::code(503, "turn_approval_disabled"));
    }
    if body["grant_scope"] != "turn_tool"
        || body["response"]["action"] != "accept"
        || i["operation"]["turn_grant_eligible"] != true
    {
        return Err(Error::code(422, "turn_grant_ineligible"));
    }
    if body["expected_scope_fingerprint"] != i["operation"]["scope_fingerprint"]
        || scope(s, r, i) != i["operation"]["scope"]
        || generation(s)? != i["operation"]["config_generation"]
    {
        return Err(Error::code(409, "scope_conflict"));
    }
    let rid = string(r, "response_id")?;
    if store::list(tx, "mcp_grant")?
        .iter()
        .filter(|g| g["scope"]["response_id"] == rid)
        .count()
        >= 16
    {
        return Err(Error::code(429, "turn_grant_limit"));
    }
    let id = format!("grant_{}", uuid::Uuid::new_v4());
    let created_at = lease_now(s);
    let grant = json!({"grant_id":id,"scope":i["operation"]["scope"],"state":"pending","created_at_ms":created_at,"expires_at_ms":created_at+TTL_MS,"application_count":1,"initial_interaction_id":i["interaction_id"]});
    let mut all = store::list(tx, "mcp_grant")?
        .into_iter()
        .filter(|g| g["scope"]["response_id"] == rid)
        .collect::<Vec<_>>();
    all.push(grant.clone());
    bounded_response(json!({"response_id":rid,"data":all}), GRANTS_BYTES)?;
    store::put(tx, "mcp_grant", &id, &grant)?;
    i["grant_id"] = json!(id);
    Ok(())
}
pub fn finish(tx: &Transaction<'_>, i: &Value, written: bool) -> Result<()> {
    let Some(id) = i["grant_id"].as_str() else {
        return Ok(());
    };
    let mut g = store::get(tx, "mcp_grant", id)?;
    if g["state"] == "pending" {
        g["state"] = json!(if written { "active" } else { "suspended" });
    }
    if !written && g["state"] == "active" {
        g["state"] = json!("suspended");
    }
    store::put(tx, "mcp_grant", id, &g)
}
pub fn auto_grant(s: &Service, iid: &str) -> Result<Option<String>> {
    refresh(s)?;
    let i = s.store.get("interaction", iid)?;
    if i["operation"]["turn_grant_eligible"] != true {
        return Ok(None);
    }
    Ok(s.store
        .list("mcp_grant")?
        .into_iter()
        .find(|g| g["state"] == "active" && g["scope"] == i["operation"]["scope"])
        .and_then(|g| g["grant_id"].as_str().map(str::to_owned)))
}
pub fn apply(s: &Service, tx: &Transaction<'_>, r: &Value, i: &mut Value, id: &str) -> Result<()> {
    let mut g = store::get(tx, "mcp_grant", id)?;
    if g["state"] != "active"
        || !valid(s, &g, r, &generation(s)?)
        || g["scope"] != i["operation"]["scope"]
        || i["operation"]["turn_grant_eligible"] != true
    {
        return Err(Error::code(409, "grant_inactive"));
    }
    g["application_count"] = json!(g["application_count"].as_u64().unwrap_or(0) + 1);
    store::put(tx, "mcp_grant", id, &g)?;
    i["grant_id"] = json!(id);
    Ok(())
}
pub fn list(s: &Service, rid: &str) -> Result<Value> {
    refresh(s)?;
    let r = s.store.get("response", rid)?;
    s.workspace_path(string(&r, "conversation_id")?)?;
    bounded_response(
        json!({"response_id":rid,"data":s.store.list("mcp_grant")?.into_iter().filter(|g|g["scope"]["response_id"]==rid).collect::<Vec<_>>()}),
        GRANTS_BYTES,
    )
}
pub fn revoke(s: &Service, id: &str, key: &str) -> Result<Value> {
    s.store.transaction(|tx| {
        let (mut op, fresh) = store::reserve(tx, key, "mcp_grant.revoke", &json!({"grant_id":id}))?;
        if !fresh {
            return Ok(store::public_operation(op));
        }
        super::service::ensure_ready(tx)?;
        let mut g = store::get(tx, "mcp_grant", id)?;
        g["state"] = json!("revoked");
        g["reason"] = json!("operator_revoked");
        store::put(tx, "mcp_grant", id, &g)?;
        let count = store::list(tx, "interaction")?
            .iter()
            .filter(|i| {
                i["grant_id"] == id
                    && matches!(
                        i["state"].as_str(),
                        Some("sending" | "submitted" | "unknown")
                    )
            })
            .count();
        op["state"] = json!("succeeded");
        op["resource"] = json!({"type":"mcp_grant","id":id});
        op["in_flight_or_unknown_count"] = json!(count);
        store::save_operation(tx, &op)?;
        Ok(store::public_operation(op))
    })
}
pub fn steer(s: &Service, thread: &str, turn: &str) -> Result<()> {
    s.store.transaction(|tx| {
        for mut r in store::list(tx, "response")? {
            if r["thread_id"] == thread && r["turn_id"] == turn {
                r["input_generation"] = json!(r["input_generation"].as_u64().unwrap_or(0) + 1);
                store::put(tx, "response", string(&r, "response_id")?, &r)?;
            }
        }
        Ok(())
    })?;
    refresh(s)
}

pub fn check_scope(s: &Service, r: &Value, i: &Value) -> Result<()> {
    if i["native_question_id"].is_string() && scope(s, r, i) != i["operation"]["scope"] {
        return Err(Error::code(409, "scope_conflict"));
    }
    Ok(())
}

pub fn check_display(s: &Service, i: &Value, body: &Value) -> Result<()> {
    if let Some(expected) = body.get("expected_scope_fingerprint")
        && (!identifier(expected)
            || i["operation"]["binding_status"] != "verified"
            || expected != &i["operation"]["scope_fingerprint"])
    {
        return Err(Error::code(409, "scope_conflict"));
    }
    if i["native_question_id"].is_string() {
        if generation(s)? != i["operation"]["config_generation"] {
            return Err(Error::code(409, "scope_conflict"));
        }
        let key = call_key(&i["thread_id"], &i["turn_id"], &i["operation"]["call_id"]);
        if !s.mcp.lock().unwrap().calls.get(&key).is_some_and(|(t, v)| {
            now().saturating_sub(*t) < TTL_MS
                && v["binding_nonce"] == i["operation"]["scope_fingerprint"]
        }) {
            return Err(Error::code(409, "operation_binding_expired"));
        }
    }
    Ok(())
}

/// An immediately following call can precede acknowledgment of the first reply.
/// Wait only while its matching grant is pending; never infer successful delivery.
pub async fn await_auto_grant(s: &Service, iid: &str) -> Result<Option<String>> {
    loop {
        if let Some(id) = auto_grant(s, iid)? {
            return Ok(Some(id));
        }
        let i = s.store.get("interaction", iid)?;
        if i["state"] != "pending"
            || i["operation"]["turn_grant_eligible"] != true
            || i["expires_at_ms"].as_u64().unwrap_or(0) <= now()
            || !s
                .store
                .list("mcp_grant")?
                .iter()
                .any(|g| g["state"] == "pending" && g["scope"] == i["operation"]["scope"])
        {
            return Ok(None);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn wall_clock_rollback_cannot_extend_ttl_or_lose_submillisecond_progress() {
        let start = tokio::time::Instant::now();
        let mut c = LeaseClock {
            wall: 1_000_000,
            tick: start,
        };
        assert_eq!(c.read(1_000_000, start), 1_000_000);
        let expiry = 1_000_000 + TTL_MS;
        assert!(c.read(1, start + std::time::Duration::from_millis(TTL_MS - 1)) < expiry);
        assert_eq!(
            c.read(1, start + std::time::Duration::from_millis(TTL_MS)),
            expiry
        );
        let forward = start + std::time::Duration::from_millis(TTL_MS + 1);
        assert_eq!(c.read(9_000_000, forward), 9_000_000);
        for micros in [100, 200, 300, 900] {
            assert_eq!(
                c.read(1, forward + std::time::Duration::from_micros(micros)),
                9_000_000
            );
        }
        assert_eq!(
            c.read(1, forward + std::time::Duration::from_millis(1)),
            9_000_001
        );
    }
}
