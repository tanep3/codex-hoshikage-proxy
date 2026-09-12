use codex_hoshikage_proxy::v2::{interactions as relay, service::Service, store};
use serde_json::{Value, json};
fn setup() -> (std::path::PathBuf, Service, String) {
    let root = std::env::temp_dir().join(format!("relay-test-{}", uuid::Uuid::new_v4()));
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    let c = s
        .conversation(
            "c",
            &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
        )
        .unwrap();
    let cid = c["resource"]["id"].as_str().unwrap();
    let (_, rid) = s
        .accept(
            cid,
            "r",
            &json!({"input":"hello","interaction_capabilities":relay::KINDS}),
        )
        .unwrap();
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
fn question() -> Value {
    json!({"threadId":"thread","turnId":"turn","questions":[{"id":"q","header":"H","question":"Choose","options":[{"label":"yes"},{"label":"no"}],"isOther":false}]})
}
fn receive(s: &Service, rid: &str, method: &str, p: &Value) -> Value {
    assert!(relay::receive(s, &json!("unsupported_fake"), method, p).unwrap());
    relay::list(s, rid).unwrap()["data"][0].clone()
}
#[test]
fn schema_and_permission_validation_never_broadens_authority() {
    let (root, s, rid) = setup();
    let p = json!({"threadId":"thread","turnId":"turn","permissions":{"network":{"enabled":true},"fileSystem":{"entries":[{"access":"write","path":{"type":"special","value":{"kind":"project_roots"}}},{"access":"deny","path":{"type":"glob_pattern","pattern":"**/.env"}}]}}});
    let i = receive(&s, &rid, "item/permissions/requestApproval", &p);
    assert!(relay::validate_reply(&i, &json!({"permissions":{}})).is_ok());
    assert!(
        relay::validate_reply(
            &i,
            &json!({"permissions":{"fileSystem":p["permissions"]["fileSystem"]},"scope":"session"})
        )
        .is_ok()
    );
    for answer in [
        json!({"permissions":{"network":{"enabled":false}}}),
        json!({"permissions":{"fileSystem":{"entries":[p["permissions"]["fileSystem"]["entries"][0]]}}}),
        json!({"permissions":{},"scope":"forever"}),
        json!({"permissions":{},"strictAutoReview":false}),
    ] {
        assert!(relay::validate_reply(&i, &answer).is_err(), "{answer}");
    }
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn form_schema_is_bounded_and_remote_references_are_rejected() {
    let (root, s, rid) = setup();
    let mut p = json!({"threadId":"thread","turnId":"turn","mode":"form","serverName":"test","message":"form","requestedSchema":{"type":"object","properties":{"name":{"type":"string","minLength":2,"maxLength":4},"count":{"type":"integer","minimum":1,"maximum":3},"kind":{"type":"string","enum":["a","b"]}},"required":["name","count"]}});
    let i = receive(&s, &rid, "mcpServer/elicitation/request", &p);
    for content in [
        json!({"name":"日本","count":2}),
        json!({"name":"test","count":3,"kind":"b"}),
    ] {
        assert!(relay::validate_reply(&i, &json!({"action":"accept","content":content})).is_ok());
    }
    for content in [
        json!({"name":"x","count":2}),
        json!({"name":"name","count":4}),
        json!({"name":"name","count":1.5}),
        json!({"name":"name","count":2,"extra":true}),
        json!({"name":"name","count":2,"kind":"z"}),
    ] {
        assert!(relay::validate_reply(&i, &json!({"action":"accept","content":content})).is_err());
    }
    p["requestedSchema"]["properties"]["remote"] = json!({"$ref":"https://example.com/schema"});
    assert_eq!(
        relay::receive(&s, &json!("other"), "mcpServer/elicitation/request", &p)
            .unwrap_err()
            .code,
        "unsupported_interaction_schema"
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn url_confirmation_never_accepts_content_or_non_http_urls() {
    let (root, s, rid) = setup();
    let mut p = json!({"threadId":"thread","mode":"url","serverName":"test","message":"Confirm","elicitationId":"url-1","url":"https://example.com/confirm"});
    let i = receive(&s, &rid, "mcpServer/elicitation/request", &p);
    for action in ["accept", "cancel", "decline"] {
        assert!(relay::validate_reply(&i, &json!({"action":action,"content":null})).is_ok());
    }
    assert!(
        relay::validate_reply(&i, &json!({"action":"accept","content":{"allow":true}})).is_err()
    );
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:password@example.com",
    ] {
        p["url"] = json!(url);
        assert!(relay::receive(&s, &json!("other"), "mcpServer/elicitation/request", &p).is_err());
    }
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn replay_target_limits_expiry_and_event_loss_are_fenced() {
    let (root, s, rid) = setup();
    let i = receive(&s, &rid, "item/tool/requestUserInput", &question());
    assert!(
        relay::receive(
            &s,
            &json!("unsupported_fake"),
            "item/tool/requestUserInput",
            &question()
        )
        .unwrap()
    );
    assert_eq!(
        relay::list(&s, &rid).unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut wrong = question();
    wrong["turnId"] = json!("other");
    assert_eq!(
        relay::receive(&s, &json!("wrong"), "item/tool/requestUserInput", &wrong)
            .unwrap_err()
            .code,
        "target_mismatch"
    );
    for n in 1..relay::MAX_COUNT {
        assert!(relay::receive(&s, &json!(n), "item/tool/requestUserInput", &question()).unwrap());
    }
    assert_eq!(
        relay::receive(&s, &json!(1000), "item/tool/requestUserInput", &question())
            .unwrap_err()
            .code,
        "interaction_limit_exceeded"
    );
    relay::event_loss(&s).unwrap();
    assert_eq!(
        s.store.get("response", &rid).unwrap()["stop_requested"],
        true
    );
    assert_eq!(
        relay::read(&s, i["interaction_id"].as_str().unwrap()).unwrap()["state"],
        "cancelled"
    );
    assert!(
        s.store
            .list("interaction")
            .unwrap()
            .iter()
            .all(|i| i["request"].is_null())
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn restart_never_resends_a_reply_with_unknown_delivery() {
    use codex_hoshikage_proxy::{
        config::{RawConfig, ValidatedConfig},
        runtime::CodexRuntime,
    };
    let (root, s, rid) = setup();
    let i = receive(&s, &rid, "item/tool/requestUserInput", &question());
    let iid = i["interaction_id"].as_str().unwrap().to_owned();
    let body = json!({"expected_revision":1,"response":{"answers":{"q":{"answers":["yes"]}}}});
    // Reproduce a committed intent whose transport result was lost.
    s.store
        .transaction(|tx| {
            let (mut op, _) = store::reserve(
                tx,
                "reply",
                "interaction.reply",
                &json!({"interaction_id":iid,"request":body}),
            )?;
            op["resource"] = json!({"type":"interaction","id":iid});
            store::save_operation(tx, &op)?;
            let mut record = store::get(tx, "interaction", &iid)?;
            record["state"] = json!("sending");
            record["reply_status"] = json!("unknown");
            record["reply_key"] = json!("reply");
            store::put(tx, "interaction", &iid, &record)
        })
        .unwrap();
    drop(s);
    let s = Service::open(&root.join("state"), &root.join("work")).unwrap();
    assert_eq!(relay::read(&s, &iid).unwrap()["state"], "unknown");
    assert!(s.store.get("interaction", &iid).unwrap()["request"].is_null());
    let mut raw = RawConfig::default();
    raw.server.v2_enabled = false;
    raw.security.allowed_cwds = vec![root.to_str().unwrap().into()];
    raw.server.default_cwd = Some(root.to_str().unwrap().into());
    let mut config = ValidatedConfig::from_raw(raw).unwrap();
    config.codex_command = env!("CARGO_BIN_EXE_fake_codex").into();
    config.codex_args = vec![];
    config.codex_home = root.join("fake");
    let runtime = CodexRuntime::launch(&config).await.unwrap();
    let mut events = runtime.subscribe();
    let op = relay::reply(&s, &runtime, &iid, "reply", &body)
        .await
        .unwrap();
    assert_eq!(op["state"], "unknown");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), events.recv())
            .await
            .is_err()
    );
    runtime.shutdown().await.unwrap();
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn early_turn_binding_and_expiry_keep_monotonic_revisions() {
    let (root, s, rid) = setup();
    for value in [
        json!(null),
        json!(["unknown"]),
        json!(["user_input", "user_input"]),
    ] {
        assert!(relay::validate_capabilities(Some(&value)).is_err());
    }
    s.store
        .update("response", &rid, |r| {
            r["phase"] = json!("dispatching");
            r["turn_id"] = Value::Null;
            Ok(())
        })
        .unwrap();
    let i = receive(&s, &rid, "item/tool/requestUserInput", &question());
    let iid = i["interaction_id"].as_str().unwrap();
    let before = s.store.get("response", &rid).unwrap()["interactions_revision"]
        .as_u64()
        .unwrap();
    let mut conflicting = question();
    conflicting["questions"][0]["question"] = json!("Changed question");
    assert_eq!(
        relay::receive(
            &s,
            &json!("unsupported_fake"),
            "item/tool/requestUserInput",
            &conflicting
        )
        .unwrap_err()
        .code,
        "interaction_request_conflict"
    );
    s.store
        .update("interaction", iid, |i| {
            i["expires_at_ms"] = json!(1);
            Ok(())
        })
        .unwrap();
    relay::refresh(&s).unwrap();
    let r = s.store.get("response", &rid).unwrap();
    assert!(r["interactions_revision"].as_u64().unwrap() > before);
    assert_eq!(r["interaction_wait_until_ms"], 0);
    assert_eq!(r["stop_requested"], true);
    assert_eq!(relay::read(&s, iid).unwrap()["state"], "expired");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
