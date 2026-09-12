//! Run only through scripts/test_v2_full_fs.py in a bounded private tmpfs.
#![cfg(target_os = "linux")]
use codex_hoshikage_proxy::v2::{limits::Limits, service::Service, store};
use serde_json::json;
use std::{fs::File, io::Write, path::Path};

fn fill(root: &Path) -> std::path::PathBuf {
    let path = root.join("filler");
    let mut file = File::create(&path).unwrap();
    let mut total = 0;
    loop {
        match file.write_all(&[7; 4096]) {
            Ok(()) => {
                total += 4096;
                assert!(total <= 16 * 1024 * 1024);
            }
            Err(e) => {
                assert_eq!(e.raw_os_error(), Some(libc::ENOSPC));
                break;
            }
        }
    }
    assert!(total > 0);
    path
}

#[test]
#[ignore = "requires private 16 MiB tmpfs; use scripts/test_v2_full_fs.py"]
fn actual_full_filesystem_preserves_committed_state() {
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::path::PathBuf::from(std::env::var("HOSHIKAGE_PARENT_MNT").unwrap())
    );
    let root = std::path::PathBuf::from(std::env::var("HOSHIKAGE_FULL_FS_ROOT").unwrap());
    let cpath = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    assert_eq!(
        unsafe { libc::statfs(cpath.as_ptr(), stat.as_mut_ptr()) },
        0
    );
    let stat = unsafe { stat.assume_init() };
    assert_eq!(stat.f_type, libc::TMPFS_MAGIC);
    assert!(stat.f_blocks as u128 * stat.f_bsize as u128 <= 16 * 1024 * 1024);
    let limits = Limits {
        disk_free_floor_bytes: 0,
        artifact_max_bytes: 65536,
        output_max_bytes: 65536,
        execution_input_max_bytes: 65536,
        ..Limits::default()
    };
    for mode in ["artifact", "sqlite"] {
        let case = root.join(mode);
        let state = case.join("state");
        let work = case.join("work");
        let s = Service::open_with_limits(&state, &work, limits.clone()).unwrap();
        let identity = (s.store.instance.clone(), s.store.generation.clone());
        let c = s
            .conversation(
                "c",
                &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
            )
            .unwrap();
        let cid = c["resource"]["id"].as_str().unwrap();
        let (_, launch) = s
            .accept(cid, "run", &json!({"input":"one execution"}))
            .unwrap();
        let rid = launch.unwrap();
        s.store
            .update("response", &rid, |r| {
                r["phase"] = json!("dispatching");
                Ok(())
            })
            .unwrap();
        let mut aid = None;
        if mode == "artifact" {
            std::fs::write(s.workspace_path(cid).unwrap().join("source"), b"original").unwrap();
            let (op, fresh) = s
                .reserve_capture(cid, "capture", &json!({"path":"source"}))
                .unwrap();
            assert!(fresh);
            aid = Some(op["resource"]["id"].as_str().unwrap().to_owned());
        } else {
            s.store
                .transaction(|tx| {
                    tx.execute_batch("CREATE TABLE fault_payload(data BLOB)")?;
                    Ok(())
                })
                .unwrap();
        }
        let filler = fill(&case);
        if let Some(aid) = &aid {
            let result = s.finish_capture(aid, "capture");
            assert!(!result.is_ok_and(|op| op["state"] == "succeeded"));
            assert!(!state.join("blobs").join(aid).exists());
        } else {
            let mut saw_full = false;
            let result = s.store.transaction(|tx| {
                store::reserve(tx, "not-committed", "test", &json!({}))?;
                // Exceeds the entire bounded filesystem, including any reusable WAL space.
                let error = tx.execute("INSERT INTO fault_payload VALUES (zeroblob(33554432))", []).unwrap_err();
                saw_full = matches!(error, rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::DiskFull);
                Err::<(), _>(error.into())
            });
            assert!(result.is_err());
            assert!(
                saw_full,
                "SQLite must report SQLITE_FULL, not an injected proxy error"
            );
        }
        std::fs::remove_file(filler).unwrap();
        if aid.is_some() {
            std::fs::write(s.workspace_path(cid).unwrap().join("source"), b"changed").unwrap();
        }
        drop(s);
        let s = Service::open_with_limits(&state, &work, limits.clone()).unwrap();
        assert_eq!(
            (s.store.instance.clone(), s.store.generation.clone()),
            identity
        );
        assert_eq!(s.store.get("response", &rid).unwrap()["phase"], "unknown");
        let (op, launch) = s
            .accept(cid, "run", &json!({"input":"one execution"}))
            .unwrap();
        assert!(launch.is_none());
        assert_eq!(op["resource"]["id"], rid);
        s.store
            .transaction(|tx| {
                let integrity: String = tx.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
                assert_eq!(integrity, "ok");
                assert!(store::operation(tx, "not-committed")?.is_none());
                if mode == "sqlite" {
                    assert_eq!(
                        tx.query_row("SELECT count(*) FROM fault_payload", [], |r| r
                            .get::<_, i64>(0))?,
                        0
                    );
                }
                Ok(())
            })
            .unwrap();
        if let Some(aid) = aid {
            // The original capture key is retained; recovery never copies the changed source.
            let op = s
                .capture(cid, "capture", &json!({"path":"source"}))
                .unwrap();
            assert_eq!(op["resource"]["id"], aid);
            assert_ne!(op["state"], "succeeded");
            assert!(!state.join("blobs").join(aid).exists());
        }
        println!("PASS {mode}: real ENOSPC, identity preserved, integrity ok, no replay");
        drop(s);
        std::fs::remove_dir_all(case).unwrap();
    }
}
