//! Real process-death boundaries; only test-owned subprocesses and /tmp stores are used.
use codex_hoshikage_proxy::v2::{files, service::Service, store};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    time::Duration,
};

#[test]
#[ignore = "subprocess fixture invoked by crash_boundaries_keep_identity_and_never_recopy"]
fn crash_fixture() {
    let root = std::path::PathBuf::from(std::env::var("HOSHIKAGE_CRASH_TEST_ROOT").unwrap());
    let mode = std::env::var("HOSHIKAGE_CRASH_TEST_MODE").unwrap();
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    let c = s
        .conversation(
            "c",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    let cid = c["resource"]["id"].as_str().unwrap();
    let (_, rid) = s.accept(cid, "run", &json!({"input":"test"})).unwrap();
    let rid = rid.unwrap();
    if mode == "dispatching" {
        s.store
            .update("response", &rid, |v| {
                v["phase"] = json!("dispatching");
                Ok(())
            })
            .unwrap();
    }
    let mut aid = Value::Null;
    if matches!(mode.as_str(), "reserved" | "published" | "write_failure") {
        std::fs::write(
            s.workspace_path(cid).unwrap().join("source"),
            b"original bytes",
        )
        .unwrap();
        let (op, fresh) = s
            .reserve_capture(cid, "capture", &json!({"path":"source"}))
            .unwrap();
        assert!(fresh);
        aid = op["resource"]["id"].clone();
        if mode == "write_failure" {
            // Confine the real filesystem write error to this subprocess. Do not
            // fill a shared disk or change the test runner's resource limits.
            std::fs::write(
                s.workspace_path(cid).unwrap().join("source"),
                vec![7u8; 262144],
            )
            .unwrap();
            let mut old = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            unsafe {
                assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut old), 0);
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                let limited = libc::rlimit {
                    rlim_cur: 65536,
                    rlim_max: old.rlim_max,
                };
                assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limited), 0);
            }
            let result = s.finish_capture(aid.as_str().unwrap(), "capture");
            unsafe {
                assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &old), 0);
            }
            assert!(!result.is_ok_and(|op| op["state"] == "succeeded"));
            assert!(
                !s.store
                    .root
                    .join("blobs")
                    .join(aid.as_str().unwrap())
                    .exists()
            );
        }
        if mode == "published" {
            let staging = s.store.root.join("staging").join(aid.as_str().unwrap());
            let mut source = files::open_source(&s.workspace_path(cid).unwrap(), "source").unwrap();
            let (size, hash) = files::copy_with_timeout(&mut source, &staging, 1024, 10).unwrap();
            files::publish(&staging,&s.store.root.join("blobs"),aid.as_str().unwrap(),&json!({
                "state":"ready","size_bytes":size,"sha256":hash,"media_type":"application/octet-stream",
                "ready_at_ms":1,"expires_at_ms":u64::MAX/2,"max_hold_until_ms":u64::MAX/2
            })).unwrap();
        }
    }
    // Prove that an in-flight SQLite transaction is rolled back by process death.
    s.store.transaction(|tx| -> codex_hoshikage_proxy::v2::Result<()> {
        store::put(tx,"test","uncommitted",&json!({"must_not_survive":true}))?;
        println!("CRASH_READY {}",json!({"cid":cid,"rid":rid,"aid":aid,"instance":s.store.instance,"generation":s.store.generation}));
        std::io::stdout().flush().unwrap();
        loop {std::thread::sleep(Duration::from_secs(60));}
    }).unwrap();
}

#[test]
fn crash_boundaries_keep_identity_and_never_recopy() {
    for mode in [
        "accepted",
        "dispatching",
        "reserved",
        "published",
        "write_failure",
    ] {
        let root = std::env::temp_dir().join(format!("hoshikage-crash-{}", uuid::Uuid::new_v4()));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "crash_fixture", "--nocapture"])
            .env("HOSHIKAGE_CRASH_TEST_ROOT", &root)
            .env("HOSHIKAGE_CRASH_TEST_MODE", mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some(value) = line.strip_prefix("CRASH_READY ") {
                    let _ = tx.send(serde_json::from_str::<Value>(value).unwrap());
                    break;
                }
            }
        });
        let result = rx.recv_timeout(Duration::from_secs(10));
        let _ = child.kill();
        child.wait().unwrap();
        reader.join().unwrap();
        let ids = result.expect("child must reach the selected crash boundary");
        let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
        assert_eq!(s.store.instance, ids["instance"]);
        assert_eq!(s.store.generation, ids["generation"]);
        assert!(s.store.get("test", "uncommitted").is_err());
        let cid = ids["cid"].as_str().unwrap();
        let rid = ids["rid"].as_str().unwrap();
        let r = s.store.get("response", rid).unwrap();
        assert_eq!(
            r["phase"],
            if mode == "dispatching" {
                "unknown"
            } else {
                "accepted"
            }
        );
        let (op, launch) = s.accept(cid, "run", &json!({"input":"test"})).unwrap();
        assert!(launch.is_none());
        assert_eq!(op["resource"]["id"], rid);
        if let Some(aid) = ids["aid"].as_str() {
            std::fs::write(
                s.workspace_path(cid).unwrap().join("source"),
                b"changed after crash",
            )
            .unwrap();
            let replay = s
                .capture(cid, "capture", &json!({"path":"source"}))
                .unwrap();
            assert_eq!(replay["resource"]["id"], aid);
            if mode == "write_failure" {
                assert!(replay["state"] == "failed" || replay["state"] == "unknown");
            } else {
                assert_eq!(
                    replay["state"],
                    if mode == "published" {
                        "succeeded"
                    } else {
                        "unknown"
                    }
                );
            }
            if mode == "published" {
                assert_eq!(
                    std::fs::read(s.store.root.join("blobs").join(aid)).unwrap(),
                    b"original bytes"
                );
            } else {
                assert!(!s.store.root.join("blobs").join(aid).exists());
            }
        }
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
}
