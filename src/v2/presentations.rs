//! Source-conversation approval displays. Never persist display text or arguments.
use super::{
    Error, Result, id, interactions, mcp_grants, retention,
    service::{Service, string},
    store,
};
use crate::runtime::CodexRuntime;
use hmac::{Hmac, Mac};
use rusqlite::Transaction;
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 32768;
pub const MAX_TEXT: usize = 1400;
pub const MAX_VERSIONS: u64 = 4;
pub const PROFILE: &str = "source-conversation-v1";

pub struct State {
    pub(crate) key: [u8; 32],
    catalog_generation: String,
    catalog_epoch: String,
    tools: BTreeMap<(String, String), String>,
}
impl Default for State {
    fn default() -> Self {
        // UUID v4 supplies independent OS randomness; HMAC key never leaves this process.
        let mut key = [0; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            key,
            catalog_generation: String::new(),
            catalog_epoch: uuid::Uuid::new_v4().to_string(),
            tools: BTreeMap::new(),
        }
    }
}
pub fn capability(enabled: bool) -> Value {
    json!({"enabled":enabled,"profile":PROFILE,"max_response_bytes":MAX_BYTES,
        "max_display_text_utf16_units":MAX_TEXT,"max_display_fields":8,"max_presentations_per_interaction":MAX_VERSIONS})
}
pub fn validate_request(body: &Value) -> Result<()> {
    if let Some(p) = body.get("approval_presentation")
        && (p.as_object().is_none_or(|v| v.len() != 1)
            || p["mode"] != "source_conversation"
            || !body["approval_context"].is_object())
    {
        return Err(Error::code(400, "invalid_approval_presentation"));
    }
    Ok(())
}
fn declared(r: &Value) -> Result<()> {
    if r["approval_presentation"]["mode"] != "source_conversation"
        || !r["approval_context"]["channel_id"].is_string()
    {
        return Err(Error::code(409, "presentation_context_mismatch"));
    }
    Ok(())
}
pub fn validate_reply(body: &Value) -> Result<()> {
    let supplied = body.get("approval_view").is_some()
        || body.get("expected_presentation_fingerprint").is_some();
    let valid = body["approval_view"] == "source_conversation"
        && body["expected_presentation_fingerprint"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 8192)
        && body["expected_scope_fingerprint"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && body["response"]["action"] == "accept";
    if supplied && !valid {
        return Err(Error::code(400, "invalid_approval_presentation"));
    }
    Ok(())
}
// Verify the live configured connection's catalog, not an arbitrary tool-name match.
// Remote implementations changing without updating their definition remain an operator trust boundary.
pub async fn refresh_catalog(s: &Service, runtime: &CodexRuntime) {
    let Ok(generation) = mcp_grants::generation(s) else {
        s.presentations.lock().unwrap().tools.clear();
        return;
    };
    let mut pages = vec![];
    let mut cursor = Value::Null;
    let mut seen = std::collections::BTreeSet::new();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for _ in 0..16 {
            let mut args = json!({"limit":100});
            if !cursor.is_null() {
                args["cursor"] = cursor.clone();
            }
            let v = runtime.request("mcpServerStatus/list", args).await.ok()?;
            if serde_json::to_vec(&v).ok()?.len() > 1_048_576 {
                return None;
            }
            pages.extend(v["data"].as_array()?.iter().cloned());
            cursor = v["nextCursor"].clone();
            if cursor.is_null() {
                return Some(json!({"data":pages}));
            }
            if !seen.insert(cursor.as_str()?.to_owned()) {
                return None;
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(Value::Null);
    if mcp_grants::generation(s).ok().as_deref() != Some(&generation) {
        return;
    }
    register_catalog(s, &generation, &result);
}
/// Catalog registration is generation-bound and used by the upstream adapter and isolated tests.
pub fn register_catalog(s: &Service, generation: &str, catalog: &Value) {
    let _gate = s.approval_gate.lock().unwrap();
    let mut accepted = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    if let Some(servers) = catalog["data"].as_array() {
        for server in servers {
            let Some(name) = server["name"].as_str() else {
                continue;
            };
            // Only the evaluated Playwright adapter; identically named tools on other servers aren't inferred.
            if name != "playwright" {
                continue;
            }
            if !seen.insert(name) {
                accepted.clear();
                break;
            }
            if let Some(tools) = server["tools"].as_object() {
                for (tool, definition) in tools {
                    if schema_matches(tool, definition) {
                        accepted.insert(
                            (name.to_owned(), tool.clone()),
                            crate::control::fingerprint(definition),
                        );
                    }
                }
            }
        }
    }
    // Serialize definition changes with display issuance and reply intent transactions.
    let result = s.store.transaction(|_| {
        let mut state = s.presentations.lock().unwrap();
        if state.catalog_generation != generation || state.tools != accepted {
            state.catalog_epoch = uuid::Uuid::new_v4().to_string();
        }
        state.catalog_generation = generation.to_owned();
        state.tools = accepted;
        Ok(())
    });
    if result.is_err() {
        let mut state = s.presentations.lock().unwrap();
        state.tools.clear();
        state.catalog_epoch = uuid::Uuid::new_v4().to_string();
    }
}
fn schema_matches(tool: &str, definition: &Value) -> bool {
    let schema = &definition["inputSchema"];
    if definition["name"] != tool || schema["type"] != "object" {
        return false;
    }
    let Some(props) = schema["properties"].as_object() else {
        return false;
    };
    let required: Vec<_> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match tool {
        "browser_find" => {
            props.len() == 2
                && props.get("text").is_some_and(|v| v["type"] == "string")
                && props.get("regex").is_some_and(|v| v["type"] == "string")
                && required.is_empty()
        }
        "browser_navigate" => {
            props.len() == 1
                && props.get("url").is_some_and(|v| v["type"] == "string")
                && required == ["url"]
        }
        "browser_tabs" => {
            props.len() == 3
                && props.get("action").is_some_and(|v| {
                    v["type"] == "string" && v["enum"] == json!(["list", "new", "close", "select"])
                })
                && props
                    .get("index")
                    .is_some_and(|v| v["type"] == "number" || v["type"] == "integer")
                && props.get("url").is_some_and(|v| v["type"] == "string")
                && required == ["action"]
        }
        _ => false,
    }
}
fn auth_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase().replace('-', "_");
    [
        "authorization",
        "proxy_authorization",
        "cookie",
        "set_cookie",
        "password",
        "passwd",
        "secret",
        "client_secret",
        "token",
        "access_token",
        "refresh_token",
        "id_token",
        "api_key",
        "apikey",
        "credential",
        "signature",
        "sig",
        "session",
        "sessionid",
        "code",
    ]
    .contains(&n.as_str())
        || n.starts_with("x_amz_")
        || n.starts_with("x_goog_")
}
fn percent_decode(text: &str) -> Option<String> {
    let mut out = vec![];
    let bytes = text.as_bytes();
    let mut n = 0;
    while n < bytes.len() {
        if bytes[n] == b'%' {
            let a = (*bytes.get(n + 1)? as char).to_digit(16)?;
            let b = (*bytes.get(n + 2)? as char).to_digit(16)?;
            out.push((a * 16 + b) as u8);
            n += 3;
        } else {
            out.push(bytes[n]);
            n += 1;
        }
    }
    String::from_utf8(out).ok()
}
fn unsafe_text(text: &str) -> bool {
    let mut decoded = text.to_owned();
    for _ in 0..2 {
        if suspicious(&decoded) {
            return true;
        }
        if !decoded.contains('%') {
            return false;
        }
        let Some(next) = percent_decode(&decoded) else {
            return true;
        };
        decoded = next;
    }
    suspicious(&decoded) || decoded.contains('%')
}
fn suspicious(text: &str) -> bool {
    if text.chars().any(|c|c.is_control()||matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')) {return true;}
    // A URL may appear in a search query too; userinfo is never a public target.
    for (_, tail) in text.match_indices("://").map(|(n, _)| (n, &text[n + 3..])) {
        if tail
            .split(['/', '?', '#', ' ', '\t', '\n'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
        {
            return true;
        }
    }
    if text.split('&').skip(1).any(|part| {
        part.split_once(';').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')
        })
    }) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if [
        "bearer ",
        "basic ",
        "-----begin ",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "&#",
        "&amp;",
        "&quot;",
        "&lt;",
        "&gt;",
        "\\u",
        "\\x",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        return true;
    }
    let words: Vec<_> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .filter(|s| !s.is_empty())
        .collect();
    for word in words {
        if auth_name(word) {
            // Require assignment in free text to avoid treating ordinary words as credentials.
            for (pos, _) in lower.match_indices(word) {
                if lower[pos + word.len()..]
                    .trim_start_matches([' ', '\t', '"', '\''])
                    .starts_with([':', '='])
                {
                    return true;
                }
            }
        }
    }
    for part in text.split(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c))) {
        let chunks: Vec<_> = part.split('.').collect();
        if chunks.len() == 3 && chunks[0].starts_with("eyJ") && chunks.iter().all(|s| !s.is_empty())
        {
            return true;
        }
    }
    false
}
fn safe_url(raw: &str) -> bool {
    if unsafe_text(raw) || raw.chars().any(char::is_whitespace) || raw.contains('\\') {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let Some((_, tail)) = raw.split_once("://") else {
        return false;
    };
    if tail
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|s| s.contains('@'))
    {
        return false;
    }
    let mut value = raw.to_owned();
    for _ in 0..=2 {
        if value.contains("/../")
            || value.contains("/./")
            || value.ends_with("/..")
            || value.ends_with("/.")
        {
            return false;
        }
        let Ok(parsed) = reqwest::Url::parse(&value) else {
            return false;
        };
        for (k, v) in parsed.query_pairs() {
            if auth_name(&k) || v.contains("://") || v.contains("%") {
                return false;
            }
        }
        if let Some(fragment) = parsed.fragment() {
            for pair in fragment.split('&') {
                if auth_name(pair.split('=').next().unwrap_or("")) || pair.contains("://") {
                    return false;
                }
            }
        }
        if !value.contains('%') {
            break;
        }
        let Some(next) = percent_decode(&value) else {
            return false;
        };
        value = next;
    }
    true
}
fn render(tool: &str, args: &Value) -> std::result::Result<Value, &'static str> {
    let Some(o) = args.as_object() else {
        return Err("unknown_arguments");
    };
    let field = |label: &str, value: &str| json!({"label":label,"value":value});
    let (renderer, title, fields, limitations) = match tool {
        "browser_find" => {
            if o.len() != 1 || !o.keys().all(|k| k == "text" || k == "regex") {
                return Err("unknown_arguments");
            }
            let (key, value) = o.iter().next().unwrap();
            let text = value
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("unknown_arguments")?;
            if unsafe_text(text) {
                return Err("private_arguments");
            }
            let regex = key == "regex";
            (
                "browser-find-v1",
                if regex {
                    "ページ内を正規表現で検索"
                } else {
                    "ページ内を検索"
                },
                vec![
                    field(
                        "操作",
                        if regex {
                            "現在のページのアクセシビリティ情報を正規表現検索します"
                        } else {
                            "現在のページのアクセシビリティ情報を文字列検索します"
                        },
                    ),
                    field("対象", "このMCP接続で現在開いているページ"),
                    field(if regex { "正規表現" } else { "検索語" }, text),
                ],
                vec![
                    "現在のページのURLは引数になく、並列操作で対象ページが変わる場合があります",
                    "操作内容の説明であり、安全性を保証するものではありません",
                ],
            )
        }
        "browser_navigate" => {
            if o.len() != 1 || !o.contains_key("url") {
                return Err("unknown_arguments");
            }
            let url = args["url"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("unknown_arguments")?;
            if !safe_url(url) {
                return Err("private_arguments");
            }
            (
                "browser-navigate-v1",
                "指定URLへ移動",
                vec![
                    field("操作", "このMCP接続のブラウザーでページを開きます"),
                    field("アクセス先URL", url),
                ],
                vec!["ページ移動を伴います。リンク先の安全性や移動後URLは保証しません"],
            )
        }
        "browser_tabs" => {
            if o.len() != 1 || args["action"] != "list" {
                return Err("unknown_arguments");
            }
            (
                "browser-tabs-list-v1",
                "ブラウザーのタブ一覧を取得",
                vec![
                    field("操作", "タブ一覧を読み取ります"),
                    field("対象", "このMCP接続のブラウザー"),
                ],
                vec!["操作内容の説明であり、安全性を保証するものではありません"],
            )
        }
        _ => return Err("unsupported_renderer"),
    };
    Ok(
        json!({"renderer":renderer,"display":{"disclosure":"source_conversation","provenance":"proxy_verified_call","title":title,"fields":fields,"limitations":limitations,"omissions":[]}}),
    )
}
fn fallback(v: &mut Value, state: &str, reason: &str, pending: bool) {
    v["state"] = json!(state);
    v["reason"] = json!(reason);
    v["renderer"] = Value::Null;
    let private = state == "private_required";
    v["actions"] = json!({"allow_once":false,"allow_turn_tool":false,"decline":pending,"open_private_details":private});
    let omission = match reason {
        "unknown_arguments" => "unknown_arguments",
        "display_too_large" => "display_too_large",
        "private_arguments" => "private_arguments",
        _ => "information_incomplete",
    };
    v["display"] = json!({"disclosure":"source_conversation","provenance":if private{"proxy_verified_call"}else{"unavailable"},"title":if private{"操作内容の補足確認が必要です"}else{"操作内容を確認できません"},"fields":[],"limitations":[match reason {"display_too_large"=>"操作情報が長いため、本人限定画面で確認してください","unsupported_renderer"|"unknown_arguments"=>"この操作形式は公開表示に未対応です。本人限定画面で確認してください","private_arguments"=>"公開できない情報を含むため、本人限定画面で確認してください",_=>"現在の操作状態を確認してください"}],"omissions":[omission]});
}
fn text_units(v: &Value) -> usize {
    match v {
        Value::String(s) => s.encode_utf16().count(),
        Value::Array(a) => a.iter().map(text_units).sum(),
        Value::Object(o) => o
            .iter()
            .filter(|(k, _)| k.as_str() != "disclosure" && k.as_str() != "provenance")
            .map(|(_, v)| text_units(v))
            .sum(),
        _ => 0,
    }
}
fn snapshot(s: &Service, i: &Value, r: &Value) -> Result<Value> {
    declared(r)?;
    let op = mcp_grants::operation_snapshot(s, i, r)?;
    let expiry = mcp_grants::call_expiry(s, i)
        .unwrap_or(0)
        .min(i["expires_at_ms"].as_u64().unwrap_or(0));
    let pending = i["state"] == "pending";
    let mut v = json!({"interaction_id":i["interaction_id"],"response_id":i["response_id"],"turn_id":op["turn_id"],"revision":i["revision"],"scope_fingerprint":op["scope_fingerprint"],"presentation_id":null,"presentation_fingerprint":null,"profile":PROFILE,"renderer":null,"audience":{"kind":"source_conversation","channel_id":r["approval_context"]["channel_id"]},"state":"inline","reason":null,"expires_at_ms":expiry,"display":{},"actions":{"allow_once":true,"allow_turn_tool":op["turn_grant_eligible"]==true,"decline":true,"open_private_details":false}});
    if !pending
        || r["stop_requested"] == true
        || r["phase"] != "started"
        || r["turn_id"] != i["turn_id"]
    {
        fallback(&mut v, "unavailable", "interaction_closed", pending);
        v["expires_at_ms"] = Value::Null;
        return Ok(v);
    }
    if expiry <= mcp_grants::lease_now(s)
        || op["arguments"].is_null()
        || mcp_grants::check_scope(s, r, i).is_err()
        || op["config_generation"] != mcp_grants::generation(s)?
    {
        fallback(&mut v, "unavailable", "operation_unavailable", pending);
        v["expires_at_ms"] = Value::Null;
        return Ok(v);
    }
    let tool = op["tool"].as_str().unwrap_or("");
    let permitted = {
        let state = s.presentations.lock().unwrap();
        state.catalog_generation == op["config_generation"].as_str().unwrap_or("")
            && state
                .tools
                .contains_key(&(op["server"].as_str().unwrap_or("").into(), tool.into()))
    };
    let rendered = if !op["redacted_paths"].as_array().is_some_and(Vec::is_empty)
        || matches!(tool, "browser_evaluate" | "browser_run_code_unsafe")
    {
        Err("private_arguments")
    } else if !permitted {
        Err("unsupported_renderer")
    } else {
        render(tool, &op["arguments"])
    };
    match rendered {
        Ok(display) => {
            v["renderer"] = display["renderer"].clone();
            v["display"] = display["display"].clone();
            if v["actions"]["allow_turn_tool"] == true {
                v["display"]["limitations"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(
                        "この依頼中の同じツールを別の引数でも許可します。サイト限定ではありません"
                    ));
            }
            if text_units(&v["display"]) > MAX_TEXT {
                fallback(&mut v, "private_required", "display_too_large", true);
            }
        }
        Err(reason) => fallback(&mut v, "private_required", reason, true),
    }
    Ok(v)
}
fn digest(s: &Service, v: &Value) -> Result<String> {
    let state = s.presentations.lock().unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(&state.key).expect("fixed HMAC key");
    mac.update(&serde_json::to_vec(&json!([
        v,
        state.catalog_generation,
        state.catalog_epoch,
        state.tools.iter().collect::<Vec<_>>()
    ]))?);
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub fn get(s: &Service, iid: &str) -> Result<Value> {
    let _gate = s.approval_gate.lock().unwrap();
    if !s.limits.mcp_turn_approval_enabled {
        return Err(Error::code(503, "inline_approval_disabled"));
    }
    interactions::refresh(s)?;
    let i = s.store.get("interaction", iid)?;
    s.workspace_path(string(&i, "conversation_id")?)?;
    s.store.transaction(|tx| {
        super::service::ensure_ready(tx)?;
        let i=store::get(tx,"interaction",iid)?;
        retention::check_access(tx,&i)?;
        let r=store::get(tx,"response",string(&i,"response_id")?)?;
        let mut v=snapshot(s,&i,&r)?;
        let prior=match store::get(tx,"mcp_presentation",iid) {Ok(v)=>Some(v),Err(e) if e.status==404=>None,Err(e)=>return Err(e)};
        if v["state"]=="unavailable" {
            if let Some(mut old)=prior {old["active"]=json!(false);store::put(tx,"mcp_presentation",iid,&old)?;}
            return wire(v);
        }
        let hash=digest(s,&v)?;
        let old_count=prior.as_ref().and_then(|p|p["version_count"].as_u64()).unwrap_or(0);
        let record=if let Some(ref old)=prior && old["active"]==true && old["digest"]==hash {
            old.clone()
        } else if old_count>=MAX_VERSIONS {
            if let Some(mut old)=prior {old["active"]=json!(false);store::put(tx,"mcp_presentation",iid,&old)?;}
            fallback(&mut v,"private_required","presentation_limit",true);v["expires_at_ms"]=Value::Null;return wire(v);
        } else {
            let token= random_token()?;
            let row=json!({"presentation_id":id("present"),"interaction_id":iid,"response_id":r["response_id"],"revision":i["revision"],"scope_fingerprint":v["scope_fingerprint"],"scope":i["operation"]["scope"],"audience":v["audience"],"renderer":v["renderer"],"state":v["state"],"expires_at_ms":v["expires_at_ms"],"presentation_fingerprint":token,"digest":hash,"active":true,"version_count":old_count+1});
            store::put(tx,"mcp_presentation",iid,&row)?;
            store::put(tx,"mcp_presentation_audit",string(&row,"presentation_id")?,&row)?;
            row
        };
        v["presentation_id"]=record["presentation_id"].clone();v["presentation_fingerprint"]=record["presentation_fingerprint"].clone();
        wire(v)
    }).map_err(|e|if e.code=="store_unavailable" {Error::code(503,"presentation_store_unavailable")} else {e})
}
fn random_token() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn wire(mut v: Value) -> Result<Value> {
    if v["expires_at_ms"].is_null() {
        v.as_object_mut().unwrap().remove("expires_at_ms");
        v["expires_at"] = Value::Null;
    }
    mcp_grants::bounded_response(retention::wire(v), MAX_BYTES)
}
/// Must run inside the transaction that persists the reply intent.
pub fn check_reply(
    s: &Service,
    tx: &Transaction<'_>,
    i: &Value,
    r: &Value,
    body: &Value,
) -> Result<()> {
    validate_reply(body)?;
    if body.get("approval_view").is_none() {
        return Ok(());
    }
    if !s.limits.mcp_turn_approval_enabled {
        return Err(Error::code(503, "inline_approval_disabled"));
    }
    declared(r)?;
    let row = store::get(tx, "mcp_presentation", string(i, "interaction_id")?).map_err(|e| {
        if e.status == 404 {
            Error::code(409, "presentation_expired")
        } else {
            e
        }
    })?;
    if row["expires_at_ms"].as_u64().unwrap_or(0) <= mcp_grants::lease_now(s) {
        return Err(Error::code(409, "presentation_expired"));
    }
    if row["active"] != true
        || row["presentation_fingerprint"] != body["expected_presentation_fingerprint"]
    {
        return Err(Error::code(409, "presentation_conflict"));
    }
    let current = snapshot(s, i, r)?;
    if current["state"] == "unavailable" {
        return Err(Error::code(409, "presentation_expired"));
    }
    if row["digest"] != digest(s, &current)? {
        return Err(Error::code(409, "presentation_conflict"));
    }
    if current["state"] != "inline" || current["actions"]["allow_once"] != true {
        return Err(Error::code(422, "inline_approval_unavailable"));
    }
    if body.get("grant_scope").is_some() && current["actions"]["allow_turn_tool"] != true {
        return Err(Error::code(422, "turn_grant_ineligible"));
    }
    Ok(())
}
