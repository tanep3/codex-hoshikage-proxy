use super::{
    Error, Result, now,
    service::{Service, string},
    store::{self},
};
use rusqlite::Transaction;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn parse_time(v: &Value) -> Result<u64> {
    let t = OffsetDateTime::parse(
        v.as_str()
            .ok_or_else(|| Error::code(400, "invalid_argument"))?,
        &Rfc3339,
    )
    .map_err(|_| Error::code(400, "invalid_argument"))?;
    u64::try_from(t.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| Error::code(400, "invalid_argument"))
}
pub fn wire(mut v: Value) -> Value {
    match &mut v {
        Value::Object(o) => {
            let keys: Vec<String> = o.keys().cloned().collect();
            for k in keys {
                let item = o.remove(&k).unwrap();
                if (k.ends_with("_at_ms")
                    || matches!(k.as_str(), "hold_until_ms" | "max_hold_until_ms"))
                    && let Some(ms) = item.as_u64()
                    && let Ok(t) =
                        OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
                {
                    o.insert(
                        k.trim_end_matches("_ms").to_owned(),
                        json!(t.format(&Rfc3339).unwrap()),
                    );
                    continue;
                }
                o.insert(k, wire(item));
            }
        }
        Value::Array(a) => {
            for item in a {
                *item = wire(item.take());
            }
        }
        _ => {}
    }
    v
}
pub fn expiry(tx: &Transaction<'_>, kind: &str, rid: &str, base: u64) -> Result<u64> {
    Ok(store::list(tx, "lease")?
        .iter()
        .filter(|l| {
            l["state"] == "active"
                && l["resource"]["id"] == rid
                && l["resource"]["type"]
                    == if kind == "response" {
                        "response_output"
                    } else {
                        "artifact"
                    }
        })
        .filter_map(|l| l["hold_until_ms"].as_u64())
        .fold(base, u64::max))
}
pub fn check_access(tx: &Transaction<'_>, record: &Value) -> Result<()> {
    let w = store::get(tx, "workspace", string(record, "workspace_id")?)?;
    if w["state"] != "ready" {
        return Err(Error::code(403, "workspace_access_revoked"));
    }
    Ok(())
}
pub fn lease(s: &Service, path: &[String], key: &str, body: &Value) -> Result<Value> {
    s.store.transaction(|tx| {
        let (mut op, fresh) = store::reserve(tx, key, "lease", &json!({ "path":path,"body":body}))?;
        if !fresh {
            return Ok(op["result"].clone());
        }
        super::service::ensure_ready(tx)?;
        let release = path.get(2).map(String::as_str) == Some("release");
        if !(path.len() == 1 || path.len() == 3 && matches!(path[2].as_str(), "extend" | "release"))
        {
            return Err(Error::code(404, "resource_not_found"));
        }
        let allowed = if path.len() == 1 {
            vec!["resource", "hold_until"]
        } else if release {
            vec![]
        } else {
            vec!["hold_until"]
        };
        if body
            .as_object()
            .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
        {
            return Err(Error::code(422, "unknown_field"));
        }
        let mut l = if path.len() == 1 {
            json!({
                "lease_id":super::id("lease"),
                "resource":body["resource"],
                "state":"active"
            })
        } else {
            store::get(tx, "lease", &path[1])?
        };
        let resource = &l["resource"];
        let kind = match resource["type"].as_str() {
            Some("artifact") => "artifact",
            Some("response_output") => "response",
            _ => return Err(Error::code(400, "invalid_argument")),
        };
        let rid = string(resource, "id")?;
        let r = store::get(tx, kind, rid)?;
        check_access(tx, &r)?;
        let content = if kind == "response" { &r["output"] } else { &r };
        if !release {
            if content["state"] != "ready"
                || expiry(
                    tx,
                    kind,
                    rid,
                    content["expires_at_ms"].as_u64().unwrap_or(0),
                )? <= now()
            {
                return Err(Error::code(410, "content_expired"));
            }
            if path.len() > 1 && l["state"] == "released" {
                return Err(Error::code(409, "lease_released"));
            }
            if path.len() > 1
                && (l["state"] != "active" || l["hold_until_ms"].as_u64().unwrap_or(0) <= now())
            {
                return Err(Error::code(410, "lease_expired"));
            }
            let maximum = content["max_hold_until_ms"].as_u64().unwrap_or_else(|| {
                content["ready_at_ms"]
                    .as_u64()
                    .unwrap_or_else(|| r["created_at_ms"].as_u64().unwrap_or(0))
                    + s.limits.lease_max_lifetime_seconds * 1000
            });
            let until = parse_time(&body["hold_until"])?;
            if until <= now()
                || until > maximum
                || path.len() > 1 && until < l["hold_until_ms"].as_u64().unwrap_or(0)
            {
                return Err(Error::code(409, "retention_limit"));
            }
            l["hold_until_ms"] = json!(until);
            l["max_hold_until_ms"] = json!(maximum);
        } else {
            l["state"] = json!("released");
        }
        l["operation_id"] = op["operation_id"].clone();
        store::put(tx, "lease", string(&l, "lease_id")?, &l)?;
        op["resource"] = json!({ "type":"lease","id":l["lease_id"]});
        op["state"] = json!("succeeded");
        op["result"] = l.clone();
        store::save_operation(tx, &op)?;
        Ok(l)
    })
}
pub fn gc(s: &Service) -> Result<()> {
    if s.store.metadata("recovery_state")? != "ready" {
        return Ok(());
    }
    s.store.transaction(|tx| {
        for mut r in store::list(tx, "response")? {
            if !r["input"].is_null()
                && r["input_expires_at_ms"].as_u64().unwrap_or(u64::MAX) <= now()
                && r["phase"] == "unknown"
            {
                r["input"] = Value::Null;
                store::put(tx, "response", string(&r, "response_id")?, &r)?;
            }
            if r["hold_state"] != "held" && r["phase"] != "unknown" {
                let cid = string(&r, "conversation_id")?;
                let mut c = store::get(tx, "conversation", cid)?;
                if c["active_response_id"] == r["response_id"] {
                    c["active_response_id"] = Value::Null;
                    store::put(tx, "conversation", cid, &c)?;
                }
            }
        }
        for kind in ["artifact", "response"] {
            for mut r in store::list(tx, kind)? {
                let rid = string(
                    &r,
                    if kind == "artifact" {
                        "artifact_id"
                    } else {
                        "response_id"
                    },
                )?
                .to_owned();
                let content = if kind == "response" { &r["output"] } else { &r };
                if content["state"] == "ready"
                    && !store::list(tx, "read_pin")?.iter().any(|p| {
                        p["resource_id"] == rid && p["hold_until_ms"].as_u64().unwrap_or(0) > now()
                    })
                    && expiry(
                        tx,
                        kind,
                        &rid,
                        content["expires_at_ms"].as_u64().unwrap_or(0),
                    )? <= now()
                {
                    if kind == "response" {
                        r["output"]["state"] = json!("expired");
                    } else {
                        r["state"] = json!("expired");
                    }
                    store::put(tx, kind, &rid, &r)?;
                }
            }
        }
        Ok(())
    })?;
    // Tombstone commit precedes deletion, so a crash can only delay reclamation.
    for kind in ["artifact", "response"] {
        for r in s.store.list(kind)? {
            let content = if kind == "response" { &r["output"] } else { &r };
            if content["state"] == "expired" {
                let rid = string(
                    &r,
                    if kind == "artifact" {
                        "artifact_id"
                    } else {
                        "response_id"
                    },
                )?;
                match std::fs::remove_file(s.store.root.join("blobs").join(rid)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(())
}
