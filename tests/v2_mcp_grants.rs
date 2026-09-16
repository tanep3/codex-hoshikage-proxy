use codex_hoshikage_proxy::v2::{
    interactions as relay, limits::Limits, mcp_grants as grants, service::Service, store,
};
use serde_json::{Value, json};
fn setup() -> (std::path::PathBuf, Service, String) {
    let root = std::env::temp_dir().join(format!("grants-{}", uuid::Uuid::new_v4()));
    let mut limits = Limits {
        mcp_turn_approval_enabled: true,
        ..Default::default()
    };
    limits.mcp_turn_grant_tools.insert(
        "test".into(),
        vec![
            "read_test".into(),
            "browser_evaluate".into(),
            "browser_run_code_unsafe".into(),
        ],
    );
    let s = Service::open_with_limits(&root.join("state"), &root.join("work"), limits).unwrap();
    let c = s
        .conversation(
            "c",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    let (_,rid)=s.accept(c["resource"]["id"].as_str().unwrap(),"r",&json!({"input":"test","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"u","channel_id":"ch","run_id":"run"}})).unwrap();
    let rid = rid.unwrap();
    s.store
        .update("response", &rid, |r| {
            r["thread_id"] = json!("thread");
            r["turn_id"] = json!("turn");
            r["phase"] = json!("started");
            Ok(())
        })
        .unwrap();
    (root, s, rid)
}
fn item(id: &str, tool: &str, args: Value) -> Value {
    json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"type":"mcpToolCall","id":id,"server":"test","tool":tool,"arguments":args}}})
}
fn native(id: &str) -> Value {
    json!({"threadId":"thread","turnId":"turn","itemId":id,"questions":[{"id":format!("mcp_tool_call_approval_{id}"),"question":"Confirmation","header":"H","isOther":false,"isSecret":false,"options":[{"label":"Allow"},{"label":"Cancel"}]}]})
}
fn receive(s: &Service, rid: &str, id: &str, tool: &str, args: Value) -> Value {
    grants::observe(s, &item(id, tool, args)).unwrap();
    assert!(relay::receive(s, &json!(id), "item/tool/requestUserInput", &native(id)).unwrap());
    let visible = relay::list(s, rid).unwrap()["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["operation"]["call_id"] == id)
        .unwrap()
        .clone();
    s.store
        .get("interaction", visible["interaction_id"].as_str().unwrap())
        .unwrap()
}
fn activate(s: &Service, rid: &str, mut i: Value) -> String {
    let r = s.store.get("response", rid).unwrap();
    let body = json!({"grant_scope":"turn_tool","expected_scope_fingerprint":i["operation"]["scope_fingerprint"],"response":{"action":"accept","content":{}}});
    s.store
        .transaction(|tx| {
            grants::create(s, tx, &r, &mut i, &body)?;
            store::put(tx, "interaction", i["interaction_id"].as_str().unwrap(), &i)?;
            grants::finish(tx, &i, true)
        })
        .unwrap();
    i["grant_id"].as_str().unwrap().into()
}
#[test]
fn explicit_grant_repeats_only_exact_scope_and_stop_revokes() {
    let (root, s, rid) = setup();
    let first = receive(&s, &rid, "1", "read_test", json!({"query":"one"}));
    let r = s.store.get("response", &rid).unwrap();
    let mut once = first.clone();
    s.store
        .transaction(|tx| {
            grants::create(
                &s,
                tx,
                &r,
                &mut once,
                &json!({"response":{"action":"accept","content":{}}}),
            )
        })
        .unwrap();
    assert!(once["grant_id"].is_null());
    let gid = activate(&s, &rid, first);
    for n in 2..=5 {
        let i = receive(&s, &rid, &n.to_string(), "read_test", json!({"query":n}));
        let iid = i["interaction_id"].as_str().unwrap();
        assert_eq!(grants::auto_grant(&s, iid).unwrap(), Some(gid.clone()));
    }
    let i = receive(&s, &rid, "6", "read_test", json!({}));
    let iid = i["interaction_id"].as_str().unwrap();
    for field in ["principal_id", "channel_id", "run_id"] {
        let before = s.store.get("response", &rid).unwrap()["approval_context"][field].clone();
        s.store
            .update("response", &rid, |r| {
                r["approval_context"][field] = json!("other");
                Ok(())
            })
            .unwrap();
        let mut call = s.store.get("interaction", iid).unwrap();
        let r = s.store.get("response", &rid).unwrap();
        assert!(
            s.store
                .transaction(|tx| grants::apply(&s, tx, &r, &mut call, &gid))
                .is_err()
        );
        s.store
            .update("response", &rid, |r| {
                r["approval_context"][field] = before;
                Ok(())
            })
            .unwrap();
    }
    s.store
        .update("response", &rid, |r| {
            r["stop_requested"] = json!(true);
            Ok(())
        })
        .unwrap();
    assert_eq!(grants::auto_grant(&s, iid).unwrap(), None);
    assert_eq!(
        grants::list(&s, &rid).unwrap()["data"][0]["state"],
        "expired"
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn provenance_risk_redaction_and_changed_call_fail_closed() {
    let (root, s, rid) = setup();
    assert!(
        !relay::receive(
            &s,
            &json!("missing"),
            "item/tool/requestUserInput",
            &native("missing")
        )
        .unwrap()
    );
    for (id, tool, args, reason) in [
        (
            "1",
            "browser_evaluate",
            json!({"function":"() => 1"}),
            "high_risk_tool",
        ),
        ("2", "browser_run_code_unsafe", json!({}), "high_risk_tool"),
        (
            "3",
            "read_test",
            json!({"token":"secret"}),
            "redacted_arguments",
        ),
    ] {
        let i = receive(&s, &rid, id, tool, args);
        assert_eq!(i["operation"]["ineligible_reason"], reason);
        let d = grants::operation(&s, i["interaction_id"].as_str().unwrap()).unwrap();
        assert!(!d.to_string().contains("\"secret\""));
    }
    let i = receive(&s, &rid, "4", "read_test", json!({"query":"a"}));
    let iid = i["interaction_id"].as_str().unwrap();
    let p = json!({"threadId":"thread","turnId":"turn","serverName":"test","mode":"form","message":"spoof","requestedSchema":{"type":"object","properties":{}},"native_call_id":"4","native_question_id":"mcp_tool_call_approval_4","_meta":{"codex_approval_kind":"mcp_tool_call"}});
    assert!(relay::receive(&s, &json!("spoof"), "mcpServer/elicitation/request", &p).unwrap());
    assert!(
        relay::list(&s, &rid).unwrap()["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["request"]["message"] == "spoof")
            .unwrap()["operation"]
            .is_null()
    );
    grants::observe(&s, &item("4", "read_test", json!({"query":"b"}))).unwrap();
    assert!(grants::check_display(&s, &i, &json!({})).is_err());
    assert_eq!(
        grants::operation(&s, iid).unwrap()["turn_grant_eligible"],
        false
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn steer_revoke_unknown_and_restart_do_not_resurrect_grants() {
    let (root, s, rid) = setup();
    let i = receive(&s, &rid, "1", "read_test", json!({}));
    let gid = activate(&s, &rid, i.clone());
    let op = grants::revoke(&s, &gid, "cancel").unwrap();
    assert_eq!(op, grants::revoke(&s, &gid, "cancel").unwrap());
    assert_eq!(
        grants::list(&s, &rid).unwrap()["data"][0]["state"],
        "revoked"
    );
    let i = receive(&s, &rid, "2", "read_test", json!({}));
    let gid = activate(&s, &rid, i.clone());
    s.store
        .transaction(|tx| grants::finish(tx, &json!({"grant_id":gid}), false))
        .unwrap();
    assert_eq!(
        grants::auto_grant(&s, i["interaction_id"].as_str().unwrap()).unwrap(),
        None
    );
    let i = receive(&s, &rid, "3", "read_test", json!({}));
    activate(&s, &rid, i);
    grants::steer(&s, "thread", "turn").unwrap();
    assert!(
        grants::list(&s, &rid).unwrap()["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["state"] != "active")
    );
    drop(s);
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    assert!(
        grants::list(&s, &rid).unwrap()["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["state"] != "active")
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn display_snapshot_generation_limits_and_wire_bounds() {
    let (root, s, rid) = setup();
    let config = root.join("mcp.toml");
    std::fs::write(&config, "[mcp_servers.test]\ncommand='old'\n").unwrap();
    s.mcp.lock().unwrap().config_paths = vec![config.clone()];
    let i = receive(&s, &rid, "a", "read_test", json!({"query":"private-query"}));
    let iid = i["interaction_id"].as_str().unwrap();
    assert!(!i.to_string().contains("private-query"));
    let d = grants::operation(&s, iid).unwrap();
    assert_eq!(d["arguments"], json!({"query":"private-query"}));
    let body = json!({"expected_scope_fingerprint":d["scope_fingerprint"]});
    grants::check_display(&s, &i, &body).unwrap();
    assert!(grants::check_display(&s, &i, &json!({"expected_scope_fingerprint":null})).is_err());
    let gid = activate(&s, &rid, i.clone());
    std::fs::write(&config, "[mcp_servers.test]\ncommand='new'\n").unwrap();
    assert_eq!(
        grants::operation(&s, iid).unwrap()["ineligible_reason"],
        "config_changed"
    );
    assert!(grants::check_display(&s, &i, &body).is_err());
    assert_eq!(grants::auto_grant(&s, iid).unwrap(), None);
    std::fs::write(&config, "[mcp_servers.test]\ncommand='old'\n").unwrap();
    assert!(grants::check_display(&s, &i, &body).is_err());
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "expired");
    let next = receive(&s, &rid, "b", "read_test", json!({}));
    grants::steer(&s, "thread", "turn").unwrap();
    let r = s.store.get("response", &rid).unwrap();
    assert!(grants::check_scope(&s, &r, &next).is_err());
    assert_eq!(
        grants::operation(&s, next["interaction_id"].as_str().unwrap()).unwrap()["ineligible_reason"],
        "input_changed"
    );
    let too_large = item("large", "read_test", json!({"x":"x".repeat(65536)}));
    grants::observe(&s, &too_large).unwrap();
    assert!(
        grants::adapt(&s, "item/tool/requestUserInput", &native("large"))
            .unwrap()
            .is_none()
    );
    assert!(grants::bounded_response(json!({"a":1}), 2).is_err());
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pending_first_reply_waits_and_revoke_wins_before_apply() {
    let (root, s, rid) = setup();
    let s = std::sync::Arc::new(s);
    let mut first = receive(&s, &rid, "first", "read_test", json!({}));
    let r = s.store.get("response", &rid).unwrap();
    s.store.transaction(|tx|{
        let body=json!({"grant_scope":"turn_tool","expected_scope_fingerprint":first["operation"]["scope_fingerprint"],"response":{"action":"accept","content":{}}});
        grants::create(&s,tx,&r,&mut first,&body)
    }).unwrap();
    let gid = first["grant_id"].as_str().unwrap().to_string();
    let next = receive(&s, &rid, "next", "read_test", json!({}));
    let iid = next["interaction_id"].as_str().unwrap().to_owned();
    let svc = s.clone();
    let waiter = tokio::spawn(async move { grants::await_auto_grant(&svc, &iid).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!waiter.is_finished());
    s.store
        .transaction(|tx| grants::finish(tx, &first, true))
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap(),
        Some(gid.clone())
    );
    grants::revoke(&s, &gid, "cancel-before-apply").unwrap();
    let mut next = next;
    assert!(
        s.store
            .transaction(|tx| grants::apply(&s, tx, &r, &mut next, &gid))
            .is_err()
    );
    // A late write acknowledgment never revives a revoked grant.
    s.store
        .transaction(|tx| grants::finish(tx, &first, true))
        .unwrap();
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "revoked");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ttl_terminal_thread_binding_and_scope_dimensions() {
    let (root, s, rid) = setup();
    let first = receive(&s, &rid, "first", "read_test", json!({}));
    let gid = activate(&s, &rid, first);
    let next = receive(&s, &rid, "next", "read_test", json!({}));
    let r = s.store.get("response", &rid).unwrap();
    for field in [
        "instance_id",
        "recovery_generation",
        "response_id",
        "conversation_id",
        "workspace_id",
        "turn_id",
        "server",
        "tool",
        "config_generation",
        "input_generation",
    ] {
        let mut altered = next.clone();
        altered["operation"]["scope"][field] = json!("different");
        assert!(
            s.store
                .transaction(|tx| grants::apply(&s, tx, &r, &mut altered, &gid))
                .is_err(),
            "{field}"
        );
    }
    grants::observe(
        &s,
        &json!({"method":"turn/completed","params":{"threadId":"other-thread","turnId":"turn"}}),
    )
    .unwrap();
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "active");
    s.store
        .update("mcp_grant", &gid, |g| {
            g["expires_at_ms"] = json!(0);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        grants::auto_grant(&s, next["interaction_id"].as_str().unwrap()).unwrap(),
        None
    );
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "expired");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pending_and_history_limits_are_distinct_and_no_truncation() {
    let (root, s, rid) = setup();
    for n in 0..16 {
        receive(&s, &rid, &n.to_string(), "read_test", json!({}));
    }
    grants::observe(&s, &item("overflow", "read_test", json!({}))).unwrap();
    assert_eq!(
        relay::receive(
            &s,
            &json!("overflow"),
            "item/tool/requestUserInput",
            &native("overflow")
        )
        .unwrap_err()
        .code,
        "interaction_limit_exceeded"
    );
    s.store
        .transaction(|tx| {
            for mut i in store::list(tx, "interaction")? {
                i["state"] = json!("resolved");
                store::put(tx, "interaction", i["interaction_id"].as_str().unwrap(), &i)?;
            }
            Ok(())
        })
        .unwrap();
    for n in 16..256 {
        // Avoid filling the bounded raw-call cache: native provenance is not needed
        // to exercise the independent durable interaction history limit.
        assert!(relay::receive(&s,&json!(n),"mcpServer/elicitation/request",&json!({"threadId":"thread","turnId":"turn","serverName":"test","mode":"form","message":"test","requestedSchema":{"type":"object","properties":{}}})).unwrap());
        s.store
            .transaction(|tx| {
                for mut i in store::list(tx, "interaction")? {
                    if i["state"] == "pending" {
                        i["state"] = json!("resolved");
                        store::put(tx, "interaction", i["interaction_id"].as_str().unwrap(), &i)?;
                    }
                }
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(
        relay::list(&s, &rid).unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        256
    );
    assert!(relay::receive(&s,&json!("257"),"mcpServer/elicitation/request",&json!({"threadId":"thread","turnId":"turn","serverName":"test","mode":"form","message":"test","requestedSchema":{"type":"object","properties":{}}})).is_err());
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn full_cache_and_oversized_duplicate_cannot_preserve_stale_approval() {
    let (root, s, rid) = setup();
    let i = receive(&s, &rid, "original", "read_test", json!({"x":"original"}));
    for n in 1..256 {
        grants::observe(&s, &item(&format!("fill-{n}"), "read_test", json!({}))).unwrap();
    }
    grants::observe(&s, &item("original", "read_test", json!({"x":"changed"}))).unwrap();
    assert!(grants::check_display(&s, &i, &json!({})).is_err());
    let i2 = receive(&s, &rid, "new", "read_test", json!({}));
    grants::observe(
        &s,
        &item("new", "read_test", json!({"x":"x".repeat(65536)})),
    )
    .unwrap();
    assert!(grants::check_display(&s, &i2, &json!({})).is_err());
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn steer_persistence_failure_rolls_back_generation() {
    let (root, s, rid) = setup();
    let i = receive(&s, &rid, "first", "read_test", json!({}));
    let gid = activate(&s, &rid, i);
    s.store.transaction(|tx|{
        tx.execute_batch("CREATE TRIGGER fail_steer BEFORE UPDATE ON records WHEN NEW.kind='response' BEGIN SELECT RAISE(ABORT, 'test injected write failure'); END;")?;
        Ok(())
    }).unwrap();
    assert!(grants::steer(&s, "thread", "turn").is_err());
    assert_eq!(
        s.store.get("response", &rid).unwrap()["input_generation"],
        0
    );
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "active");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn grants_are_limited_to_sixteen_including_expired_records() {
    let (root, s, rid) = setup();
    for n in 0..16 {
        let i = receive(&s, &rid, &n.to_string(), "read_test", json!({}));
        let iid = i["interaction_id"].as_str().unwrap().to_owned();
        let gid = activate(&s, &rid, i);
        grants::revoke(&s, &gid, &format!("revoke-{n}")).unwrap();
        s.store
            .update("interaction", &iid, |i| {
                i["state"] = json!("resolved");
                Ok(())
            })
            .unwrap();
    }
    let mut i = receive(&s, &rid, "17", "read_test", json!({}));
    let r = s.store.get("response", &rid).unwrap();
    let b = json!({"grant_scope":"turn_tool","expected_scope_fingerprint":i["operation"]["scope_fingerprint"],"response":{"action":"accept","content":{}}});
    assert_eq!(
        s.store
            .transaction(|tx| grants::create(&s, tx, &r, &mut i, &b))
            .unwrap_err()
            .code,
        "turn_grant_limit"
    );
    assert_eq!(
        grants::list(&s, &rid).unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn active_grant_never_survives_formal_restore_or_event_gap() {
    use codex_hoshikage_proxy::v2::backup;
    let (root, s, rid) = setup();
    let i = receive(
        &s,
        &rid,
        "first",
        "read_test",
        json!({"private":"only memory"}),
    );
    let iid = i["interaction_id"].as_str().unwrap().to_owned();
    let gid = activate(&s, &rid, i);
    let generation = s.store.generation.clone();
    let limits = s.limits.clone();
    assert_eq!(
        backup::create(&s, &root.join("blocked-backup"))
            .unwrap_err()
            .code,
        "workspace_busy"
    );
    // Formal backup refuses an executing workspace. Finish it, deliberately
    // retaining the old grant record to test restore's independent fences.
    s.store
        .update("response", &rid, |r| {
            r["phase"] = json!("finished");
            r["hold_state"] = json!("released");
            Ok(())
        })
        .unwrap();
    backup::create(&s, &root.join("backup")).unwrap();
    relay::event_loss(&s).unwrap();
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "expired");
    assert!(grants::operation(&s, &iid).unwrap()["arguments"].is_null());
    drop(s);
    backup::restore(&root.join("state"), &root.join("backup")).unwrap();
    let s = Service::open_with_limits(&root.join("state"), &root.join("work"), limits).unwrap();
    assert_ne!(s.store.generation, generation);
    assert_eq!(
        s.store.metadata("recovery_state").unwrap(),
        "recovery_blocked"
    );
    grants::refresh(&s).unwrap();
    assert_eq!(s.store.get("mcp_grant", &gid).unwrap()["state"], "expired");
    assert_eq!(grants::auto_grant(&s, &iid).unwrap(), None);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn independent_parallel_responses_do_not_share_grants_or_stops() {
    let (root, s, rid) = setup();
    let first = receive(&s, &rid, "first", "read_test", json!({}));
    let first_grant = activate(&s, &rid, first);
    let c = s
        .conversation(
            "second-conversation",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    let (_,rid2)=s.accept(c["resource"]["id"].as_str().unwrap(),"second-run",&json!({"input":"second","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"u","channel_id":"ch","run_id":"run"}})).unwrap();
    let rid2 = rid2.unwrap();
    s.store
        .update("response", &rid2, |r| {
            r["thread_id"] = json!("thread2");
            r["turn_id"] = json!("turn2");
            r["phase"] = json!("started");
            Ok(())
        })
        .unwrap();
    let mut e = item("second", "read_test", json!({}));
    e["params"]["threadId"] = json!("thread2");
    e["params"]["turnId"] = json!("turn2");
    let mut p = native("second");
    p["threadId"] = json!("thread2");
    p["turnId"] = json!("turn2");
    grants::observe(&s, &e).unwrap();
    relay::receive(&s, &json!("second"), "item/tool/requestUserInput", &p).unwrap();
    let list = relay::list(&s, &rid2).unwrap();
    let iid = list["data"][0]["interaction_id"].as_str().unwrap();
    assert_eq!(grants::auto_grant(&s, iid).unwrap(), None);
    let second_grant = activate(&s, &rid2, s.store.get("interaction", iid).unwrap());
    s.store
        .update("response", &rid, |r| {
            r["stop_requested"] = json!(true);
            Ok(())
        })
        .unwrap();
    grants::refresh(&s).unwrap();
    assert_eq!(
        s.store.get("mcp_grant", &first_grant).unwrap()["state"],
        "expired"
    );
    assert_eq!(
        s.store.get("mcp_grant", &second_grant).unwrap()["state"],
        "active"
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_stop_or_revoke_and_application_have_a_single_order() {
    for stopping in [false, true] {
        for _ in 0..4 {
            let (root, s, rid) = setup();
            let first = receive(&s, &rid, "first", "read_test", json!({}));
            let gid = activate(&s, &rid, first);
            let next = receive(&s, &rid, "next", "read_test", json!({}));
            let iid = next["interaction_id"].as_str().unwrap().to_owned();
            let gate = std::sync::Barrier::new(2);
            let (applied, count) = std::thread::scope(|scope| {
                let applying = scope.spawn(|| {
                    gate.wait();
                    s.store
                        .transaction(|tx| {
                            let r = store::get(tx, "response", &rid)?;
                            let mut i = store::get(tx, "interaction", &iid)?;
                            grants::apply(&s, tx, &r, &mut i, &gid)?;
                            i["state"] = json!("sending");
                            store::put(tx, "interaction", &iid, &i)
                        })
                        .is_ok()
                });
                let cancelling=scope.spawn(|| {
                    gate.wait();
                    if stopping {
                        s.store.update("response",&rid,|r|{r["stop_requested"]=json!(true);Ok(())}).unwrap();
                        None
                    } else {Some(grants::revoke(&s,&gid,"concurrent-revoke").unwrap()["in_flight_or_unknown_count"].as_u64().unwrap())}
                });
                (applying.join().unwrap(), cancelling.join().unwrap())
            });
            assert_eq!(
                s.store.get("mcp_grant", &gid).unwrap()["application_count"],
                if applied { 2 } else { 1 }
            );
            if let Some(count) = count {
                assert_eq!(count, u64::from(applied));
            }
            grants::refresh(&s).unwrap();
            assert_eq!(grants::auto_grant(&s, &iid).unwrap(), None);
            drop(s);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn full_size_operation_cache_rejects_overflow_without_losing_existing_calls() {
    let (root, s, _) = setup();
    let start = std::time::Instant::now();
    let payload = "x".repeat(62_000);
    for n in 0..256 {
        grants::observe(
            &s,
            &item(&format!("large-{n}"), "read_test", json!({"data":payload})),
        )
        .unwrap();
    }
    for n in [0, 255] {
        assert!(
            grants::adapt(
                &s,
                "item/tool/requestUserInput",
                &native(&format!("large-{n}"))
            )
            .unwrap()
            .is_some()
        );
    }
    grants::observe(&s, &item("excess", "read_test", json!({}))).unwrap();
    assert!(
        grants::adapt(&s, "item/tool/requestUserInput", &native("excess"))
            .unwrap()
            .is_none()
    );
    println!(
        "bounded details: 256 x 62000-byte arguments admitted; 257th rejected; elapsed_ms={}",
        start.elapsed().as_millis()
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
