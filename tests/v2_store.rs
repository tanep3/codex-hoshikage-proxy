use codex_hoshikage_proxy::v2::{
    files,
    service::Service,
    store::{self, Store},
};
use serde_json::json;
use std::{os::unix::fs::symlink, path::PathBuf};
fn root() -> PathBuf {
    std::env::temp_dir().join(format!("hoshikage-v2-{}", uuid::Uuid::new_v4()))
}
fn conversation(s: &Service) -> String {
    let op = s
        .conversation(
            "conversation",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    op["resource"]["id"].as_str().unwrap().into()
}
#[test]
fn single_owner_and_stable_identity() {
    let r = root();
    let s = Store::open(&r).unwrap();
    assert!(Store::open(&r).is_err());
    let i = s.instance.clone();
    let g = s.generation.clone();
    drop(s);
    let s = Store::open(&r).unwrap();
    assert_eq!(s.instance, i);
    assert_eq!(s.generation, g);
}
#[test]
fn schema_two_migrates_without_rewriting_legacy_records_and_future_versions_fail() {
    let r = root();
    let s = Store::open(&r).unwrap();
    s.transaction(|tx| {
        tx.execute("UPDATE metadata SET value='2' WHERE key='schema'", [])?;
        store::put(
            tx,
            "response",
            "legacy",
            &json!({"response_id":"legacy","phase":"accepted","input":"kept"}),
        )
    })
    .unwrap();
    let instance = s.instance.clone();
    drop(s);
    let s = Store::open(&r).unwrap();
    assert_eq!(s.instance, instance);
    assert_eq!(
        s.get("response", "legacy").unwrap(),
        json!({"response_id":"legacy","phase":"accepted","input":"kept"})
    );
    s.transaction(|tx| {
        let version: String =
            tx.query_row("SELECT value FROM metadata WHERE key='schema'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(version, "3");
        tx.execute("UPDATE metadata SET value='4' WHERE key='schema'", [])?;
        Ok(())
    })
    .unwrap();
    drop(s);
    assert!(matches!(Store::open(&r),Err(e) if e.code=="schema_mismatch"));
}
#[test]
fn preparation_recovery_never_replays_an_unacknowledged_mutation() {
    use codex_hoshikage_proxy::v2::approval_policy as policy;
    let r = root();
    let s = Store::open(&r).unwrap();
    let clock = policy::Clock::read().unwrap();
    for stage in [
        "unsent",
        "config_unknown",
        "ready",
        "turn_unknown",
        "stopped",
    ] {
        let (mut public, mut private) = policy::initial(None, &clock).unwrap();
        if stage != "unsent" {
            policy::configuration_intent(&mut public, &mut private, "runtime", &clock).unwrap();
        }
        if matches!(stage, "ready" | "turn_unknown") {
            policy::ready(&mut public, &mut private, "proof", &clock).unwrap();
        }
        if stage == "turn_unknown" {
            policy::turn_intent(&mut public, &private, false, &clock).unwrap();
        }
        s.transaction(|tx|store::put(tx,"response",stage,&json!({"response_id":stage,
            "phase":"dispatching","execution_status":"not_started","hold_state":"held","hold_revision":1,
            "approval_presentation":{"profile":policy::PROFILE},"approval_policy":public,
            "_approval_prepare":private,"stop_requested":stage=="stopped","input":"original","output":{"state":"pending"}}))).unwrap();
    }
    let deadline =
        s.get("response", "ready").unwrap()["approval_policy"]["preparation"]["deadline_at"]
            .clone();
    drop(s);
    let s = Store::open(&r).unwrap();
    for stage in ["unsent", "ready"] {
        let record = s.get("response", stage).unwrap();
        assert_eq!(record["phase"], "accepted");
        assert_eq!(record["execution_status"], "not_started");
    }
    assert_eq!(
        s.get("response", "ready").unwrap()["approval_policy"]["preparation"]["deadline_at"],
        deadline
    );
    for stage in ["config_unknown", "stopped"] {
        let record = s.get("response", stage).unwrap();
        assert_eq!(record["phase"], "unknown");
        assert_eq!(record["execution_status"], "not_started");
        assert_eq!(record["hold_state"], "held");
        assert_eq!(record["dispatch_eligible"], false);
    }
    let record = s.get("response", "turn_unknown").unwrap();
    assert_eq!(record["phase"], "unknown");
    assert_eq!(record["execution_status"], "unknown");
    drop(s);
    let s = Store::open(&r).unwrap();
    assert_eq!(
        s.get("response", "config_unknown").unwrap()["dispatch_eligible"],
        false
    );
}
#[test]
fn operation_transaction_rolls_back() {
    let r = root();
    let s = Store::open(&r).unwrap();
    let result: codex_hoshikage_proxy::v2::Result<()> = s.transaction(|tx| {
        store::reserve(tx, "key", "test", &json!({}))?;
        Err(codex_hoshikage_proxy::v2::Error::code(409, "test"))
    });
    assert!(result.is_err());
    assert!(
        s.transaction(|tx| store::operation(tx, "key"))
            .unwrap()
            .is_none()
    );
}
#[test]
fn cancellation_before_acceptance_survives_restart() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    s.stop(
        "stop",
        &json!({"target":{"conversation_id":cid,"request_key":"run"}}),
    )
    .unwrap();
    drop(s);
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let (op, launch) = s.accept(&cid, "run", &json!({"input":"test"})).unwrap();
    assert!(launch.is_none());
    let rid = op["resource"]["id"].as_str().unwrap();
    assert_eq!(s.store.get("response", rid).unwrap()["phase"], "cancelled");
}
#[test]
fn cancellation_and_dispatch_have_one_winner() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    let (_, rid) = s.accept(&cid, "run", &json!({"input":"test"})).unwrap();
    let rid = rid.unwrap();
    s.stop("stop", &json!({"target":{"response_id":rid}}))
        .unwrap();
    assert_eq!(s.store.get("response", &rid).unwrap()["phase"], "cancelled");
    let (_, launch) = s.accept(&cid, "run", &json!({"input":"test"})).unwrap();
    assert!(launch.is_none());
}
#[test]
fn immutable_capture_replay_does_not_read_changed_source() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    let w = s.workspace_path(&cid).unwrap();
    std::fs::write(w.join("report.txt"), "first").unwrap();
    let body = json!({"path":"report.txt"});
    let a = s.capture(&cid, "capture", &body).unwrap();
    std::fs::write(w.join("report.txt"), "second").unwrap();
    let b = s.capture(&cid, "capture", &body).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        std::fs::read_to_string(
            s.store
                .root
                .join("blobs")
                .join(a["resource"]["id"].as_str().unwrap())
        )
        .unwrap(),
        "first"
    );
}
#[test]
fn traversal_symlink_hardlink_and_fifo_are_rejected() {
    let r = root();
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(r.join("data"), "safe").unwrap();
    symlink("data", r.join("link")).unwrap();
    assert!(files::open_source(&r, "link").is_err());
    assert!(files::open_source(&r, "../data").is_err());
    assert!(files::open_source(&r, "/etc/passwd").is_err());
    std::fs::hard_link(r.join("data"), r.join("hard")).unwrap();
    assert!(files::open_source(&r, "hard").is_err());
    let path = std::ffi::CString::new(r.join("fifo").to_str().unwrap()).unwrap();
    unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert!(files::open_source(&r, "fifo").is_err());
}

