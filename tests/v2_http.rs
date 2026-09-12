use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use codex_hoshikage_proxy::{
    catalog::ModelCatalogManager,
    config::{RawConfig, RawModelConfig, ValidatedConfig},
    http::{AppState, router},
    journal::EventJournal,
    runtime::CodexRuntime,
    store::ResponseStore,
    v2::service::Service,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
async fn app() -> (Router, Arc<Service>, Arc<CodexRuntime>) {
    app_args(&[]).await
}
async fn app_args(args: &[&str]) -> (Router, Arc<Service>, Arc<CodexRuntime>) {
    let root = std::env::temp_dir().join(format!("v2-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("test-key".into());
    raw.server.default_cwd = Some(root.to_str().unwrap().into());
    raw.security.allowed_cwds = vec![root.to_str().unwrap().into()];
    raw.providers.get_mut("chatgpt").unwrap().enabled = false;
    raw.providers.get_mut("hoshikage").unwrap().base_url = None;
    raw.models
        .insert("hoshikage/test".into(), RawModelConfig::default());
    let mut config = ValidatedConfig::from_raw(raw).unwrap();
    config.codex_command = env!("CARGO_BIN_EXE_fake_codex").into();
    config.codex_args = args.iter().map(|v| v.to_string()).collect();
    config.codex_home = root.join("codex");
    let runtime = CodexRuntime::launch(&config).await.unwrap();
    let store = Arc::new(ResponseStore::open(&root).await.unwrap());
    let journal = Arc::new(EventJournal::open(&root).await.unwrap());
    let mut state = AppState::new(
        runtime.clone(),
        ModelCatalogManager::new(config.models.clone(), runtime.clone()).unwrap(),
        config.cwd_policy,
        config.default_cwd,
        Some("test-key".into()),
        if args.contains(&"--short-idle-timeout") {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(10)
        },
        Duration::from_secs(5),
        3,
        Duration::from_secs(1),
        "read-only".into(),
        Duration::from_secs(5),
        false,
        journal,
        store,
    );
    let service = Arc::new(Service::open(&root.join("v2"), &root.join("work")).unwrap());
    state.v2 = Some(service.clone());
    codex_hoshikage_proxy::v2::events::start_maintenance(state.clone(), service.clone());
    (router(state), service, runtime)
}
async fn call(
    app: &Router,
    s: &Service,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/v2/codex/{path}"))
        .header("authorization", "Bearer test-key")
        .header("x-proxy-instance-id", &s.store.instance)
        .header("x-proxy-recovery-generation", &s.store.generation)
        .header("content-type", "application/json");
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
#[tokio::test]
async fn managed_execution_can_be_recovered_without_http_stream() {
    let (app, s, runtime) = app().await;
    let (status, c) = call(
        &app,
        &s,
        "POST",
        "conversations",
        Some("c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{c}");
    let cid = c["resource"]["id"].as_str().unwrap();
    let (status, r) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("r"),
        json!({"input":"hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{r}");
    let rid = r["resource"]["id"].as_str().unwrap();
    let mut output = Value::Null;
    for _ in 0..100 {
        let (_, v) = call(
            &app,
            &s,
            "GET",
            &format!("responses/{rid}"),
            None,
            json!({}),
        )
        .await;
        if v["output"]["state"] == "ready" {
            output = v;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        output["execution_status"],
        "completed",
        "{:?}",
        s.store.get("response", rid)
    );
    let (status, o) = call(
        &app,
        &s,
        "GET",
        &format!("responses/{rid}/output"),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(o["output"][0]["content"][0]["text"], "fake response");
    runtime.shutdown().await.unwrap();
}
#[tokio::test]
async fn stop_before_acceptance_and_generation_precondition() {
    let (app, s, runtime) = app().await;
    let (_, c) = call(
        &app,
        &s,
        "POST",
        "conversations",
        Some("c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    let cid = c["resource"]["id"].as_str().unwrap();
    let (status, st) = call(
        &app,
        &s,
        "POST",
        "stops",
        Some("s"),
        json!({"target":{"conversation_id":cid,"request_key":"late"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(st["stop_status"], "cancelled_before_start");
    let (_, r) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("late"),
        json!({"input":"must not run"}),
    )
    .await;
    assert_eq!(r["state"], "failed");
    let res = app
        .oneshot(
            Request::builder()
                .uri("/v2/codex/conversations/anything")
                .header("authorization", "Bearer test-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PRECONDITION_REQUIRED);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_tool_registers_immutable_artifact() {
    let (app, s, runtime) = app_args(&["--artifact-tool"]).await;
    let (_, c) = call(
        &app,
        &s,
        "POST",
        "conversations",
        Some("c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    let cid = c["resource"]["id"].as_str().unwrap();
    let (_, r) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("r"),
        json!({"input":"publish"}),
    )
    .await;
    let rid = r["resource"]["id"].as_str().unwrap();
    for _ in 0..100 {
        if s.store.get("response", rid).unwrap()["output"]["state"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (_, a) = call(
        &app,
        &s,
        "GET",
        &format!("conversations/{cid}/artifacts"),
        None,
        json!({}),
    )
    .await;
    assert_eq!(a["data"].as_array().unwrap().len(), 1);
    assert_eq!(a["data"][0]["state"], "ready");
    let (_, o) = call(
        &app,
        &s,
        "GET",
        &format!("responses/{rid}/output"),
        None,
        json!({}),
    )
    .await;
    assert_eq!(o["output"][0]["content"][0]["text"], "artifact published");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_stop_interrupts_silent_managed_turn_and_is_idempotent() {
    let (app, s, runtime) = app_args(&["--silent-turn"]).await;
    let (_, c) = call(
        &app,
        &s,
        "POST",
        "conversations",
        Some("c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    let cid = c["resource"]["id"].as_str().unwrap();
    let (_, op) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("r"),
        json!({"input":"wait"}),
    )
    .await;
    let rid = op["resource"]["id"].as_str().unwrap();
    for _ in 0..100 {
        if s.store.get("response", rid).unwrap()["phase"] == "started" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(s.store.get("response", rid).unwrap()["phase"], "started");
    let (_, stop) = call(
        &app,
        &s,
        "POST",
        "stops",
        Some("stop"),
        json!({"target":{"response_id":rid}}),
    )
    .await;
    let (_, repeat) = call(
        &app,
        &s,
        "POST",
        "stops",
        Some("stop"),
        json!({"target":{"response_id":rid}}),
    )
    .await;
    assert_eq!(stop["stop_id"], repeat["stop_id"]);
    for _ in 0..100 {
        if s.store.get("response", rid).unwrap()["phase"] == "finished" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let r = s.store.get("response", rid).unwrap();
    assert_eq!(r["execution_status"], "interrupted");
    assert_eq!(r["hold_state"], "released");
    assert_eq!(r["output"]["state"], "unavailable");
    let (_, final_stop) = call(
        &app,
        &s,
        "GET",
        &format!("stops/{}", stop["stop_id"].as_str().unwrap()),
        None,
        json!({}),
    )
    .await;
    assert_eq!(final_stop["stop_status"], "interrupted");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn replay_does_not_require_retired_model_or_free_copy_slot() {
    let (app, s, runtime) = app().await;
    let request = json!({"workspace":{"mode":"automatic"},"model":"hoshikage/retired-model"});
    let original = s.conversation("retired", &request).unwrap();
    let (status, repeated) =
        call(&app, &s, "POST", "conversations", Some("retired"), request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(original["operation_id"], repeated["operation_id"]);
    let cid = original["resource"]["id"].as_str().unwrap();
    std::fs::write(s.workspace_path(cid).unwrap().join("file"), "saved").unwrap();
    let capture = s.capture(cid, "capture", &json!({"path":"file"})).unwrap();
    let _occupied = s
        .copies
        .acquire_many(s.limits.capture_concurrency as u32)
        .await
        .unwrap();
    let (status, repeated) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/artifacts"),
        Some("capture"),
        json!({"path":"file"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(capture["operation_id"], repeated["operation_id"]);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn durable_acceptance_is_dispatched_even_if_http_worker_was_lost() {
    let (_app, s, runtime) = app().await;
    let op = s
        .conversation(
            "c",
            &json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
        )
        .unwrap();
    let cid = op["resource"]["id"].as_str().unwrap();
    let (_, rid) = s
        .accept(
            cid,
            "accepted-before-disconnect",
            &json!({"input":"recover dispatch"}),
        )
        .unwrap();
    let rid = rid.unwrap();
    // No engine::run call: this represents a lost HTTP future after the durable commit.
    for _ in 0..400 {
        if s.store.get("response", &rid).unwrap()["output"]["state"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        s.store.get("response", &rid).unwrap()["output"]["state"],
        "ready"
    );
    assert!(
        s.accept(
            cid,
            "accepted-before-disconnect",
            &json!({"input":"recover dispatch"})
        )
        .unwrap()
        .1
        .is_none()
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn generated_images_are_discovered_without_sse_and_download_as_artifacts() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let (app, s, runtime) = app_args(&["--generated-image"]).await;
    let (_, cap) = call(&app, &s, "GET", "capabilities", None, json!({})).await;
    assert_eq!(cap["features"]["response_generated_images"], true);
    let (_, c) = call(
        &app,
        &s,
        "POST",
        "conversations",
        Some("images-c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    let cid = c["resource"]["id"].as_str().unwrap();
    let (_, op) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("images-r"),
        json!({"input":"draw"}),
    )
    .await;
    let rid = op["resource"]["id"].as_str().unwrap();
    let path = format!("responses/{rid}/generated-images");
    let mut manifest = Value::Null;
    for _ in 0..200 {
        let (status, m) = call(&app, &s, "GET", &path, None, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{m}");
        if m["state"] == "complete" {
            manifest = m;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(manifest["items"][0]["state"], "ready", "{manifest}");
    assert!(manifest["expires_at"].is_string());
    assert!(manifest.get("policy").is_none());
    let aid = manifest["items"][0]["artifact_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v2/codex/artifacts/{aid}/content"))
                .header("authorization", "Bearer test-key")
                .header("x-proxy-instance-id", &s.store.instance)
                .header("x-proxy-recovery-generation", &s.store.generation)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    let data = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(data.as_ref(),STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jGZkAAAAASUVORK5CYII=").unwrap());
    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v2/codex/{path}"))
                .header("authorization", "Bearer test-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::PRECONDITION_REQUIRED);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_interaction_stops_only_the_accepted_execution() {
    let (app, s, runtime) = app_args(&["--server-request=item/permissions/requestApproval"]).await;
    let (_, c) = call(
        &app,
        &s,
        "POST",
        "conversations",
        Some("interaction-c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    let cid = c["resource"]["id"].as_str().unwrap();
    let (_, r) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("interaction-r"),
        json!({"input":"hello"}),
    )
    .await;
    let rid = r["resource"]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let record = s.store.get("response", rid).unwrap();
            if record["phase"] == "finished" {
                assert_eq!(record["error"]["code"], "unsupported_interaction");
                assert_eq!(record["execution_status"], "interrupted");
                assert_eq!(record["hold_state"], "released");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (_, replay) = call(
        &app,
        &s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("interaction-r"),
        json!({"input":"hello"}),
    )
    .await;
    assert_eq!(replay["resource"]["id"], rid);
    runtime.shutdown().await.unwrap();
}

async fn interactive_request(app: &Router, s: &Service, kind: &str) -> (String, String) {
    let (_, c) = call(
        app,
        s,
        "POST",
        "conversations",
        Some("relay-c"),
        json!({"workspace":{"mode":"automatic"},"model":"hoshikage/test"}),
    )
    .await;
    let cid = c["resource"]["id"].as_str().unwrap().to_owned();
    let (_, r) = call(
        app,
        s,
        "POST",
        &format!("conversations/{cid}/responses"),
        Some("relay-r"),
        json!({"input":"hello","interaction_capabilities":[kind]}),
    )
    .await;
    let rid = r["resource"]["id"].as_str().unwrap().to_owned();
    let iid = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, items) = call(
                app,
                s,
                "GET",
                &format!("responses/{rid}/interactions"),
                None,
                json!({}),
            )
            .await;
            if let Some(i) = items["data"].as_array().and_then(|a| a.first())
                && s.store.get("response", &rid).unwrap()["phase"] == "started"
            {
                assert!(i.get("rpc_id").is_none());
                break i["interaction_id"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    (rid, iid)
}

#[tokio::test]
async fn opted_in_interactions_validate_reply_and_never_resend() {
    for (kind, method, answer, bad) in [
        (
            "user_input",
            "item/tool/requestUserInput",
            json!({"answers":{"color":{"answers":["Blue"]}}}),
            json!({"answers":{"color":{"answers":["invented"]}}}),
        ),
        (
            "permissions",
            "item/permissions/requestApproval",
            json!({"permissions":{"network":{"enabled":true}},"scope":"turn"}),
            json!({"permissions":{"fileSystem":{"write":["/etc"]}}}),
        ),
        (
            "mcp_form",
            "mcpServer/elicitation/request",
            json!({"action":"accept","content":{"ok":true}}),
            json!({"action":"accept","content":{"ok":"not a bool"}}),
        ),
    ] {
        let arg = format!("--server-request={method}");
        let (app, s, runtime) = app_args(&[&arg]).await;
        let mut events = runtime.subscribe();
        let (rid, iid) = interactive_request(&app, &s, kind).await;
        let path = format!("interactions/{iid}/reply");
        let (status, _) = call(
            &app,
            &s,
            "POST",
            &path,
            Some("bad-reply"),
            json!({"expected_revision":1,"response":bad}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let body = json!({"expected_revision":1,"response":answer});
        let (status, op) = call(&app, &s, "POST", &path, Some("reply-1"), body.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{op}");
        assert_eq!(op["state"], "succeeded");
        let (_, replayed) = call(&app, &s, "POST", &path, Some("reply-1"), body).await;
        assert_eq!(replayed["operation_id"], op["operation_id"]);
        let (conflict, _) = call(
            &app,
            &s,
            "POST",
            &path,
            Some("reply-1"),
            json!({"expected_revision":1,"response":{}}),
        )
        .await;
        assert_eq!(conflict, StatusCode::CONFLICT);
        tokio::time::timeout(Duration::from_secs(3), async {
            while s.store.get("response", &rid).unwrap()["phase"] != "finished" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let (_, i) = call(
            &app,
            &s,
            "GET",
            &format!("interactions/{iid}"),
            None,
            json!({}),
        )
        .await;
        assert_eq!(i["state"], "resolved");
        assert!(i["request"].is_null());
        let mut replies = 0;
        while let Ok(e) = events.try_recv() {
            if e["method"] == "test/serverReply" {
                replies += 1;
                assert!(e["params"].get("result").is_some());
            }
        }
        assert_eq!(replies, 1);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn stop_and_expiry_fence_interaction_answers() {
    for expired in [false, true] {
        let (app, s, runtime) = app_args(&["--server-request=item/tool/requestUserInput"]).await;
        let (rid, iid) = interactive_request(&app, &s, "user_input").await;
        if expired {
            s.store
                .update("interaction", &iid, |i| {
                    i["expires_at_ms"] = json!(0);
                    Ok(())
                })
                .unwrap();
        } else {
            call(
                &app,
                &s,
                "POST",
                "stops",
                Some("stop-first"),
                json!({"target":{"response_id":rid}}),
            )
            .await;
        }
        let (status, _) = call(
            &app,
            &s,
            "POST",
            &format!("interactions/{iid}/reply"),
            Some("too-late"),
            json!({"expected_revision":1,"response":{"answers":{"color":{"answers":["Blue"]}}}}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(s.store.get("response", &rid).unwrap()["stop_requested"] == true);
        assert!(s.store.get("interaction", &iid).unwrap()["request"].is_null());
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn pending_interaction_uses_its_deadline_instead_of_model_idle_timeout() {
    let (app, s, runtime) = app_args(&[
        "--server-request=item/tool/requestUserInput",
        "--short-idle-timeout",
    ])
    .await;
    let (rid, iid) = interactive_request(&app, &s, "user_input").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(s.store.get("response", &rid).unwrap()["phase"], "started");
    let (status, op) = call(
        &app,
        &s,
        "POST",
        &format!("interactions/{iid}/reply"),
        Some("waited-reply"),
        json!({"expected_revision":1,"response":{"answers":{"color":{"answers":["Blue"]}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{op}");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_answers_have_one_winner_and_generation_is_checked() {
    let (app, s, runtime) = app_args(&["--server-request=item/tool/requestUserInput"]).await;
    let (_, iid) = interactive_request(&app, &s, "user_input").await;
    let path = format!("interactions/{iid}/reply");
    let body = json!({"expected_revision":1,"response":{"answers":{"color":{"answers":["Blue"]}}}});
    let mut events = runtime.subscribe();
    let (a, b) = tokio::join!(
        call(&app, &s, "POST", &path, Some("race-a"), body.clone()),
        call(&app, &s, "POST", &path, Some("race-b"), body.clone())
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::ACCEPTED)
            .count(),
        1
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v2/codex/{path}"))
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .header("x-proxy-instance-id", &s.store.instance)
        .header("x-proxy-recovery-generation", "old-generation")
        .header("idempotency-key", "old-answer")
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut count = 0;
    while let Ok(e) = events.try_recv() {
        if e["method"] == "test/serverReply" {
            count += 1;
        }
    }
    assert_eq!(count, 1);
    runtime.shutdown().await.unwrap();
}
