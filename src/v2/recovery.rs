use super::{
    Result, files,
    service::{Service, string},
    store,
};
use serde_json::{Value, json};
pub fn recover(s: &Service) -> Result<()> {
    for kind in ["artifact", "response"] {
        for mut r in s.store.list(kind)? {
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
            if !matches!(
                content["state"].as_str(),
                Some("creating" | "saving" | "ready" | "failed")
            ) {
                continue;
            }
            let manifest = s.store.root.join("blobs").join(format!("{rid}.manifest"));
            if content["state"] == "failed" && !manifest.exists() {
                continue;
            }
            let recovered = (|| -> Result<Value> {
                let metadata: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
                let blob = s.store.root.join("blobs").join(&rid);
                let staging = s.store.root.join("staging").join(&rid);
                let source = if blob.exists() { &blob } else { &staging };
                files::verify(
                    source,
                    metadata["size_bytes"].as_u64().unwrap_or(0),
                    string(&metadata, "sha256")?,
                )?;
                if !blob.exists() {
                    std::fs::rename(&staging, &blob)?;
                    files::sync_directory(&s.store.root.join("blobs"))?;
                }
                Ok(metadata)
            })();
            match recovered {
                Ok(metadata) => {
                    if kind == "response" {
                        r["output"] = metadata;
                    } else {
                        for (k, v) in metadata.as_object().unwrap() {
                            r[k] = v.clone();
                        }
                    }
                }
                Err(_) => {
                    let state = if content["state"] == "ready" {
                        "corrupt"
                    } else {
                        "unknown"
                    };
                    if kind == "response" {
                        r["output"]["state"] =
                            json!(if state == "unknown" { "failed" } else { state });
                    } else {
                        r["state"] = json!(state);
                    }
                }
            }
            s.store.transaction(|tx| {
                store::put(tx, kind, &rid, &r)?;
                if kind == "artifact" {
                    let mut stmt = tx.prepare("SELECT value FROM operations")?;
                    let ops = stmt
                        .query_map([], |r| r.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    for raw in ops {
                        let mut op: Value = serde_json::from_str(&raw)?;
                        if op["resource"]["id"] == rid {
                            op["state"] = json!(if r["state"] == "ready" {
                                "succeeded"
                            } else {
                                "unknown"
                            });
                            store::save_operation(tx, &op)?;
                        }
                    }
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}