#[test]
fn audited_release_keeps_unknown_and_fences_old_conversation() {
    use codex_hoshikage_proxy::v2::admin;
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    let (_, rid) = s.accept(&cid, "run", &json!({"input":"test"})).unwrap();
    let rid = rid.unwrap();
    s.store
        .update("response", &rid, |r| {
            r["phase"] = json!("unknown");
            r["execution_status"] = json!("unknown");
            Ok(())
        })
        .unwrap();
    let review = admin::execute(
        &s,
        &json!({"action":"execution-hold.inspect","response_id":rid}),
    )
    .unwrap();
    let request = json!({"action":"execution-hold.release","response_id":rid,"operation_id":"release","expected_revision":review["hold_revision"],"review_token":review["review_token"],"reason":"verified ambiguous upstream","accept_risk":true});
    let a = admin::execute(&s, &request).unwrap();
    let b = admin::execute(&s, &request).unwrap();
    assert_eq!(a, b);
    let r = s.store.get("response", &rid).unwrap();
    assert_eq!(r["execution_status"], "unknown");
    assert_eq!(r["dispatch_eligible"], false);
    assert_eq!(r["hold_state"], "administratively_released");
    assert!(s.accept(&cid, "new", &json!({"input":"test"})).is_err());
}
#[test]
fn lease_prevents_gc_and_release_does_not_extend_base_expiry() {
    use codex_hoshikage_proxy::v2::{now, retention};
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    std::fs::write(s.workspace_path(&cid).unwrap().join("x"), "content").unwrap();
    let a = s.capture(&cid, "a", &json!({"path":"x"})).unwrap();
    let aid = a["resource"]["id"].as_str().unwrap();
    let time = retention::wire(json!({"hold_until_ms":now()+600000}))["hold_until"].clone();
    let lease = retention::lease(
        &s,
        &["leases".into()],
        "l",
        &json!({"resource":{"type":"artifact","id":aid},"hold_until":time}),
    )
    .unwrap();
    s.store
        .update("artifact", aid, |a| {
            a["expires_at_ms"] = json!(now() - 1);
            Ok(())
        })
        .unwrap();
    retention::gc(&s).unwrap();
    assert!(s.store.root.join("blobs").join(aid).exists());
    retention::lease(
        &s,
        &[
            "leases".into(),
            lease["lease_id"].as_str().unwrap().into(),
            "release".into(),
        ],
        "release",
        &json!({}),
    )
    .unwrap();
    retention::gc(&s).unwrap();
    assert!(!s.store.root.join("blobs").join(aid).exists());
    assert_eq!(s.store.get("artifact", aid).unwrap()["state"], "expired");
}
#[test]
fn ranges_reject_overflow_and_multiple_parts() {
    use codex_hoshikage_proxy::v2::download::parse_range;
    assert_eq!(parse_range("bytes=2-4", 10).unwrap(), (2, 4));
    assert_eq!(parse_range("bytes=-3", 10).unwrap(), (7, 9));
    assert!(parse_range("bytes=10-", 10).is_err());
    assert!(parse_range("bytes=0-1,3-4", 10).is_err());
    assert!(parse_range("bytes=18446744073709551616-", 10).is_err());
}

