//! Explicit v0.6 grants. Callers hold the interaction reply transaction.
use super::{
    Error, Result, approval_policy, approval_v06, catalog, id, mcp_grants,
    service::{Service, string},
    store,
};
use rusqlite::Transaction;
use serde_json::{Value, json};

pub fn selected(g: &Value) -> bool {
    g["profile"] == approval_policy::PROFILE
}
pub fn refresh(s: &Service, r: &Value, g: &mut Value, configuration: &str) {
    let terminal = matches!(g["state"].as_str(), Some("revoked" | "expired"));
    let reason = if terminal {
        None
    } else if g["expires_at_ms"].as_u64().unwrap_or(0) <= mcp_grants::lease_now(s) {
        Some("expired")
    } else if g["scope"]["instance_id"] != s.store.instance
        || g["scope"]["recovery_generation"] != s.store.generation
    {
        Some("runtime_restarted")
    } else if g["scope"]["config_generation"] != configuration {
        Some("config_changed")
    } else if g["scope"]["input_generation"] != r["input_generation"] {
        Some("input_changed")
    } else if g["scope"]["execution_policy_binding_id"] != r["approval_policy"]["binding_id"]
        || g["scope"]["policy_generation"] != r["approval_policy"]["generation"]
        || g["execution_policy"]["selection"] != r["approval_policy"]["selection"]
    {
        Some("policy_changed")
    } else if r["stop_requested"] == true
        || r["phase"] != "started"
        || r["approval_policy"]["state"] != "ready"
        || g["scope"]["context"] != r["approval_context"]
        || g["scope"]["turn_id"] != r["turn_id"]
        || g["scope"]["response_id"] != r["response_id"]
        || g["scope"]["conversation_id"] != r["conversation_id"]
        || g["scope"]["workspace_id"] != r["workspace_id"]
    {
        Some("scope_ended")
    } else {
        None
    };
    if let Some(reason) = reason {
        g["state"] = json!(if reason == "expired" {
            "expired"
        } else {
            "revoked"
        });
        g["reason"] = json!(reason);
    }
    let mut availability = json!({"state":"inactive","reason":g["reason"],"retry_after_ms":null});
    if g["state"] == "pending" {
        availability["reason"] = json!("activation_pending");
    } else if g["state"] == "suspended" {
        availability["reason"] = json!("response_unknown");
    } else if g["state"] == "active" {
        let status = approval_v06::catalog_key(s, r)
            .map(|k| s.catalog.peek(&k))
            .unwrap_or(catalog::Status::Failed("catalog_binding_mismatch"));
        match status {
            catalog::Status::Loading => {
                availability =
                    json!({"state":"refreshing","reason":"catalog_loading","retry_after_ms":2000})
            }
            catalog::Status::Failed(_) => {
                g["state"] = json!("revoked");
                g["reason"] = json!("catalog_failed");
                availability["reason"] = g["reason"].clone();
            }
            catalog::Status::Ready { snapshot, epoch } => {
                let definition = g["scope"]["server"]
                    .as_str()
                    .and_then(|name| snapshot.servers.get(name))
                    .filter(|server| server.available().is_ok())
                    .and_then(|server| {
                        g["scope"]["tool"]
                            .as_str()
                            .and_then(|tool| server.tools.get(tool))
                    })
                    .map(|hash| crate::control::fingerprint(&json!([hash, epoch])));
                if definition
                    .as_deref()
                    .is_some_and(|d| g["scope"]["definition_generation"] == d)
                {
                    availability = json!({"state":"ready","reason":null,"retry_after_ms":null});
                } else {
                    g["state"] = json!("revoked");
                    g["reason"] = json!("catalog_changed");
                    availability["reason"] = g["reason"].clone();
                }
            }
        }
    }
    g["availability"] = availability;
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
    if body["grant_scope"] != "turn_tool" || body["response"]["action"] != "accept" {
        return Err(Error::code(422, "turn_grant_ineligible"));
    }
    let operation = approval_v06::grant_operation(s, i, r)?;
    let policy = &operation["tool_policy"];
    let rid = string(r, "response_id")?;
    let mut all = store::list(tx, "mcp_grant")?
        .into_iter()
        .filter(|g| g["scope"]["response_id"] == rid)
        .collect::<Vec<_>>();
    if all.len() >= 16 {
        return Err(Error::code(429, "turn_grant_limit"));
    }
    let created = mcp_grants::lease_now(s);
    let gid = id("grant");
    let grant = json!({"grant_id":gid,"profile":approval_policy::PROFILE,"scope":operation["scope"],
        "execution_policy":r["approval_policy"],"grant_policy":{"policy_id":policy["policy_id"],"policy_version":policy["version"],
        "execution_policy_binding_id":r["approval_policy"]["binding_id"],"policy_generation":policy["policy_generation"],
        "definition_generation":policy["definition_generation"],"grant_scope":"turn_tool","allowed_effects":policy["effects"],
        "always_confirm_effects":["delete","external_send","permission_change","execute","credential","unknown"],
        "eligible_operations":policy["eligible_operations"],"always_confirm_operations":policy["always_confirm_operations"]},
        "availability":{"state":"inactive","reason":"activation_pending","retry_after_ms":null},
        "state":"pending","reason":null,"created_at_ms":created,"expires_at_ms":created+mcp_grants::TTL_MS,
        "application_count":1,"initial_interaction_id":i["interaction_id"]});
    if serde_json::to_vec(&grant)?.len() > 49152 {
        return Err(Error::code(422, "turn_grant_metadata_too_large"));
    }
    all.push(grant.clone());
    if serde_json::to_vec(&json!({"response_id":rid,"data":all}))?.len() > mcp_grants::GRANTS_BYTES
    {
        return Err(Error::code(503, "approval_response_too_large"));
    }
    store::put(tx, "mcp_grant", &gid, &grant)?;
    i["grant_id"] = json!(gid);
    Ok(())
}
pub fn candidate(s: &Service, r: &Value, i: &Value) -> Result<Option<String>> {
    let Ok(op) = approval_v06::grant_operation(s, i, r) else {
        return Ok(None);
    };
    Ok(s.store
        .list("mcp_grant")?
        .into_iter()
        .find(|g| {
            selected(g)
                && g["state"] == "active"
                && g["availability"]["state"] == "ready"
                && g["scope"] == op["scope"]
        })
        .and_then(|g| g["grant_id"].as_str().map(str::to_owned)))
}
pub fn apply(s: &Service, tx: &Transaction<'_>, r: &Value, i: &mut Value, gid: &str) -> Result<()> {
    let op = approval_v06::grant_operation(s, i, r)?;
    let mut g = store::get(tx, "mcp_grant", gid)?;
    refresh(s, r, &mut g, &mcp_grants::generation(s)?);
    if !selected(&g)
        || g["state"] != "active"
        || g["availability"]["state"] != "ready"
        || g["scope"] != op["scope"]
    {
        return Err(Error::code(409, "grant_inactive"));
    }
    g["application_count"] = json!(
        g["application_count"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::code(503, "store_corrupt"))?
    );
    store::put(tx, "mcp_grant", gid, &g)?;
    i["grant_id"] = json!(gid);
    Ok(())
}

