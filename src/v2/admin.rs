//! Local operator control; never mounted on the Bearer HTTP API.
use super::{
    Error, Result, id, now,
    service::{Service, string},
    store,
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub fn execute(s: &Service, request: &Value) -> Result<Value> {
    let action = string(request, "action")?;
    match action {
        "recovery.release" => {
            let rid = string(request, "restore_id")?;
            if request["generation"] != s.store.generation
                || request["accept_risk"] != true
                || string(request, "reason")?.trim().is_empty()
            {
                return Err(Error::code(409, "recovery_confirmation_mismatch"));
            }
            let marker = s
                .store
                .root
                .parent()
                .unwrap()
                .join("v2-restore-pending.json");
            let result=s.store.transaction(|tx|{
     let(mut op,fresh)=store::reserve(tx,&format!("recovery-release-{rid}"),"recovery.release",request)?;
     if !fresh{
return store::get(tx,"audit",string(&op["resource"],"id")?);
}
     let metadata:Value=serde_json::from_slice(&std::fs::read(&marker)?)?;
     if request["restore_id"]!=metadata["restore_id"]||request["generation"]!=metadata["generation"]{
return Err(Error::code(409,"recovery_confirmation_mismatch"));
}
     let audit=json!({
         "audit_id":id("audit"),
         "action":"recovery.release",
         "restore_id":rid,
         "generation":s.store.generation,
         "reason":request["reason"],
         "created_at_ms":now(),
         "operator_uid":unsafe{ libc::geteuid()}
     });
     store::put(tx,"audit",string(&audit,"audit_id")?,&audit)?;
     tx.execute("UPDATE metadata SET value='ready' WHERE key='recovery_state'",[])?;
     tx.execute("INSERT INTO metadata VALUES ('restore_released',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[rid])?;
     op["state"]=json!("succeeded");
op["resource"]=json!({ "type":"audit","id":audit["audit_id"]});
store::save_operation(tx,&op)?;
Ok(audit)
   }
)?;
            match std::fs::remove_file(marker) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            super::files::sync_directory(s.store.root.parent().unwrap())?;
            Ok(result)
        }
        "workspace.register" => {
            use std::os::unix::fs::MetadataExt;
            let path = std::fs::canonicalize(string(request, "path")?)?;
            if path.starts_with(s.protected_root()) || s.protected_root().starts_with(&path) {
                return Err(Error::code(403, "resource_access_denied"));
            }
            let m = std::fs::symlink_metadata(&path)?;
            if !m.is_dir() {
                return Err(Error::code(400, "invalid_argument"));
            }
            s.store.transaction(|tx| {
                let (mut op, fresh) = store::reserve(
                    tx,
                    string(request, "operation_id")?,
                    "admin.workspace.register",
                    request,
                )?;
                if !fresh {
                    return store::get(tx, "workspace", string(&op["resource"], "id")?);
                }
                let wid = id("ws");
                let w = json!({
                    "workspace_id":wid,
                    "path":path,
                    "mode":"shared",
                    "state":"ready",
                    "display_name":string(request,"display_name")?,
                    "device":m.dev(),
                    "inode":m.ino()
                });
                store::put(tx, "workspace", &wid, &w)?;
                op["state"] = json!("succeeded");
                op["resource"] = json!({ "type":"workspace","id":wid});
                store::save_operation(tx, &op)?;
                Ok(w)
            })
        }
        "workspace.revoke" => s
            .store
            .update("workspace", string(request, "workspace_id")?, |w| {
                w["state"] = json!("revoked");
                Ok(())
            }),
        "backup.create" => {
            super::backup::create(s, std::path::Path::new(string(request, "destination")?))
        }
        "execution-hold.inspect" => s.store.transaction(|tx| {
            let rid = string(request, "response_id")?;
            let (r, _legacy) = hold_record(tx, rid)?;
            let token = id("review");
            let review = json!({
                "response_id":rid,
                "revision":r["hold_revision"],
                "generation":s.store.generation,
                "expires_at_ms":now()+300000,
                "used":false
            });
            store::put(tx, "review", &token, &review)?;
            Ok(json!({
                "instance_id":s.store.instance,
                "recovery_generation":s.store.generation,
                "response_id":rid,
                "conversation_id":r["conversation_id"],
                "workspace_id":r["workspace_id"],
                "execution_status":r["execution_status"],
                "hold_state":r["hold_state"],
                "hold_revision":r["hold_revision"],
                "review_token":token,
                "risk":"Unconfirmed execution may still access this workspace; release does not stop it."
            }))
        }),
        "execution-hold.release" => s.store.transaction(|tx| {
            let rid = string(request, "response_id")?;
            let key = string(request, "operation_id")?;
            let (mut op, fresh) = store::reserve(tx, &format!("admin-{key}"), action, request)?;
            if !fresh {
                return store::get(tx, "audit", string(&op["resource"], "id")?);
            }
            if request["accept_risk"] != true || string(request, "reason")?.trim().is_empty() {
                return Err(Error::code(400, "risk_acknowledgement_required"));
            }
            let token = string(request, "review_token")?;
            let mut review = store::get(tx, "review", token)?;
            let (mut r, legacy) = hold_record(tx, rid)?;
            if review["used"] == true
                || review["generation"] != s.store.generation
                || review["expires_at_ms"].as_u64().unwrap_or(0) <= now()
            {
                return Err(Error::code(409, "review_token_expired"));
            }
            if review["response_id"] != rid
                || review["revision"] != r["hold_revision"]
                || request["expected_revision"] != r["hold_revision"]
            {
                return Err(Error::code(409, "hold_revision_conflict"));
            }
            if r["phase"] != "unknown"
                || r["execution_status"] != "unknown"
                || r["hold_state"] != "held"
            {
                return Err(Error::code(409, "execution_not_unknown"));
            }
            let aid = id("audit");
            let audit = json!({
                "audit_id":aid,
                "operation_id":key,
                "response_id":rid,
                "execution_status":"unknown",
                "hold_state":"administratively_released",
                "dispatch_eligible":false,
                "conversation_state":"recovery_blocked",
                "workspace_reuse":"explicit_new_conversation",
                "reason":request["reason"],
                "created_at_ms":now(),
                "generation":s.store.generation,
                "operator_uid":unsafe{ libc::geteuid()}
            });
            r["hold_state"] = json!("administratively_released");
            r["hold_revision"] = json!(r["hold_revision"].as_u64().unwrap_or(0)+1);
            r["dispatch_eligible"] = json!(false);
            r["administrative_release"] = json!({ "audit_id":aid,"created_at_ms":now()});
            r["input"] = Value::Null;
            if !legacy {
                let cid = string(&r, "conversation_id")?;
                let mut c = store::get(tx, "conversation", cid)?;
                c["state"] = json!("recovery_blocked");
                store::put(tx, "conversation", cid, &c)?;
                let wid = string(&r, "workspace_id")?;
                let mut w = store::get(tx, "workspace", wid)?;
                w["mode"] = json!("shared");
                w["recovery_reuse"] = json!(true);
                store::put(tx, "workspace", wid, &w)?;
            }
            review["used"] = json!(true);
            store::put(tx, "review", token, &review)?;
            if legacy {
                r["state"] = json!("administratively_released");
                r["quarantined"] = json!(true);
                store::put(tx, "legacy_hold", rid, &r)?;
            } else {
                store::put(tx, "response", rid, &r)?;
            }
            store::put(tx, "audit", &aid, &audit)?;
            op["state"] = json!("succeeded");
            op["resource"] = json!({ "type":"audit","id":aid});
            store::save_operation(tx, &op)?;
            Ok(audit)
        }),
        _ => Err(Error::code(400, "invalid_admin_action")),
    }
}
pub fn serve(s: Arc<Service>) -> Result<tokio::task::JoinHandle<()>> {
    let path = s.store.root.join("admin.sock");
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let Ok(peer) = socket.peer_cred() else {
                continue;
            };
            if peer.uid() != unsafe { libc::geteuid() } {
                continue;
            }
            let service = s.clone();
            tokio::spawn(async move {
                let (mut read, mut write) = socket.into_split();
                let mut reader = BufReader::new((&mut read).take(16385));
                let mut line = String::new();
                let request = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    reader.read_line(&mut line),
                )
                .await;
                if !matches!(request, Ok(Ok(_))) || line.len() > 16384 {
                    return;
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    return;
                };
                let result = tokio::task::spawn_blocking(move || execute(&service, &request)).await;
                let response = match result {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => json!({ "error":{ "code":e.code} }),
                    Err(_) => json!({ "error":{ "code":"store_unavailable"} }),
                };
                let _ = write.write_all(format!("{response}\n").as_bytes()).await;
            });
        }
    }))
}
pub async fn client(root: &std::path::Path, args: &[String]) -> Result<()> {
    let group = args
        .first()
        .ok_or_else(|| Error::code(400, "invalid_admin_action"))?;
    let action = args
        .get(1)
        .ok_or_else(|| Error::code(400, "invalid_admin_action"))?;
    let mut request = json!({ "action":format!("{group}.{action}")});
    let mut i = 2;
    while i < args.len() {
        let key = args[i]
            .strip_prefix("--")
            .ok_or_else(|| Error::code(400, "invalid_argument"))?
            .replace('-', "_");
        if key == "accept_risk" {
            request[key] = json!(true);
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| Error::code(400, "invalid_argument"))?;
        request[key.clone()] = if key == "expected_revision" {
            json!(
                value
                    .parse::<u64>()
                    .map_err(|_| Error::code(400, "invalid_argument"))?
            )
        } else {
            json!(value)
        };
        i += 2;
    }
    let mut stream = tokio::net::UnixStream::connect(root.join("admin.sock")).await?;
    stream.write_all(format!("{request}\n").as_bytes()).await?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    println!("{line}");
    let response: Value = serde_json::from_str(&line)?;
    if response.get("error").is_some() {
        return Err(Error::code(409, "admin_operation_failed"));
    }
    Ok(())
}

fn hold_record(tx: &rusqlite::Transaction<'_>, rid: &str) -> Result<(Value, bool)> {
    match store::get(tx, "response", rid) {
        Ok(r) => Ok((r, false)),
        Err(e) if e.status == 404 => Ok((store::get(tx, "legacy_hold", rid)?, true)),
        Err(e) => Err(e),
    }
}
