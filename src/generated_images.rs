//! Read-only bridge for images emitted by Codex image tools outside the workspace.
//! The execution supplies the thread and time interval; callers cannot select a root.
use crate::{
    control::Execution,
    http::{ApiError, AppState},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path as FsPath, PathBuf},
    time::UNIX_EPOCH,
};
const LIMIT: u64 = 10 * 1024 * 1024;
type Result<T> = std::result::Result<T, ApiError>;

fn error(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code, code.replace('_', " "))
}

fn io_error(_: std::io::Error) -> ApiError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "generated_image_unavailable",
    )
}

fn scope(state: &AppState, rid: &str) -> Result<(PathBuf, Execution)> {
    if state.api_key.is_none() {
        return Err(error(StatusCode::UNAUTHORIZED, "api_key_required"));
    }
    let execution = state
        .responses
        .control
        .get(rid)
        .map_err(io_error)?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "response_not_found"))?;
    if execution.phase != "finished" {
        return Err(error(StatusCode::CONFLICT, "response_not_finished"));
    }
    let thread = execution
        .thread_id
        .as_deref()
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "thread_not_found"))?;
    if thread.is_empty()
        || !thread
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return Err(error(StatusCode::FORBIDDEN, "source_access_denied"));
    }
    let root = state
        .generated_images_root
        .as_ref()
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "generated_images_unavailable"))?
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
pub(crate) async fn list(
    State(state): State<AppState>,
    Path(rid): Path<String>,
) -> Result<Response> {
    let (root, execution) = scope(&state, &rid)?;
    let result=tokio::task::spawn_blocking(move || -> Result<_> {
        let entries=match std::fs::read_dir(&root) { Ok(v)=>v,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(Vec::new()),Err(e)=>return Err(io_error(e)) };
        let mut images=Vec::new();
        for entry in entries {
            let entry=entry.map_err(io_error)?;let name=entry.file_name().to_string_lossy().into_owned();
            if !allowed(&name,&entry.metadata().map_err(io_error)?,&execution) {continue;}
            if let Ok(file)=open_source(&root,&name) {
                if !allowed(&name,&file.metadata().map_err(io_error)?,&execution) {continue;}
                images.push(json!({"name":name,"content_type":"image/png","size_bytes":file.metadata().map_err(io_error)?.len()}));
            }
            if images.len()>16 {return Err(error(StatusCode::PAYLOAD_TOO_LARGE,"too_many_images"));}
        }
        images.sort_by_key(|v|v["name"].as_str().unwrap_or_default().to_owned());
        Ok(images)
    }).await.map_err(|_|error(StatusCode::SERVICE_UNAVAILABLE,"store_unavailable"))??;
    Ok(Json(json!({"data":result})).into_response())
}
pub(crate) async fn content(
    State(state): State<AppState>,
    Path((rid, name)): Path<(String, String)>,
) -> Result<Response> {
    let (root, execution) = scope(&state, &rid)?;
    let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut file = open_source(&root, &name)?;
        let before = file.metadata().map_err(io_error)?;
        if !allowed(&name, &before, &execution) {
            return Err(error(StatusCode::NOT_FOUND, "image_not_found"));
        }
        let mut data = Vec::new();
        (&mut file)
            .take(LIMIT + 1)
            .read_to_end(&mut data)
            .map_err(io_error)?;
        let after = file.metadata().map_err(io_error)?;
        if data.len() as u64 > LIMIT
            || before.len() != after.len()
            || before.modified().map_err(io_error)? != after.modified().map_err(io_error)?
        {
            return Err(error(StatusCode::CONFLICT, "source_changed"));
        }
        if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "invalid_image"));
        }
        Ok(data)
    })
    .await
    .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "store_unavailable"))??;
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

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

fn open_source(root: &FsPath, relative: &str) -> Result<File> {
    if relative.is_empty() || relative.contains(['/', '\\']) || relative == "." || relative == ".."
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_image_name"));
    }
    let root_metadata = std::fs::symlink_metadata(root).map_err(io_error)?;
    let relative =
        CString::new(relative).map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_image_name"))?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(io_error)?;
    let actual = directory.metadata().map_err(io_error)?;
    if (actual.dev(), actual.ino()) != (root_metadata.dev(), root_metadata.ino()) {
        return Err(error(StatusCode::CONFLICT, "source_changed"));
    }
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x01,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory.as_raw_fd(),
            relative.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOENT) => error(StatusCode::NOT_FOUND, "image_not_found"),
            _ => error(StatusCode::FORBIDDEN, "source_access_denied"),
        });
    }
    let file = unsafe { File::from_raw_fd(fd as i32) };
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(error(StatusCode::FORBIDDEN, "source_access_denied"));
    }
    Ok(file)
}