pub async fn await_candidate(
    s: &Service,
    runtime: &std::sync::Arc<crate::runtime::CodexRuntime>,
    iid: &str,
) -> Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let i = s.store.get("interaction", iid)?;
        let r = s.store.get("response", string(&i, "response_id")?)?;
        if i["state"] != "pending"
            || r["stop_requested"] == true
            || r["phase"] != "started"
            || i["expires_at_ms"].as_u64().unwrap_or(0) <= mcp_grants::lease_now(s)
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(None);
        }
        let matching = s.store.list("mcp_grant")?.into_iter().any(|g| {
            selected(&g)
                && matches!(g["state"].as_str(), Some("pending" | "active"))
                && g["scope"]["execution_policy_binding_id"] == r["approval_policy"]["binding_id"]
                && i["operation"]["scope"]
                    .as_object()
                    .is_some_and(|scope| scope.iter().all(|(k, v)| g["scope"][k] == *v))
        });
        if !matching {
            return Ok(None);
        }
        let Some(key) = approval_v06::catalog_key(s, &r) else {
            return Ok(None);
        };
        let Ok(receiver) = s.catalog.request(key, runtime.clone()) else {
            return Ok(None);
        };
        let status = catalog::Manager::view(receiver).await;
        let current = s.store.get("response", string(&r, "response_id")?)?;
        if current["phase"] != "started" || current["stop_requested"] == true {
            if let Some(thread) = current["thread_id"].as_str() {
                s.catalog.release_thread(runtime.id(), thread);
            }
            return Ok(None);
        }
        mcp_grants::refresh(s)?;
        if let Some(gid) = candidate(s, &r, &i)? {
            return Ok(Some(gid));
        }
        if !matches!(status, catalog::Status::Loading) {
            // The first reply can still be pending acknowledgement.
            let pending = s.store.list("mcp_grant")?.iter().any(|g| {
                selected(g)
                    && g["state"] == "pending"
                    && g["scope"]["response_id"] == r["response_id"]
                    && g["scope"]["server"] == i["operation"]["server"]
                    && g["scope"]["tool"] == i["operation"]["tool"]
            });
            if !pending {
                return Ok(None);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}
