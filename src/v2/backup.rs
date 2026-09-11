//! Explicit, offline restoration. No generation inference from file timestamps.
use super::{
    Error, Result, files, id,
    service::{Service, string},
    store::Store,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::Path,
};
fn digest(path: &Path) -> Result<String> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(Error::code(400, "invalid_backup"));
    }
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(value.to_string().as_bytes())?;
    f.sync_all()?;
    Ok(())
}
pub fn create(s: &Service, destination: &Path) -> Result<Value> {
    s.store.transaction(|tx| {
        super::service::ensure_ready(tx)?;
        for r in super::store::list(tx, "response")? {
            if r["hold_state"] == "held"
                || r["phase"] == "unknown"
                || r["output"]["state"] == "saving"
            {
                return Err(Error::code(409, "workspace_busy"));
            }
        }
        for r in super::store::list(tx, "artifact")? {
            if r["state"] == "creating" {
                return Err(Error::code(409, "capture_capacity_busy"));
            }
        }
        for r in super::store::list(tx, "legacy_hold")? {
            if r["state"] == "held" {
                return Err(Error::code(409, "workspace_busy"));
            }
        }
        tx.execute(
            "UPDATE metadata SET value='backup_blocked' WHERE key='recovery_state'",
            [],
        )?;
        Ok(())
    })?;
    let result = (|| {
        if !destination.is_absolute() || destination.starts_with(&s.store.root) {
            return Err(Error::code(400, "invalid_argument"));
        }
        std::fs::create_dir(destination)?;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
        let mut entries = Vec::new();
        let db = destination.join("metadata.sqlite3");
        s.store.backup_database(&db)?;
        entries.push(json!({ "path":"metadata.sqlite3","sha256":digest(&db)?}));
        std::fs::create_dir(destination.join("blobs"))?;
        for entry in std::fs::read_dir(s.store.root.join("blobs"))? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(Error::code(503, "store_corrupt"));
            }
            let relative = Path::new("blobs").join(entry.file_name());
            let target = destination.join(&relative);
            std::fs::copy(entry.path(), &target)?;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))?;
            File::open(&target)?.sync_all()?;
            entries.push(json!({ "path":relative,"sha256":digest(&target)?}));
        }
        let history = s
            .store
            .root
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("codex-home"));
        if let Some(history) = history.as_ref().filter(|p| p.is_dir()) {
            for entry in std::fs::read_dir(history)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| Error::code(400, "invalid_backup"))?;
                if name == "sessions"
                    || name == "archived_sessions"
                    || name.starts_with("state_") && name.ends_with(".sqlite")
                    || name == "session_index.jsonl"
                {
                    backup_tree(
                        &entry.path(),
                        destination,
                        &Path::new("history").join(name),
                        &mut entries,
                    )?;
                }
            }
        }
        let legacy = s.store.root.parent().unwrap().join("responses");
        if legacy.is_dir() {
            backup_tree(&legacy, destination, Path::new("legacy"), &mut entries)?;
        }
        let manifest = json!({
            "backup_id":id("backup"),
            "instance_id":s.store.instance,
            "recovery_generation":s.store.generation,
            "workspace_originals":"not_included",
            "codex_history":if history.is_some_and(|p|p.is_dir()){ "included"} else{ "absent"},
            "entries":entries
        });
        write_json(&destination.join("manifest.json"), &manifest)?;
        files::sync_directory(&destination.join("blobs"))?;
        files::sync_directory(destination)?;
        Ok(manifest)
    })();
    s.store.set_metadata("recovery_state", "ready")?;
    result
}
pub fn restore(root: &Path, bundle: &Path) -> Result<Value> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(bundle.join("manifest.json"))?)?;
    let entries = manifest["entries"]
        .as_array()
        .ok_or_else(|| Error::code(400, "invalid_backup"))?;
    for e in entries {
        let relative = string(e, "path")?;
        if relative != "metadata.sqlite3"
            && !(relative.starts_with("blobs/") && relative.split('/').count() == 2)
            && !relative.starts_with("history/")
            && !relative.starts_with("legacy/")
        {
            return Err(Error::code(400, "invalid_backup"));
        }
        if relative
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
            || relative.contains('\\')
        {
            return Err(Error::code(400, "invalid_backup"));
        }
        if digest(&bundle.join(relative))? != string(e, "sha256")? {
            return Err(Error::code(503, "content_corrupt"));
        }
    }
    let parent = root
        .parent()
        .ok_or_else(|| Error::code(400, "invalid_argument"))?;
    let _restore_lock = exclusive(&parent.join("v2-restore.lock"))?;
    let marker = parent.join("v2-restore-pending.json");
    let metadata: Value = if marker.exists() {
        let m: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
        if m["backup_id"] != manifest["backup_id"] {
            return Err(Error::code(409, "restore_in_progress"));
        }
        m
    } else {
        let m = json!({
            "restore_id":id("restore"),
            "generation":id("gen"),
            "backup_id":manifest["backup_id"]
        });
        // Verify that the service is stopped before creating a recovery marker.
        let _owner = exclusive(&root.join("owner.lock"))?;
        write_json(&marker, &m)?;
        files::sync_directory(parent)?;
        m
    };
    let restore = string(&metadata, "restore_id")?;
    let generation = string(&metadata, "generation")?;
    if !restore.starts_with("restore_")
        || !restore
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::code(503, "store_corrupt"));
    }
    let staging = parent.join(format!("v2-{restore}"));
    let previous = parent.join(format!("v2-before-{restore}"));
    let owner_path = if root.exists() { root } else { &previous };
    let _owner = exclusive(&owner_path.join("owner.lock"))?;
    let current_generation = if root.join("metadata.sqlite3").exists() {
        let db = rusqlite::Connection::open_with_flags(
            root.join("metadata.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Some(db.query_row(
            "SELECT value FROM metadata WHERE key='generation'",
            [],
            |r| r.get::<_, String>(0),
        )?)
    } else {
        None
    };
    if current_generation.as_deref() != Some(generation) {
        std::fs::create_dir_all(staging.join("blobs"))?;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
        let _staged = exclusive(&staging.join("owner.lock"))?;
        // Every retry starts from the verified immutable bundle. Never resumes execution.
        for suffix in ["metadata.sqlite3-wal", "metadata.sqlite3-shm"] {
            remove_if_exists(&staging.join(suffix))?;
        }
        for e in entries {
            let relative = string(e, "path")?;
            if relative.starts_with("history/") || relative.starts_with("legacy/") {
                continue;
            }
            atomic_copy(&bundle.join(relative), &staging.join(relative))?;
        }
        drop(_staged);
        {
            let restored = Store::open(&staging)?;
            if restored.instance != manifest["instance_id"] {
                return Err(Error::code(409, "instance_mismatch"));
            }
            restored.transaction(|tx| {
                for mut r in super::store::list(tx, "response")? {
                    if matches!(
                        r["phase"].as_str(),
                        Some("accepted" | "dispatching" | "started" | "unknown")
                    ) {
                        r["phase"] = json!("unknown");
                        r["execution_status"] = json!("unknown");
                        r["dispatch_eligible"] = json!(false);
                        super::store::put(tx, "response", string(&r, "response_id")?, &r)?;
                    }
                }
                tx.execute(
                    "UPDATE metadata SET value=?1 WHERE key='generation'",
                    [generation],
                )?;
                tx.execute(
                    "UPDATE metadata SET value='recovery_blocked' WHERE key='recovery_state'",
                    [],
                )?;
                Ok(())
            })?;
        }
        files::sync_directory(&staging.join("blobs"))?;
        files::sync_directory(&staging)?;
        if root.exists() {
            std::fs::rename(root, &previous)?;
        }
        std::fs::rename(&staging, root)?;
        files::sync_directory(parent)?;
    }
    // The marker continues to fence startup throughout auxiliary state restoration.
    for e in entries {
        let relative = string(e, "path")?;
        let target = if let Some(p) = relative.strip_prefix("history/") {
            parent
                .parent()
                .ok_or_else(|| Error::code(400, "invalid_argument"))?
                .join("codex-home")
                .join(p)
        } else if let Some(p) = relative.strip_prefix("legacy/") {
            parent.join("responses").join(p)
        } else {
            continue;
        };
        if target.extension().is_some_and(|e| e == "sqlite") {
            remove_if_exists(&target.with_extension("sqlite-wal"))?;
            remove_if_exists(&target.with_extension("sqlite-shm"))?;
        }
        atomic_copy(&bundle.join(relative), &target)?;
    }
    // Completion marker is recorded only after all roots have been synchronized.
    let db = rusqlite::Connection::open(root.join("metadata.sqlite3"))?;
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    db.execute("INSERT INTO metadata VALUES ('restore_complete',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[restore])?;
    Ok(json!({
        "restore_id":restore,
        "recovery_generation":generation,
        "state":"recovery_blocked",
        "previous_state":previous
    }))
}
fn exclusive(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::code(409, "store_in_use"));
    }
    Ok(file)
}
fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn atomic_copy(source: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| Error::code(400, "invalid_backup"))?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    let temp = parent.join(format!(".restore-{}", id("copy")));
    std::fs::copy(source, &temp)?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    File::open(&temp)?.sync_all()?;
    std::fs::rename(temp, target)?;
    files::sync_directory(parent)
}
fn backup_tree(
    source: &Path,
    bundle: &Path,
    relative: &Path,
    entries: &mut Vec<Value>,
) -> Result<()> {
    let m = std::fs::symlink_metadata(source)?;
    if m.is_dir() {
        std::fs::create_dir_all(bundle.join(relative))?;
        for e in std::fs::read_dir(source)? {
            let e = e?;
            backup_tree(&e.path(), bundle, &relative.join(e.file_name()), entries)?;
        }
        files::sync_directory(&bundle.join(relative))?;
    } else if m.is_file() {
        let target = bundle.join(relative);
        std::fs::create_dir_all(target.parent().unwrap())?;
        if source.extension().is_some_and(|e| e == "sqlite") {
            let db = rusqlite::Connection::open(source)?;
            db.busy_timeout(std::time::Duration::from_secs(5))?;
            db.execute(
                "VACUUM INTO ?1",
                [target
                    .to_str()
                    .ok_or_else(|| Error::code(400, "invalid_backup"))?],
            )?;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))?;
            File::open(&target)?.sync_all()?;
        } else {
            atomic_copy(source, &target)?;
        }
        entries.push(json!({ "path":relative,"sha256":digest(&target)?}));
    } else {
        return Err(Error::code(400, "invalid_backup"));
    }
    Ok(())
}
