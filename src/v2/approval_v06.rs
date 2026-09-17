//! Generic full-argument presentation and single-approval verification.
//! No Discord identity or UI behavior belongs in this module.
use super::{
    Error, Result, approval_display, approval_policy, id, interactions, mcp_grants, retention,
    service::{Service, string},
    store,
};
use hmac::{Hmac, Mac};
use rusqlite::Transaction;
use serde_json::{Value, json};
use sha2::Sha256;

pub fn query(raw: Option<&str>) -> Result<(String, usize, Option<String>)> {
    let url = reqwest::Url::parse(&format!("http://local/?{}", raw.unwrap_or("")))
        .map_err(|_| Error::code(400, "invalid_approval_presentation"))?;
    let mut audience = "source_conversation".to_owned();
    let mut page = 0;
    let mut id = None;
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in url.query_pairs() {
        if !seen.insert(key.to_string()) {
            return Err(Error::code(400, "invalid_approval_presentation"));
        }
        match key.as_ref() {
            "audience" if ["requester", "source_conversation"].contains(&value.as_ref()) => {
                audience = value.into_owned()
            }
            "page" => {
                page = value
                    .parse()
                    .map_err(|_| Error::code(400, "invalid_presentation_page"))?;
                if page >= 64 {
                    return Err(Error::code(400, "invalid_presentation_page"));
                }
            }
            "presentation_id" if !value.is_empty() && value.len() <= 128 => {
                id = Some(value.into_owned())
            }
            _ => return Err(Error::code(400, "invalid_approval_presentation")),
        }
    }
    if page > 0 && id.is_none() {
        return Err(Error::code(400, "invalid_presentation_page"));
    }
    Ok((audience, page, id))
}
pub fn selected(r: &Value) -> bool {
    r["approval_presentation"]["profile"] == approval_policy::PROFILE
}
pub fn handles(r: &Value, i: &Value) -> bool {
    selected(r) && i["native_question_id"].is_string()
}
fn digest(s: &Service, value: &Value) -> Result<String> {
    let key = s.presentations.lock().unwrap().key;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC key");
    mac.update(&serde_json::to_vec(value)?);
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn bounded(v: Value) -> Result<Value> {
    if serde_json::to_vec(&v)?.len() > 65536 {
        return Err(Error::code(503, "approval_response_too_large"));
    }
    Ok(v)
}
pub fn catalog_key(s: &Service, r: &Value) -> Option<super::catalog::Key> {
    Some(super::catalog::Key {
        instance: s.store.instance.clone(),
        recovery: s.store.generation.clone(),
        runtime: r["_approval_runtime"].as_str()?.into(),
        thread: r["thread_id"].as_str()?.into(),
        config: mcp_grants::generation(s).ok()?,
    })
}
fn evaluation(
    s: &Service,
    r: &Value,
    op: &Value,
) -> Result<super::approval_evaluation::Evaluation> {
    let status = catalog_key(s, r)
        .map(|key| s.catalog.peek(&key))
        .unwrap_or(super::catalog::Status::Failed("catalog_binding_mismatch"));
    Ok(super::approval_evaluation::evaluate(
        r,
        op,
        &status,
        &mcp_grants::generation(s)?,
    ))
}
pub(crate) fn operation(s: &Service, i: &Value, r: &Value) -> Result<Value> {
    let mut op = mcp_grants::raw_operation_snapshot(s, i, r)?;
    op["profile"] = json!(approval_policy::PROFILE);
    op["execution_policy"] = r["approval_policy"].clone();
    let evaluation = evaluation(s, r, &op)?;
    op["semantic_assessment"] = evaluation.assessment;
    op["tool_policy"] = evaluation.tool_policy;
    op["turn_grant_eligible"] = json!(false);
    op["ineligible_reason"] = if op["tool_policy"].is_null() {
        json!("policy_not_selected")
    } else {
        op["tool_policy"]["reason"].clone()
    };
    let complete = op["binding_status"] == "verified" && !op["arguments"].is_null();
    op["turn_grant_eligible"] = json!(
        s.limits.mcp_turn_approval_enabled
            && complete
            && op["tool_policy"]["turn_eligible"] == true
    );
    op["argument_integrity"] = json!({"status":if complete{"complete"}else{"unavailable"},"source":"codex_call_event","reason":if complete{Value::Null}else if op["binding_status"]!="verified"{json!("call_unbound")}else{json!("operation_details_expired")}});
    if op["scope"].is_object() {
        op["scope"]["execution_policy_binding_id"] = r["approval_policy"]["binding_id"].clone();
        op["scope"]["policy_generation"] = r["approval_policy"]["generation"].clone();
        op["scope"]["definition_generation"] = evaluation.definition_generation;
        op["scope_fingerprint"] = json!(digest(
            s,
            &json!([
                op["scope"],
                i["operation"]["scope_fingerprint"],
                op["arguments"]
            ])
        )?);
    }
    op["arguments_delivery"] = json!(if complete { "inline" } else { "unavailable" });
    Ok(op)
}
pub fn operation_details(s: &Service, iid: &str) -> Result<Value> {
    interactions::refresh(s)?;
    let i = s.store.get("interaction", iid)?;
    s.workspace_path(string(&i, "conversation_id")?)?;
    let r = s.store.get("response", string(&i, "response_id")?)?;
    let mut op = operation(s, &i, &r)?;
    if serde_json::to_vec(&op)?.len() > 65536 && op["argument_integrity"]["status"] == "complete" {
        op["arguments"] = Value::Null;
        op["arguments_delivery"] = json!("presentation_pages");
    }
    bounded(op)
}
struct View {
    base: Value,
    pages: Vec<Value>,
    hash: String,
}
fn snapshot(s: &Service, i: &Value, r: &Value, audience: &str) -> Result<View> {
    if !["requester", "source_conversation"].contains(&audience) {
        return Err(Error::code(400, "invalid_approval_presentation"));
    }
    let op = operation(s, i, r)?;
    let expiry = mcp_grants::call_expiry(s, i)
        .unwrap_or(0)
        .min(i["expires_at_ms"].as_u64().unwrap_or(0));
    let mut base = json!({"interaction_id":i["interaction_id"],"response_id":i["response_id"],"turn_id":i["turn_id"],"revision":i["revision"],"scope_fingerprint":op["scope_fingerprint"],"scope":op["scope"],
        "presentation_id":null,"presentation_fingerprint":null,"profile":approval_policy::PROFILE,"renderer":"raw-arguments-v1",
        "audience":{"kind":audience,"channel_id":r["approval_context"]["channel_id"],"principal_id":null},"state":"ready","reason":null,"expires_at_ms":expiry,
        "diagnostic":{"code":null,"retryable":false,"retry_after_ms":null},"page":null,
        "execution_policy":op["execution_policy"],"argument_integrity":op["argument_integrity"],"semantic_assessment":op["semantic_assessment"],"tool_policy":op["tool_policy"],
        "display":{"disclosure":if audience=="requester"{"requester_only"}else{"source_conversation"},"provenance":"unavailable","title":"MCP操作の確認","fields":[],"limitations":[],"omissions":[]},
        "actions":{"allow_once":false,"allow_turn_tool":false,"decline":i["state"]=="pending","open_private_details":false,"retry":false}});
    if audience == "requester" {
        base["audience"]["principal_id"] = r["approval_context"]["principal_id"].clone();
    }
    let reason = if i["state"] != "pending"
        || r["stop_requested"] == true
        || r["phase"] != "started"
        || r["turn_id"] != i["turn_id"]
    {
        Some("interaction_closed")
    } else if expiry <= mcp_grants::lease_now(s) {
        Some("operation_details_expired")
    } else if op["argument_integrity"]["status"] != "complete" {
        Some("information_incomplete")
    } else if mcp_grants::check_scope(s, r, i).is_err()
        || mcp_grants::generation(s)? != op["config_generation"]
    {
        Some("presentation_stale")
    } else if r["approval_policy"]["state"] != "ready" {
        Some("approval_policy_not_ready")
    } else if op["tool_policy"]["decision"] == "blocked" {
        Some("policy_denied")
    } else if op["tool_policy"]["decision"] == "unavailable" {
        Some("policy_check_unavailable")
    } else {
        None
    };
    let mut pages = vec![];
    if let Some(reason) = reason {
        base["state"] = json!("unavailable");
        base["reason"] = json!(reason);
        base["diagnostic"]["code"] = json!(reason);
    } else {
        let evaluation = evaluation(s, r, &op)?;
        if evaluation.description.is_some() {
            base["renderer"] = json!("evaluated-operation-v1");
        }
        match approval_display::pages(
            op["server"].as_str().unwrap_or(""),
            op["tool"].as_str().unwrap_or(""),
            &op["arguments"],
            evaluation.description,
        ) {
            Ok(mut p) => {
                let disclosure = if evaluation.description.is_some() {
                    super::approval_privacy::ordinary_arguments(&op["arguments"])
                } else {
                    super::approval_privacy::Disclosure::Unclassified
                };
                if audience == "source_conversation"
                    && (disclosure != super::approval_privacy::Disclosure::Public || p.len() != 1)
                {
                    let reason = match disclosure {
                        super::approval_privacy::Disclosure::Public => "display_too_large",
                        super::approval_privacy::Disclosure::Requester => "private_arguments",
                        _ => "privacy_unclassified",
                    };
                    base["state"] = json!("private_required");
                    base["reason"] = json!(reason);
                    base["display"]["limitations"] =
                        json!(["操作の全内容を、自分だけに表示して確認してください"]);
                    base["actions"]["open_private_details"] = json!(true);
                } else {
                    if audience == "source_conversation" {
                        for page in &mut p {
                            page["disclosure"] = json!("source_conversation");
                        }
                    }
                    pages = p;
                    base["actions"]["allow_once"] = json!(true);
                    base["actions"]["allow_turn_tool"] = op["turn_grant_eligible"].clone();
                }
            }
            Err(reason) => {
                base["state"] = json!("unavailable");
                base["reason"] = json!(reason);
                base["diagnostic"]["code"] = json!(reason);
            }
        }
    }
    let hash = digest(s, &json!([base, pages]))?;
    Ok(View { base, pages, hash })
}
fn record_key(iid: &str, audience: &str) -> String {
    format!("{iid}:{audience}")
}
fn token(s: &Service, record: &Value, index: usize) -> Result<String> {
    digest(
        s,
        &json!([
            record["presentation_id"],
            record["digest"],
            index,
            record["expires_at_ms"],
            record["audience"]
        ]),
    )
}
pub fn get(
    s: &Service,
    iid: &str,
    audience: &str,
    page: usize,
    expected_id: Option<&str>,
) -> Result<Value> {
    let _gate = s.approval_gate.lock().unwrap();
    interactions::refresh(s)?;
    let i = s.store.get("interaction", iid)?;
    s.workspace_path(string(&i, "conversation_id")?)?;
    s.store.transaction(|tx|{
        super::service::ensure_ready(tx)?;
        let i=store::get(tx,"interaction",iid)?;retention::check_access(tx,&i)?;
        let r=store::get(tx,"response",string(&i,"response_id")?)?;
        let mut v=snapshot(s,&i,&r,audience)?;
        if v.pages.is_empty() {return bounded(retention::wire(v.base));}
        if page>=v.pages.len() {return Err(Error::code(400,"invalid_presentation_page"));}
        let key=record_key(iid,audience);
        let prior=match store::get(tx,"mcp_presentation_v06",&key) {Ok(r)=>Some(r),Err(e)if e.status==404=>None,Err(e)=>return Err(e)};
        let versions=prior.as_ref().and_then(|r|r["version_count"].as_u64()).unwrap_or(0);
        let row=if let Some(ref row)=prior && row["digest"]==v.hash {row.clone()} else {
            if expected_id.is_some() {return Err(Error::code(409,"presentation_stale"));}
            if versions>=4 {return Err(Error::code(409,"presentation_limit"));}
            let row=json!({"presentation_id":id("present"),"interaction_id":iid,"response_id":r["response_id"],"audience":audience,"digest":v.hash,
                "presentation_fingerprint":id("view"),"version_count":versions+1,"expires_at_ms":v.base["expires_at_ms"]});
            store::put(tx,"mcp_presentation_v06",&key,&row)?;row
        };
        if expected_id.is_some_and(|id|row["presentation_id"]!=id) {return Err(Error::code(409,"presentation_stale"));}
        v.base["presentation_id"]=row["presentation_id"].clone();
        v.base["presentation_fingerprint"]=row["presentation_fingerprint"].clone();
        v.base["display"]=v.pages[page].clone();
        v.base["page"]=json!({"index":page,"count":v.pages.len(),"token":token(s,&row,page)?,"content_fingerprint":v.hash});
        bounded(retention::wire(v.base))
    })
}
pub fn check_reply(
    s: &Service,
    tx: &Transaction<'_>,
    i: &Value,
    r: &Value,
    body: &Value,
) -> Result<()> {
    if body["response"]["action"] == "decline" {
        return Ok(());
    }
    if body["expected_policy_binding_id"] != r["approval_policy"]["binding_id"]
        || !body["expected_policy_binding_id"].is_string()
    {
        return Err(Error::code(409, "approval_policy_binding_mismatch"));
    }
    let audience = body["approval_view"]
        .as_str()
        .ok_or_else(|| Error::code(409, "presentation_audience_mismatch"))?;
    let view = snapshot(s, i, r, audience)?;
    if body.get("grant_scope").is_some()
        && (body["grant_scope"] != "turn_tool" || view.base["actions"]["allow_turn_tool"] != true)
    {
        return Err(Error::code(422, "turn_grant_ineligible"));
    }
    if view.base["actions"]["allow_once"] != true {
        return Err(Error::code(409, "presentation_incomplete"));
    }
    if view.base["scope_fingerprint"] != body["expected_scope_fingerprint"] {
        return Err(Error::code(409, "presentation_stale"));
    }
    let row = store::get(
        tx,
        "mcp_presentation_v06",
        &record_key(string(i, "interaction_id")?, audience),
    )
    .map_err(|e| {
        if e.status == 404 {
            Error::code(409, "presentation_expired")
        } else {
            e
        }
    })?;
    if row["expires_at_ms"].as_u64().unwrap_or(0) <= mcp_grants::lease_now(s) {
        return Err(Error::code(409, "presentation_expired"));
    }
    if row["digest"] != view.hash
        || row["presentation_fingerprint"] != body["expected_presentation_fingerprint"]
    {
        return Err(Error::code(409, "presentation_stale"));
    }
    let supplied = body["expected_page_tokens"]
        .as_array()
        .ok_or_else(|| Error::code(409, "presentation_incomplete"))?;
    if supplied.len() != view.pages.len() {
        return Err(Error::code(409, "presentation_incomplete"));
    }
    for (index, value) in supplied.iter().enumerate() {
        if *value != token(s, &row, index)? {
            return Err(Error::code(409, "presentation_incomplete"));
        }
    }
    Ok(())
}

/// Rechecked in the reply transaction, including native binding, stop/steer,
/// current definitions, preparation proof, and the argument/display budget.
pub(crate) fn grant_operation(s: &Service, i: &Value, r: &Value) -> Result<Value> {
    let view = snapshot(s, i, r, "requester")?;
    if view.base["actions"]["allow_turn_tool"] != true {
        return Err(Error::code(409, "grant_inactive"));
    }
    operation(s, i, r)
}
pub(crate) fn accepted_metadata(s: &Service, i: &mut Value, r: &Value) {
    if let Ok(mut op) = operation(s, i, r) {
        op.as_object_mut().unwrap().remove("arguments");
        op["grant_id"] = i["grant_id"].clone();
        i["_accepted_operation"] = op;
    }
}
