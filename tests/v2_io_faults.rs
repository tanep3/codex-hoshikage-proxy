//! Process-local syscall fault injection; never fills or mounts a shared disk.
#![cfg(target_os = "linux")]
use codex_hoshikage_proxy::v2::service::Service;
use serde_json::json;
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

#[test]
#[ignore = "isolated subprocess used by storage_faults_do_not_publish_unconfirmed_data"]
fn io_fault_fixture() {
    let root = PathBuf::from(std::env::var("HOSHIKAGE_IO_ROOT").unwrap());
    let mode = std::env::var("HOSHIKAGE_IO_MODE").unwrap();
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    let c = s
        .conversation(
            "c",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    let cid = c["resource"]["id"].as_str().unwrap();
    let source = s.workspace_path(cid).unwrap().join("source");
    std::fs::write(&source, b"original immutable bytes").unwrap();
    let (op, fresh) = s
        .reserve_capture(cid, "capture", &json!({"path":"source"}))
        .unwrap();
    assert!(fresh);
    let aid = op["resource"]["id"].as_str().unwrap();
    std::fs::write(root.join("armed"), b"active").unwrap();
    let result = s.finish_capture(aid, "capture").unwrap();
    assert_eq!(result["state"], "failed");
    assert!(
        root.join("hit").exists(),
        "the selected syscall must actually fail"
    );
    assert_ne!(s.store.get("artifact", aid).unwrap()["state"], "ready");
    std::fs::write(&source, b"must never replace the saved content").unwrap();
    // A matching hash in page cache is insufficient when sync keeps failing.
    drop(s);
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    assert_ne!(
        s.store.get("artifact", aid).unwrap()["state"],
        "ready",
        "recovery must confirm durability before publishing {mode}"
    );
    std::fs::remove_file(root.join("armed")).unwrap();
    drop(s);
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    let replay = s
        .capture(cid, "capture", &json!({"path":"source"}))
        .unwrap();
    assert_eq!(replay["resource"]["id"], aid);
    if matches!(mode.as_str(), "sync_manifest" | "sync_directory") {
        assert_eq!(replay["state"], "succeeded");
        assert!(replay["error"].is_null());
        assert_eq!(
            std::fs::read(s.store.root.join("blobs").join(aid)).unwrap(),
            b"original immutable bytes"
        );
        // A transient sync error on previously ready data is not corruption.
        std::fs::write(root.join("armed"), b"active").unwrap();
        drop(s);
        let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
        assert_eq!(s.store.get("artifact", aid).unwrap()["state"], "unknown");
        std::fs::remove_file(root.join("armed")).unwrap();
        drop(s);
        let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
        assert_eq!(s.store.get("artifact", aid).unwrap()["state"], "ready");
    } else {
        assert_ne!(replay["state"], "succeeded");
        assert!(!s.store.root.join("blobs").join(aid).exists());
    }
}

#[test]
fn storage_faults_do_not_publish_unconfirmed_data() {
    let root = std::env::temp_dir().join(format!("hoshikage-io-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let library = root.join("fault.so");
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/io_fault.c"
            ))
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    for mode in [
        "write_staging",
        "sync_staging",
        "sync_manifest",
        "sync_directory",
    ] {
        let case = root.join(mode);
        std::fs::create_dir_all(&case).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "io_fault_fixture", "--nocapture"])
            .env("LD_PRELOAD", &library)
            .env("HOSHIKAGE_IO_ROOT", &case)
            .env("HOSHIKAGE_IO_MODE", mode)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "fault scenario failed: {mode}");
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("fault subprocess timed out: {mode}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