#[test]
fn completed_blob_is_recovered_after_metadata_commit_loss() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    std::fs::write(s.workspace_path(&cid).unwrap().join("report"), "fixed").unwrap();
    let a = s
        .capture(&cid, "capture", &json!({"path":"report"}))
        .unwrap();
    let aid = a["resource"]["id"].as_str().unwrap().to_owned();
    s.store
        .update("artifact", &aid, |a| {
            a["state"] = json!("creating");
            a.as_object_mut().unwrap().remove("sha256");
            a.as_object_mut().unwrap().remove("size_bytes");
            Ok(())
        })
        .unwrap();
    drop(s);
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    assert_eq!(s.store.get("artifact", &aid).unwrap()["state"], "ready");
    assert_eq!(
        std::fs::read_to_string(s.store.root.join("blobs").join(&aid)).unwrap(),
        "fixed"
    );
}
#[test]
fn formal_restore_changes_generation_and_blocks_admission() {
    use codex_hoshikage_proxy::v2::backup;
    let r = root();
    let state = r.join("state");
    let s = Service::open(&state, &r.join("work")).unwrap();
    let cid = conversation(&s);
    let original = s.store.generation.clone();
    let bundle = r.join("backup");
    backup::create(&s, &bundle).unwrap();
    drop(s);
    let restored = backup::restore(&state, &bundle).unwrap();
    assert_ne!(restored["recovery_generation"], original);
    let s = Service::open(&state, &r.join("work")).unwrap();
    assert_eq!(
        s.store.metadata("recovery_state").unwrap(),
        "recovery_blocked"
    );
    assert!(
        s.accept(&cid, "new", &json!({"input":"must not run"}))
            .is_err()
    );
    assert!(s.store.get("conversation", &cid).is_ok());
}
#[test]
fn listing_cursor_does_not_include_new_versions() {
    use codex_hoshikage_proxy::v2::listing;
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    std::fs::write(s.workspace_path(&cid).unwrap().join("r"), "v").unwrap();
    for key in ["a", "b"] {
        s.capture(&cid, key, &json!({"path":"r"})).unwrap();
    }
    let path = vec!["conversations".into(), cid.clone(), "artifacts".into()];
    let first = listing::list(&s, &path, Some("limit=1")).unwrap();
    let cursor = first["next_cursor"].as_str().unwrap();
    s.capture(&cid, "c", &json!({"path":"r"})).unwrap();
    let second = listing::list(&s, &path, Some(&format!("limit=1&cursor={cursor}"))).unwrap();
    assert_eq!(second["data"].as_array().unwrap().len(), 1);
    assert!(second["next_cursor"].is_null());
    assert_ne!(
        first["data"][0]["artifact_id"],
        second["data"][0]["artifact_id"]
    );
}

