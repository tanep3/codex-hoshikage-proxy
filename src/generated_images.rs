//! Read-only bridge for images emitted by Codex image tools outside the workspace.
//! The execution supplies the thread and time interval; callers cannot select a root.
use crate::{
    control::Execution,
    http::AppState,
    v2::{Error, Result, files},
};
use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{io::Read, path::PathBuf, time::UNIX_EPOCH};
const LIMIT: u64 = 10 * 1024 * 1024;
fn scope(state: &AppState, rid: &str) -> Result<(PathBuf, Execution)> {
    if state.api_key.is_none() {
        return Err(Error::code(401, "api_key_required"));
    }
    let execution = state
        .responses
        .control
        .get(rid)?
        .ok_or(Error::code(404, "response_not_found"))?;
    if execution.phase != "finished" {
        return Err(Error::code(409, "response_not_finished"));
    }
    let thread = execution
        .thread_id
        .as_deref()
        .ok_or(Error::code(404, "thread_not_found"))?;
    if thread.is_empty()
        || !thread
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return Err(Error::code(403, "source_access_denied"));
    }
    let root = state
        .generated_images_root
        .as_ref()
        .ok_or(Error::code(404, "generated_images_unavailable"))?
        .join(thread);
    Ok((root, execution))
}
fn allowed(name: &str, m: &std::fs::Metadata, execution: &Execution) -> bool {
    let modified = m
        .modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|v| v.as_millis());
    !name.is_empty()
        && name.ends_with(".png")
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        && m.is_file()
        && m.len() <= LIMIT
        && modified
            .is_some_and(|v| v >= execution.started_at_ms && v <= execution.last_observed_at_ms)
}
pub async fn list(State(state): State<AppState>, Path(rid): Path<String>) -> Result<Response> {
    let (root, execution) = scope(&state, &rid)?;
    let result=tokio::task::spawn_blocking(move || -> Result<_> {
        let entries=match std::fs::read_dir(&root) { Ok(v)=>v,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(Vec::new()),Err(e)=>return Err(e.into()) };
        let mut images=Vec::new();
        for entry in entries {
            let entry=entry?;let name=entry.file_name().to_string_lossy().into_owned();
            if !allowed(&name,&entry.metadata()?,&execution) {continue;}
            if let Ok(file)=files::open_source(&root,&name) {
                if !allowed(&name,&file.metadata()?,&execution) {continue;}
                images.push(json!({"name":name,"content_type":"image/png","size_bytes":file.metadata()?.len()}));
            }
            if images.len()>16 {return Err(Error::code(413,"too_many_images"));}
        }
        images.sort_by_key(|v|v["name"].as_str().unwrap_or_default().to_owned());
        Ok(images)
    }).await.map_err(|_|Error::code(503,"store_unavailable"))??;
    Ok(Json(json!({"data":result})).into_response())
}
pub async fn content(
    State(state): State<AppState>,
    Path((rid, name)): Path<(String, String)>,
) -> Result<Response> {
    let (root, execution) = scope(&state, &rid)?;
    let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut file = files::open_source(&root, &name)?;
        let before = file.metadata()?;
        if !allowed(&name, &before, &execution) {
            return Err(Error::code(404, "image_not_found"));
        }
        let mut data = Vec::new();
        (&mut file).take(LIMIT + 1).read_to_end(&mut data)?;
        let after = file.metadata()?;
        if data.len() as u64 > LIMIT
            || before.len() != after.len()
            || before.modified()? != after.modified()?
        {
            return Err(Error::code(409, "source_changed"));
        }
        if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(Error::code(415, "invalid_image"));
        }
        Ok(data)
    })
    .await
    .map_err(|_| Error::code(503, "store_unavailable"))??;
    let hash = format!("\"{:x}\"", Sha256::digest(&data));
    Ok((
        [
            ("content-type", "image/png"),
            ("cache-control", "private, no-store"),
            ("x-content-type-options", "nosniff"),
            ("etag", hash.as_str()),
        ],
        data,
    )
        .into_response())
}