#[test]
fn cancellation_fence_cannot_be_reassigned_to_another_conversation() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    let other = s
        .conversation(
            "other",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap()["resource"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.stop(
        "stop1",
        &json!({"target":{"conversation_id":cid,"request_key":"future"}}),
    )
    .unwrap();
    assert_eq!(
        s.stop(
            "stop2",
            &json!({"target":{"conversation_id":other,"request_key":"future"}})
        )
        .unwrap_err()
        .code,
        "target_mismatch"
    );
    assert!(
        s.accept(&cid, "future", &json!({"input":"test"}))
            .unwrap()
            .1
            .is_none()
    );
}
#[test]
fn simultaneous_shared_workspace_acceptance_has_one_winner() {
    use std::sync::{Arc, Barrier};
    let r = root();
    let s = Arc::new(Service::open(&r.join("state"), &r.join("work")).unwrap());
    let cid = conversation(&s);
    let c = s.store.get("conversation", &cid).unwrap();
    let wid = c["workspace_id"].as_str().unwrap();
    s.store
        .update("workspace", wid, |w| {
            w["mode"] = json!("shared");
            Ok(())
        })
        .unwrap();
    let other = s
        .conversation(
            "other",
            &json!({"workspace":{"mode":"shared","workspace_id":wid},"model":"chatgpt/test"}),
        )
        .unwrap()["resource"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [cid, other]
        .into_iter()
        .enumerate()
        .map(|(i, cid)| {
            let s = s.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                s.accept(&cid, &format!("run-{i}"), &json!({"input":"test"}))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .next()
            .unwrap()
            .code,
        "workspace_busy"
    );
}
#[test]
fn pending_capture_is_not_recaptured_after_restart() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    std::fs::write(s.workspace_path(&cid).unwrap().join("file.txt"), "original").unwrap();
    let body = json!({"path":"file.txt"});
    let (op, fresh) = s.reserve_capture(&cid, "capture", &body).unwrap();
    assert!(fresh);
    let aid = op["resource"]["id"].as_str().unwrap().to_owned();
    drop(s);
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    std::fs::write(
        s.workspace_path(&cid).unwrap().join("file.txt"),
        "replacement",
    )
    .unwrap();
    assert_eq!(
        s.capture(&cid, "capture", &body).unwrap()["state"],
        "unknown"
    );
    assert!(!s.store.root.join("blobs").join(aid).exists());
}
#[test]
fn revocation_between_reservation_and_copy_prevents_publication() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    let cid = conversation(&s);
    std::fs::write(s.workspace_path(&cid).unwrap().join("file.txt"), "private").unwrap();
    let (op, _) = s
        .reserve_capture(&cid, "capture", &json!({"path":"file.txt"}))
        .unwrap();
    let aid = op["resource"]["id"].as_str().unwrap();
    let wid = s.store.get("conversation", &cid).unwrap()["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.store
        .update("workspace", &wid, |w| {
            w["state"] = json!("revoked");
            Ok(())
        })
        .unwrap();
    assert_eq!(s.finish_capture(aid, "capture").unwrap()["state"], "failed");
    assert!(!s.store.root.join("blobs").join(aid).exists());
}
#[test]
fn backup_does_not_release_recovery_block() {
    let r = root();
    let s = Service::open(&r.join("state"), &r.join("work")).unwrap();
    s.store
        .set_metadata("recovery_state", "recovery_blocked")
        .unwrap();
    assert_eq!(
        codex_hoshikage_proxy::v2::backup::create(&s, &r.join("backup"))
            .unwrap_err()
            .code,
        "recovery_blocked"
    );
    assert_eq!(
        s.store.metadata("recovery_state").unwrap(),
        "recovery_blocked"
    );
}

#[test]
fn restore_resumes_after_marker_and_partial_staging_copy() {
    use codex_hoshikage_proxy::v2::{admin, backup};
    let r = root();
    let state = r.join("state");
    let s = Service::open(&state, &r.join("work")).unwrap();
    let cid = conversation(&s);
    let manifest = backup::create(&s, &r.join("backup")).unwrap();
    drop(s);
    let restore = "restore_partial_test";
    let generation = "gen_partial_test";
    std::fs::write(
        r.join("v2-restore-pending.json"),
        json!({"restore_id":restore,"generation":generation,"backup_id":manifest["backup_id"]})
            .to_string(),
    )
    .unwrap();
    std::fs::create_dir(r.join(format!("v2-{restore}"))).unwrap();
    std::fs::write(
        r.join(format!("v2-{restore}/metadata.sqlite3")),
        b"incomplete",
    )
    .unwrap();
    let result = backup::restore(&state, &r.join("backup")).unwrap();
    assert_eq!(result["recovery_generation"], generation);
    let again = backup::restore(&state, &r.join("backup")).unwrap();
    assert_eq!(result, again);
    let s = Service::open(&state, &r.join("work")).unwrap();
    assert!(s.store.get("conversation", &cid).is_ok());
    let release = json!({"action":"recovery.release","restore_id":restore,"generation":generation,"accept_risk":true,"reason":"verified isolated restore"});
    let first = admin::execute(&s, &release).unwrap();
    let second = admin::execute(&s, &release).unwrap();
    assert_eq!(first, second);
}
#[test]
fn provider_reservation_is_shared_with_legacy_clients() {
    use codex_hoshikage_proxy::v2::coordination;
    use std::sync::Arc;
    let r = root();
    let s = Arc::new(Service::open(&r.join("state"), &r.join("work")).unwrap());
    let cid = conversation(&s);
    let legacy = r.join("legacy");
    std::fs::create_dir(&legacy).unwrap();
    let guard = coordination::reserve_legacy(s.clone(), &legacy, "legacy-r", "chatgpt", 1).unwrap();
    assert_eq!(
        s.accept_with_provider_limit(&cid, "run", &json!({"input":"test"}), Some(1))
            .unwrap_err()
            .code,
        "provider_busy"
    );
    drop(guard); // No upstream dispatch took place; cancellation releases the reservation.
    assert!(
        s.accept_with_provider_limit(&cid, "run", &json!({"input":"test"}), Some(1))
            .unwrap()
            .1
            .is_some()
    );
}

#[test]
#[ignore = "explicit 512 MiB local capacity measurement"]
fn measure_two_maximum_artifact_copies() {
    use std::sync::Arc;
    let r = root();
    let s = Arc::new(Service::open(&r.join("state"), &r.join("work")).unwrap());
    let cid = conversation(&s);
    let source = s.workspace_path(&cid).unwrap().join("large.bin");
    std::fs::File::create(&source)
        .unwrap()
        .set_len(s.limits.artifact_max_bytes)
        .unwrap();
    let start = std::time::Instant::now();
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let svc = s.clone();
            let cid = cid.clone();
            std::thread::spawn(move || {
                svc.capture(&cid, &format!("capture-{i}"), &json!({"path":"large.bin"}))
                    .unwrap()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap()["state"], "succeeded");
    }
    let capacity = s.capacity().unwrap();
    assert_eq!(
        capacity["artifacts"]["used_bytes"],
        s.limits.artifact_max_bytes * 2
    );
    println!(
        "two_256mib_copies_seconds={:.3}",
        start.elapsed().as_secs_f64()
    );
    drop(s);
    std::fs::remove_dir_all(&r).unwrap();
}

#[tokio::test]
async fn legacy_migration_preserves_keys_and_failed_response_ids() {
    use codex_hoshikage_proxy::{
        control::Execution,
        store::{ResponseMapping, ResponseStore},
    };
    let r = root();
    let old = ResponseStore::open(&r).await.unwrap();
    old.put(ResponseMapping {
        response_id: "resp_7".into(),
        thread_id: "thread-old".into(),
        model_id: "chatgpt/test".into(),
    })
    .await
    .unwrap();
    let mut record = Execution::new(
        "resp_8".into(),
        Some("old-key".into()),
        Some("old-fingerprint".into()),
    );
    record.phase = "rejected".into();
    old.control.reserve(record).unwrap();
    drop(old);
    let service = Service::open(&r.join("state/v2"), &root().join("work")).unwrap();
    let migrated = ResponseStore::open(&r).await.unwrap();
    assert_eq!(migrated.next_response_id(), "resp_9");
    assert_eq!(
        migrated
            .control
            .by_request("old-key")
            .unwrap()
            .unwrap()
            .fingerprint
            .as_deref(),
        Some("old-fingerprint")
    );
    assert_eq!(
        migrated.get("resp_7").await.unwrap().thread_id,
        "thread-old"
    );
    let original = std::fs::read(r.join("state/responses/executions.jsonl")).unwrap();
    migrated
        .control
        .reserve(Execution::new(
            "resp_9".into(),
            Some("new-key".into()),
            None,
        ))
        .unwrap();
    assert_eq!(
        std::fs::read(r.join("state/responses/executions.jsonl")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(r.join("state/responses/pre-v2/executions.jsonl")).unwrap(),
        original
    );
    drop(migrated);
    let reopened = ResponseStore::open(&r).await.unwrap();
    assert_eq!(reopened.next_response_id(), "resp_10");
    assert!(reopened.control.by_request("new-key").unwrap().is_some());
    assert!(service.store.metadata("legacy_migrated").is_ok());
}

#[tokio::test]
async fn migrated_unknown_without_cwd_keeps_global_hold_until_audited_release() {
    use codex_hoshikage_proxy::{control::Execution, store::ResponseStore, v2::admin};
    let r = root();
    let old = ResponseStore::open(&r).await.unwrap();
    let mut e = Execution::new("resp_5".into(), Some("legacy-run".into()), None);
    e.phase = "unknown".into();
    e.thread_id = Some("legacy-thread".into());
    old.control.reserve(e).unwrap();
    drop(old);
    let work = root();
    let s = Service::open(&r.join("state/v2"), &work).unwrap();
    let migrated = ResponseStore::open(&r).await.unwrap();
    let cid = conversation(&s);
    assert_eq!(
        s.accept(&cid, "run", &json!({"input":"test"}))
            .unwrap_err()
            .code,
        "workspace_busy"
    );
    let inspect = admin::execute(
        &s,
        &json!({"action":"execution-hold.inspect","response_id":"resp_5"}),
    )
    .unwrap();
    admin::execute(&s,&json!({"action":"execution-hold.release","response_id":"resp_5","operation_id":"release-legacy","review_token":inspect["review_token"],"expected_revision":inspect["hold_revision"],"reason":"legacy child confirmed absent in isolated test","accept_risk":true})).unwrap();
    assert_eq!(
        s.store.get("legacy_hold", "resp_5").unwrap()["quarantined"],
        true
    );
    assert_eq!(
        migrated
            .control
            .by_request("legacy-run")
            .unwrap()
            .unwrap()
            .phase,
        "unknown"
    );
    assert!(
        s.accept(&cid, "run", &json!({"input":"test"}))
            .unwrap()
            .1
            .is_some()
    );
}

#[tokio::test]
async fn range_response_digest_matches_returned_bytes_and_416_reports_length() {
    use axum::http::{HeaderMap, StatusCode};
    use base64::Engine;
    use codex_hoshikage_proxy::v2::download;
    use http_body_util::BodyExt;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    let r = root();
    let s = Arc::new(Service::open(&r.join("state"), &r.join("work")).unwrap());
    let cid = conversation(&s);
    std::fs::write(s.workspace_path(&cid).unwrap().join("file"), b"0123456789").unwrap();
    let op = s.capture(&cid, "capture", &json!({"path":"file"})).unwrap();
    let aid = op["resource"]["id"].as_str().unwrap().to_owned();
    let mut headers = HeaderMap::new();
    headers.insert("range", "bytes=2-5".parse().unwrap());
    let response = download::content(s.clone(), "artifact", aid.clone(), headers)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 2-5/10");
    let digest = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(b"2345"))
    );
    assert_eq!(response.headers()["content-digest"], digest);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        b"2345"
    );
    let mut headers = HeaderMap::new();
    headers.insert("range", "bytes=100-200".parse().unwrap());
    let response = download::content(s, "artifact", aid, headers)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()["content-range"], "bytes */10");
}

#[tokio::test]
async fn legacy_writer_cannot_invalidate_v2_read_modify_write_snapshot() {
    use codex_hoshikage_proxy::{control::Execution, store::ResponseStore};
    use std::sync::Arc;
    let r = root();
    let s = Arc::new(Service::open(&r.join("state/v2"), &root()).unwrap());
    let legacy = Arc::new(ResponseStore::open(&r).await.unwrap());
    let cid = conversation(&s);
    let (send, receive) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        receive.recv().unwrap();
        legacy
            .control
            .reserve(Execution::new(
                "resp_77".into(),
                Some("legacy-writer".into()),
                None,
            ))
            .unwrap();
    });
    s.store
        .transaction(|tx| {
            let mut c = store::get(tx, "conversation", &cid)?;
            send.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            c["checked"] = json!(true);
            store::put(tx, "conversation", &cid, &c)?;
            Ok(())
        })
        .unwrap();
    writer.join().unwrap();
    assert_eq!(s.store.get("conversation", &cid).unwrap()["checked"], true);
}

#[tokio::test]
async fn disconnected_download_releases_capacity_and_range_resumes_same_blob() {
    use axum::http::{HeaderMap, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    let r = root();
    let s = Arc::new(Service::open(&r.join("state"), &r.join("work")).unwrap());
    let cid = conversation(&s);
    let original: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(
        s.workspace_path(&cid).unwrap().join("download.bin"),
        &original,
    )
    .unwrap();
    let op = s
        .capture(&cid, "download-capture", &json!({"path":"download.bin"}))
        .unwrap();
    let aid = op["resource"]["id"].as_str().unwrap().to_owned();
    let mut held = Vec::new();
    for _ in 0..s.limits.download_concurrency {
        held.push(
            codex_hoshikage_proxy::v2::download::content(
                s.clone(),
                "artifact",
                aid.clone(),
                HeaderMap::new(),
            )
            .await
            .unwrap(),
        );
    }
    let error = codex_hoshikage_proxy::v2::download::content(
        s.clone(),
        "artifact",
        aid.clone(),
        HeaderMap::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "download_capacity_busy");
    let mut body = held.pop().unwrap().into_body();
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let offset = frame.len();
    assert!(offset > 0 && offset < original.len());
    let mut received = frame.to_vec();
    drop(body); // Emulate client disconnect before the next chunk is requested.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while s.downloads.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("download worker releases capacity after disconnect");
    let mut headers = HeaderMap::new();
    headers.insert("range", format!("bytes={offset}-").parse().unwrap());
    // Mutable workspace content is never consulted by a resumed artifact read.
    std::fs::write(
        s.workspace_path(&cid).unwrap().join("download.bin"),
        b"replacement",
    )
    .unwrap();
    let resumed = codex_hoshikage_proxy::v2::download::content(s.clone(), "artifact", aid, headers)
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::PARTIAL_CONTENT);
    received.extend_from_slice(&resumed.into_body().collect().await.unwrap().to_bytes());
    assert_eq!(received, original);
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while s.downloads.available_permits() != s.limits.download_concurrency {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all abandoned downloads release capacity");
    assert!(s.store.list("read_pin").unwrap().is_empty());
    drop(s);
    std::fs::remove_dir_all(r).unwrap();
}
