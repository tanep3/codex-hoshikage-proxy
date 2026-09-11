use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use codex_hoshikage_proxy::{
    config::{RawConfig, RawModelConfig, ValidatedConfig},
    http::{AppState, router},
    journal::EventJournal,
    runtime::CodexRuntime,
    store::ResponseStore,
};
use http_body_util::BodyExt;
use std::{
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

async fn test_app(args: &[&str]) -> axum::Router {
    test_app_with_runtime(args, Duration::from_secs(600))
        .await
        .0
}

async fn test_app_with_runtime(
    args: &[&str],
    idle_timeout: Duration,
) -> (axum::Router, Arc<CodexRuntime>) {
    let (app, runtime, _) = test_app_with_store(args, idle_timeout, None).await;
    (app, runtime)
}

async fn test_app_with_store(
    args: &[&str],
    idle_timeout: Duration,
    store: Option<Arc<ResponseStore>>,
) -> (axum::Router, Arc<CodexRuntime>, Arc<ResponseStore>) {
    let mut raw = RawConfig::default();
    // Exercise legacy compatibility independently of the v2 service.
    raw.server.v2_enabled = false;
    raw.providers.get_mut("chatgpt").unwrap().enabled = args.contains(&"--model-pages");
    raw.providers.get_mut("hoshikage").unwrap().base_url = None;
    raw.models.insert(
        "hoshikage/unsloth-gemma4-12b-qat-thinking-off".into(),
        RawModelConfig::default(),
    );
    for model in ["hoshikage/model-b", "hoshikage/model-rejected"] {
        raw.models.insert(
            model.into(),
            RawModelConfig {
                upstream_id: model.split_once('/').unwrap().1.into(),
                ..Default::default()
            },
        );
    }
    raw.security.allowed_cwds = vec![
        env::current_dir()
            .expect("current directory")
            .to_string_lossy()
            .into_owned(),
    ];
    let mut config = ValidatedConfig::from_raw(raw).expect("valid fake config");
    config.codex_command = env!("CARGO_BIN_EXE_fake_codex").into();
    config.codex_args = args.iter().map(|arg| (*arg).into()).collect();
    config.codex_home = PathBuf::from(format!(
        "/tmp/codex-hoshikage-proxy-http-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    let runtime = CodexRuntime::launch(&config)
        .await
        .expect("fake runtime launches");
    let journal = Arc::new(EventJournal::open(&config.codex_home).await.unwrap());
    let responses = match store {
        Some(store) => store,
        None => Arc::new(ResponseStore::open(&config.codex_home).await.unwrap()),
    };
    let catalog = codex_hoshikage_proxy::catalog::ModelCatalogManager::new(
        config.models.clone(),
        runtime.clone(),
    )
    .unwrap();
    let app = router(AppState::new(
        runtime.clone(),
        catalog,
        config.cwd_policy.clone(),
        config.default_cwd.clone(),
        None,
        idle_timeout,
        Duration::from_secs(180),
        3,
        Duration::from_secs(30),
        config.sandbox_mode.clone(),
        Duration::from_secs(5),
        !args.contains(&"--global-auto-approval-off"),
        journal,
        responses.clone(),
    ));
    (app, runtime, responses)
}

async fn response_text(response: Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes()
            .to_vec(),
    )
    .expect("utf8 response")
}

#[tokio::test]
async fn non_interactive_stream_reports_approval_required_and_ends() {
    let app = test_app(&["--approval"]).await;
    let response = app
        .oneshot(
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","messages":[{"role":"user","content":"run"}],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("approval_required"), "SSE body: {body}");
    assert!(body.contains("[DONE]"), "SSE body: {body}");
}

#[tokio::test]
async fn approval_api_rejects_second_http_decision() {
    let app = test_app(&["--approval", "--string-approval-id"]).await;
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","messages":[{"role":"user","content":"run"}],"stream":true,"metadata":{"codex.approval_capability":"interactive"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stream_response = response;

    let approval_id = wait_approval(&app, "turn_fake_1").await;
    let approval_path = format!("/v1/codex/approvals/{approval_id}");
    let first = app
        .clone()
        .oneshot(
            Request::post(&approval_path)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"decision":"accept"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let body = tokio::time::timeout(Duration::from_secs(2), response_text(stream_response))
        .await
        .expect("string-ID approval reaches Codex and completes the turn");
    assert!(body.contains("approved response"), "{body}");
    assert!(body.contains("[DONE]"), "{body}");

    let second = app
        .oneshot(
            Request::post(&approval_path)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"decision":"accept"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn mixed_workspace_file_changes_require_approval() {
    let app = test_app(&["--approval", "--file-approval"]).await;
    let response = tokio::time::timeout(Duration::from_secs(2), app.oneshot(
        Request::post("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","messages":[{"role":"user","content":"edit files"}]}"#)).unwrap()
    )).await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(response_text(response).await.contains("approval_required"));
}

#[tokio::test]
async fn file_change_started_notification_supplies_workspace_approval_paths() {
    let app = test_app(&["--approval", "--workspace-file-approval"]).await;
    let response = tokio::time::timeout(Duration::from_secs(2), app.oneshot(
        Request::post("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","messages":[{"role":"user","content":"edit files"}]}"#)).unwrap()
    )).await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response_text(response).await.contains("approved response"));
}

fn turn_request(endpoint: &str, stream: bool) -> Request<Body> {
    let mut body = serde_json::json!({"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off", "stream":stream});
    if endpoint == "/v1/responses" {
        body["input"] = serde_json::json!("hello");
    } else {
        body["messages"] = serde_json::json!([{"role":"user", "content":"hello"}]);
    }
    Request::post(endpoint)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn unrelated_turn_events_do_not_extend_idle_timeout() {
    for endpoint in ["/v1/responses", "/v1/chat/completions"] {
        for stream in [false, true] {
            let (app, runtime) =
                test_app_with_runtime(&["--silent-turn"], Duration::from_millis(60)).await;
            let publisher = runtime.clone();
            let noise = tokio::spawn(async move {
                loop {
                    publisher.publish(serde_json::json!({"method":"item/agentMessage/delta", "params":{
                        "threadId":"another_thread", "turnId":"another_turn", "delta":"unrelated"
                    }}));
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
            let result = tokio::time::timeout(Duration::from_secs(1), async {
                let response = app.oneshot(turn_request(endpoint, stream)).await.unwrap();
                if !stream {
                    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
                }
                response_text(response).await
            })
            .await;
            noise.abort();
            runtime.shutdown().await.unwrap();
            let body = result.expect("another turn must not keep the idle turn alive");
            assert!(
                body.contains("timeout") || body.contains("timed out"),
                "{endpoint}: {body}"
            );
            assert!(!body.contains("unrelated"), "{body}");
        }
    }
}

#[tokio::test]
async fn codex_exit_finishes_active_http_requests_promptly() {
    for endpoint in ["/v1/responses", "/v1/chat/completions"] {
        for stream in [false, true] {
            let (app, runtime) =
                test_app_with_runtime(&["--exit-during-turn"], Duration::from_secs(600)).await;
            let result = tokio::time::timeout(Duration::from_secs(2), async {
                let response = app.oneshot(turn_request(endpoint, stream)).await.unwrap();
                if !stream {
                    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
                }
                response_text(response).await
            })
            .await;
            runtime.shutdown().await.unwrap();
            let body = result
                .expect("Codex exit must end the request without waiting for the idle timeout");
            assert!(body.contains("error"), "{body}");
            assert!(!body.contains("timed out"), "{body}");
        }
    }
}

#[tokio::test]
async fn disconnected_stream_interrupts_a_silent_codex_turn() {
    for endpoint in ["/v1/responses", "/v1/chat/completions"] {
        let (app, runtime) =
            test_app_with_runtime(&["--silent-turn"], Duration::from_secs(600)).await;
        let mut events = runtime.subscribe();
        let response = app.oneshot(turn_request(endpoint, true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = events.recv().await.unwrap();
                if event["method"] == "test/interrupted" {
                    assert_eq!(event["params"]["threadId"], "thread_fake_1");
                    assert_eq!(event["params"]["turnId"], "turn_fake_1");
                    break;
                }
            }
        })
        .await;
        runtime.shutdown().await.unwrap();
        result.expect("disconnect must interrupt even if Codex produces no more events");
    }
}

#[tokio::test]
async fn codex_receives_multimodal_input_schema_and_chat_reasoning() {
    let schema = serde_json::json!({"type":"object", "properties":{"answer":{"type":"string"}}, "required":["answer"], "additionalProperties":false});
    let url = "data:image/png;base64,iVBORw0KGgo=";
    for endpoint in ["/v1/responses", "/v1/chat/completions"] {
        let (app, runtime) =
            test_app_with_runtime(&["--model-pages", "--echo-turn"], Duration::from_secs(5)).await;
        let mut request = serde_json::json!({"model":"chatgpt/gpt-test-second"});
        if endpoint == "/v1/responses" {
            request["input"] = serde_json::json!([{"role":"user", "content":[
                {"type":"input_text", "text":"describe"}, {"type":"input_image", "image_url":url, "detail":"high"}
            ]}]);
            request["text"] = serde_json::json!({"format":{"type":"json_schema", "name":"answer", "schema":schema, "strict":true}});
            request["reasoning"] = serde_json::json!({"effort":"high"});
        } else {
            request["messages"] = serde_json::json!([{"role":"user", "content":[
                {"type":"text", "text":"describe"}, {"type":"image_url", "image_url":{"url":url, "detail":"high"}}
            ]}]);
            request["response_format"] = serde_json::json!({"type":"json_schema", "json_schema":{"name":"answer", "schema":schema, "strict":true}});
            request["reasoning_effort"] = serde_json::json!("high");
        }
        let response = app
            .oneshot(
                Request::post(endpoint)
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&response_text(response).await).unwrap();
        let text = if endpoint == "/v1/responses" {
            &body["output"][0]["content"][0]["text"]
        } else {
            &body["choices"][0]["message"]["content"]
        };
        let turn: serde_json::Value = serde_json::from_str(text.as_str().unwrap()).unwrap();
        assert_eq!(
            turn["input"],
            serde_json::json!([
                {"type":"text", "text":"[user]\n"}, {"type":"text", "text":"describe"}, {"type":"image", "url":url, "detail":"high"}
            ])
        );
        assert_eq!(turn["outputSchema"], schema);
        assert_eq!(turn["effort"], "high");
        assert_eq!(turn["model"], "gpt-test-second");
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn invalid_input_and_output_formats_are_rejected() {
    for (endpoint, fields) in [
        (
            "/v1/responses",
            serde_json::json!({"input":[{"type":"input_text"}]}),
        ),
        (
            "/v1/responses",
            serde_json::json!({"input":[{"type":"input_image", "image_url":"file:///etc/passwd"}]}),
        ),
        (
            "/v1/responses",
            serde_json::json!({"input":"hi", "text":{"format":{"type":"json_schema"}}}),
        ),
        (
            "/v1/chat/completions",
            serde_json::json!({"messages":[{"role":"user", "content":"hi"}], "response_format":{"type":"json_object"}}),
        ),
    ] {
        let (app, runtime) = test_app_with_runtime(&[], Duration::from_secs(5)).await;
        let mut body = fields;
        body["model"] = serde_json::json!("hoshikage/unsloth-gemma4-12b-qat-thinking-off");
        let response = app
            .oneshot(
                Request::post(endpoint)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{}",
            response_text(response).await
        );
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn model_catalog_follows_pages_and_rejects_cursor_loops() {
    for repeat in [false, true] {
        let args = if repeat {
            vec!["--model-pages", "--repeat-model-cursor"]
        } else {
            vec!["--model-pages"]
        };
        let (app, runtime) = test_app_with_runtime(&args, Duration::from_secs(5)).await;
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            app.oneshot(Request::get("/v1/models").body(Body::empty()).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&response_text(response).await).unwrap();
        let ids = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids.contains(&"chatgpt/gpt-test-first"), !repeat);
        assert_eq!(ids.contains(&"chatgpt/gpt-test-second"), !repeat);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn continuation_resumes_thread_and_reports_missing_rollouts() {
    for missing in [false, true] {
        let args = if missing {
            vec!["--missing-resume-thread"]
        } else {
            vec![]
        };
        let (app, runtime) = test_app_with_runtime(&args, Duration::from_secs(5)).await;
        let response = app
            .clone()
            .oneshot(turn_request("/v1/responses", false))
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&response_text(response).await).unwrap();
        let request = serde_json::json!({
            "model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off",
            "input":"continue", "previous_response_id":body["id"]
        });
        let response = app
            .oneshot(
                Request::post("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if missing {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            }
        );
        if missing {
            assert!(response_text(response).await.contains("thread_not_found"));
        }
        runtime.shutdown().await.unwrap();
    }
}

async fn wait_approval(app: &axum::Router, turn: &str) -> String {
    for _ in 0..100 {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/v1/codex/turns/{turn}/approvals"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&response_text(response).await).unwrap();
        if let Some(id) = value
            .pointer("/data/0/approval_id")
            .and_then(serde_json::Value::as_str)
        {
            return id.into();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("approval was not registered");
}

async fn api(
    app: &axum::Router,
    path: &str,
    body: Option<serde_json::Value>,
    key: Option<&str>,
) -> Response {
    let mut request = if body.is_some() {
        Request::post(path)
    } else {
        Request::get(path)
    };
    request = request.header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    app.clone()
        .oneshot(
            request
                .body(
                    body.map(|b| Body::from(b.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap()
}
fn start_body(stream: bool) -> serde_json::Value {
    serde_json::json!({"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","input":"hello","stream":stream})
}
async fn json_body(response: Response) -> serde_json::Value {
    serde_json::from_str(&response_text(response).await).unwrap()
}

#[tokio::test]
async fn request_identity_dedup_and_conflict_survive_restart() {
    let (app, runtime, store) = test_app_with_store(&[], Duration::from_secs(20), None).await;
    let response = api(
        &app,
        "/v1/responses",
        Some(start_body(false)),
        Some("req-1"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response_id = response.headers()["x-response-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.headers()["x-codex-thread-id"], "thread_fake_1");
    assert_eq!(response.headers()["x-codex-turn-id"], "turn_fake_1");
    let body = json_body(response).await;
    assert_eq!(body["id"], response_id);
    let duplicate = api(
        &app,
        "/v1/responses",
        Some(start_body(false)),
        Some("req-1"),
    )
    .await;
    assert_eq!(json_body(duplicate).await["response_id"], response_id);
    let mut changed = start_body(false);
    changed["input"] = "different".into();
    assert_eq!(
        api(&app, "/v1/responses", Some(changed), Some("req-1"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    runtime.shutdown().await.unwrap();
    let root = store
        .path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let reopened = Arc::new(ResponseStore::open(root).await.unwrap());
    let (app, runtime, _) = test_app_with_store(
        &["--read-unavailable"],
        Duration::from_secs(20),
        Some(reopened),
    )
    .await;
    let record = json_body(api(&app, "/v1/codex/requests/req-1", None, None).await).await;
    assert_eq!(record["response_id"], response_id);
    assert_eq!(record["last_observed_status"], "completed");
    let snapshot =
        json_body(api(&app, "/v1/codex/turns/turn_fake_1/status", None, None).await).await;
    assert_eq!(snapshot["status"], "unknown");
    assert_eq!(snapshot["last_observed_status"], "completed");
    assert_eq!(
        json_body(
            api(
                &app,
                "/v1/responses",
                Some(start_body(false)),
                Some("req-1")
            )
            .await
        )
        .await["response_id"],
        response_id
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_start_reply_is_unknown_and_never_replayed() {
    let (app, runtime, store) =
        test_app_with_store(&["--exit-before-start-reply"], Duration::from_secs(5), None).await;
    let first = api(
        &app,
        "/v1/responses",
        Some(start_body(true)),
        Some("crash-1"),
    )
    .await;
    assert_eq!(first.status(), StatusCode::BAD_GATEWAY);
    let record = json_body(api(&app, "/v1/codex/requests/crash-1", None, None).await).await;
    assert_eq!(record["phase"], "unknown");
    assert!(record["turn_id"].is_null());
    assert_eq!(record["thread_id"], "thread_fake_1");
    let duplicate = json_body(
        api(
            &app,
            "/v1/responses",
            Some(start_body(true)),
            Some("crash-1"),
        )
        .await,
    )
    .await;
    assert_eq!(duplicate["response_id"], record["response_id"]);
    assert_eq!(store.control.records().unwrap().len(), 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn steer_and_interrupt_validate_target_and_do_not_start_new_turns() {
    let (app, runtime) = test_app_with_runtime(&["--silent-turn"], Duration::from_secs(30)).await;
    let running = api(
        &app,
        "/v1/responses",
        Some(start_body(true)),
        Some("active"),
    )
    .await;
    assert_eq!(running.status(), StatusCode::OK);
    assert_eq!(running.headers()["x-codex-turn-id"], "turn_fake_1");
    let steer = "/v1/codex/turns/turn_fake_1/steer";
    let invalid = serde_json::json!({"expected_turn_id":"other","input":"correction"});
    assert_eq!(
        api(&app, steer, Some(invalid), None).await.status(),
        StatusCode::CONFLICT
    );
    let valid = serde_json::json!({"expected_turn_id":"turn_fake_1","input":"correction"});
    assert_eq!(
        api(&app, steer, Some(valid.clone()), None).await.status(),
        StatusCode::ACCEPTED
    );
    let mut override_body = valid.clone();
    override_body["cwd"] = "/tmp".into();
    assert_eq!(
        api(&app, steer, Some(override_body), None).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let interrupt = "/v1/codex/turns/turn_fake_1/interrupt";
    assert_eq!(
        api(&app, interrupt, Some(serde_json::json!({})), None)
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        api(&app, interrupt, Some(serde_json::json!({})), None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        api(&app, steer, Some(valid), None).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        api(
            &app,
            "/v1/codex/turns/absent/interrupt",
            Some(serde_json::json!({})),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        api(&app, "/v1/codex/turns/absent/events/stream", None, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let body = response_text(running).await;
    assert!(body.contains("response.failed"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_suppression_target_check_and_restart_unique_ids() {
    let (app, runtime) = test_app_with_runtime(
        &["--approval", "--workspace-file-approval"],
        Duration::from_secs(30),
    )
    .await;
    let mut body = start_body(true);
    body["metadata"] = serde_json::json!({"codex.approval_capability":"interactive","codex.auto_approve_workspace":"false"});
    let running = api(&app, "/v1/responses", Some(body.clone()), None).await;
    let id = wait_approval(&app, "turn_fake_1").await;
    let path = format!("/v1/codex/approvals/{id}");
    assert_eq!(
        api(
            &app,
            &path,
            Some(serde_json::json!({"decision":"accept","expected_turn_id":"wrong"})),
            None
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    let steer = serde_json::json!({"expected_turn_id":"turn_fake_1","input":"correction"});
    assert_eq!(
        api(&app, "/v1/codex/turns/turn_fake_1/steer", Some(steer), None)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    runtime.shutdown().await.unwrap();
    drop(running);
    let (new_app, new_runtime) = test_app_with_runtime(
        &["--approval", "--workspace-file-approval"],
        Duration::from_secs(30),
    )
    .await;
    let running = api(&new_app, "/v1/responses", Some(body), None).await;
    let new_id = wait_approval(&new_app, "turn_fake_1").await;
    assert_ne!(id, new_id);
    assert_eq!(
        api(
            &new_app,
            &path,
            Some(serde_json::json!({"decision":"accept"})),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    new_runtime.shutdown().await.unwrap();
    drop(running);
}

#[tokio::test]
async fn reconnect_snapshot_contains_pending_approval_and_event_ids() {
    let (app, runtime) = test_app_with_runtime(&["--approval"], Duration::from_secs(30)).await;
    let mut body = start_body(true);
    body["metadata"] = serde_json::json!({"codex.approval_capability":"interactive"});
    let running = api(&app, "/v1/responses", Some(body), None).await;
    let id = wait_approval(&app, "turn_fake_1").await;
    for _ in 0..2 {
        let response = api(
            &app,
            "/v1/codex/turns/turn_fake_1/events/stream",
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let mut text = String::new();
        while !text.contains("codex.turn.snapshot") {
            let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Ok(data) = frame.into_data() {
                text.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
        assert!(text.contains(&id), "{text}");
        assert!(text.contains("id:"), "{text}");
        // Dropping only the observer must not cancel execution.
        drop(body);
    }
    let status = json_body(api(&app, "/v1/codex/turns/turn_fake_1/status", None, None).await).await;
    assert_eq!(status["status"], "inProgress");
    runtime.shutdown().await.unwrap();
    drop(running);
}

#[tokio::test]
async fn capabilities_explicitly_decline_output_recovery() {
    let (app, runtime) = test_app_with_runtime(&[], Duration::from_secs(30)).await;
    let capability = json_body(api(&app, "/v1/codex/capabilities", None, None).await).await;
    assert_eq!(capability["contract_version"], "1.0");
    assert_eq!(capability["turn_steer"], true);
    assert_eq!(capability["output_retrieval"], false);
    assert_eq!(
        api(&app, "/v1/codex/requests/missing", None, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_thread_is_serialized_and_suppression_survives_continuation() {
    let (app, runtime) = test_app_with_runtime(
        &[
            "--approval",
            "--workspace-file-approval",
            "--approval-after-first",
        ],
        Duration::from_secs(30),
    )
    .await;
    let mut first = start_body(false);
    first["metadata"] = serde_json::json!({"codex.auto_approve_workspace":"false"});
    let completed = json_body(api(&app, "/v1/responses", Some(first), None).await).await;
    let mut next = start_body(true);
    next["previous_response_id"] = completed["id"].clone();
    next["metadata"] = serde_json::json!({"codex.approval_capability":"interactive","codex.auto_approve_workspace":"true"});
    let running = api(&app, "/v1/responses", Some(next.clone()), None).await;
    assert_eq!(running.status(), StatusCode::OK);
    let id = wait_approval(&app, "turn_fake_2").await;
    assert_eq!(
        api(&app, "/v1/responses", Some(next), None).await.status(),
        StatusCode::CONFLICT
    );
    let old = json_body(api(&app, "/v1/codex/turns/turn_fake_1/status", None, None).await).await;
    assert_eq!(old["status"], "completed");
    let pending =
        json_body(api(&app, "/v1/codex/turns/turn_fake_2/approvals", None, None).await).await;
    assert_eq!(pending["data"][0]["approval_id"], id);
    let decision = serde_json::json!({"decision":"accept","expected_turn_id":"turn_fake_2","expected_thread_id":"thread_fake_1"});
    assert_eq!(
        api(
            &app,
            &format!("/v1/codex/approvals/{id}"),
            Some(decision),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert!(response_text(running).await.contains("response.completed"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn upstream_steer_completion_race_is_not_reported_accepted() {
    for (mode, code) in [
        ("--steer-race", "upstream_control_rejected"),
        ("--upstream-waiting", "turn_not_steerable"),
    ] {
        let (app, runtime) =
            test_app_with_runtime(&["--silent-turn", mode], Duration::from_secs(30)).await;
        let running = api(&app, "/v1/responses", Some(start_body(true)), None).await;
        let response = api(
            &app,
            "/v1/codex/turns/turn_fake_1/steer",
            Some(serde_json::json!({"expected_turn_id":"turn_fake_1","input":"change"})),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(json_body(response).await["error"]["code"], code);
        runtime.shutdown().await.unwrap();
        drop(running);
    }
}

#[tokio::test]
async fn conversation_model_changes_preserve_identity_and_last_accepted_selection() {
    let (app, runtime, store) =
        test_app_with_store(&["--echo-turn"], Duration::from_secs(30), None).await;
    let first = json_body(api(&app, "/v1/responses", Some(start_body(false)), None).await).await;
    let first_id = first["id"].as_str().unwrap();
    let switched = serde_json::json!({"model":"hoshikage/model-b","previous_response_id":first_id,"input":"continue"});
    let second_response = api(&app, "/v1/responses", Some(switched), None).await;
    assert_eq!(second_response.status(), StatusCode::OK);
    assert_eq!(
        second_response.headers()["x-codex-thread-id"],
        "thread_fake_1"
    );
    let second = json_body(second_response).await;
    assert_eq!(second["model"], "hoshikage/model-b");
    let echoed: serde_json::Value =
        serde_json::from_str(second["output"][0]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(echoed["model"], "model-b");
    assert_eq!(echoed["threadId"], "thread_fake_1");
    assert_eq!(
        store.get(first_id).await.unwrap().model_id,
        "hoshikage/unsloth-gemma4-12b-qat-thinking-off"
    );
    let rejected = serde_json::json!({"model":"hoshikage/model-rejected","previous_response_id":first_id,"input":"continue"});
    assert_eq!(
        api(&app, "/v1/responses", Some(rejected), Some("bad-model"))
            .await
            .status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        json_body(api(&app, "/v1/codex/requests/bad-model", None, None).await).await["phase"],
        "rejected"
    );
    // An old response identifies the conversation; it does not revert its latest model.
    let inherited = serde_json::json!({"previous_response_id":first_id,"input":"continue"});
    let next = json_body(api(&app, "/v1/responses", Some(inherited.clone()), None).await).await;
    assert_eq!(next["model"], "hoshikage/model-b");
    runtime.shutdown().await.unwrap();
    let root = store
        .path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let reopened = Arc::new(ResponseStore::open(root).await.unwrap());
    let (app, runtime, _) =
        test_app_with_store(&["--echo-turn"], Duration::from_secs(30), Some(reopened)).await;
    let after_restart = api(&app, "/v1/responses", Some(inherited), None).await;
    assert_eq!(after_restart.status(), StatusCode::OK);
    assert_eq!(
        after_restart.headers()["x-codex-thread-id"],
        "thread_fake_1"
    );
    assert_eq!(json_body(after_restart).await["model"], "hoshikage/model-b");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn cross_provider_change_is_rejected_before_start() {
    let (app, runtime) = test_app_with_runtime(&["--model-pages"], Duration::from_secs(30)).await;
    let first = json_body(api(&app, "/v1/responses", Some(start_body(false)), None).await).await;
    let change = serde_json::json!({"previous_response_id":first["id"],"model":"chatgpt/gpt-test-first","input":"continue"});
    let response = api(&app, "/v1/responses", Some(change), None).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "cross_provider_model_change_unsupported"
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn request_cannot_enable_globally_disabled_auto_approval() {
    let (app, runtime) = test_app_with_runtime(
        &[
            "--approval",
            "--workspace-file-approval",
            "--global-auto-approval-off",
        ],
        Duration::from_secs(30),
    )
    .await;
    let mut body = start_body(true);
    body["metadata"] = serde_json::json!({"codex.approval_capability":"interactive","codex.auto_approve_workspace":"true"});
    let running = api(&app, "/v1/responses", Some(body), None).await;
    wait_approval(&app, "turn_fake_1").await;
    runtime.shutdown().await.unwrap();
    drop(running);
}

#[tokio::test]
async fn approval_without_turn_id_before_start_reply_is_bound_to_that_execution() {
    let (app, runtime) =
        test_app_with_runtime(&["--approval-before-start-reply"], Duration::from_secs(30)).await;
    let mut body = start_body(true);
    body["metadata"] = serde_json::json!({"codex.approval_capability":"interactive"});
    let running = api(&app, "/v1/responses", Some(body), None).await;
    let id = wait_approval(&app, "turn_fake_1").await;
    let approved = api(
        &app,
        &format!("/v1/codex/approvals/{id}"),
        Some(serde_json::json!({"decision":"accept","expected_turn_id":"turn_fake_1"})),
        None,
    )
    .await;
    assert_eq!(approved.status(), StatusCode::OK);
    assert_eq!(json_body(approved).await["reply_status"], "written");
    assert!(response_text(running).await.contains("response.completed"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_interrupt_is_not_resent_while_completion_is_pending() {
    let (app, runtime) = test_app_with_runtime(
        &["--silent-turn", "--defer-interrupt-completion"],
        Duration::from_secs(30),
    )
    .await;
    let running = api(&app, "/v1/responses", Some(start_body(true)), None).await;
    let mut events = runtime.subscribe();
    for _ in 0..2 {
        let response = api(
            &app,
            "/v1/codex/turns/turn_fake_1/interrupt",
            Some(serde_json::json!({})),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(json_body(response).await["result"], "accepted");
    }
    let mut count = 0;
    while let Ok(event) = events.try_recv() {
        if event["method"] == "test/interrupted" {
            count += 1;
        }
    }
    assert_eq!(count, 1);
    let snapshot =
        json_body(api(&app, "/v1/codex/turns/turn_fake_1/status", None, None).await).await;
    assert_eq!(snapshot["status"], "inProgress");
    runtime.shutdown().await.unwrap();
    drop(running);
}

#[tokio::test]
async fn interrupt_completion_race_preserves_actual_completed_state() {
    let (app, runtime) = test_app_with_runtime(
        &["--silent-turn", "--interrupt-race"],
        Duration::from_secs(30),
    )
    .await;
    let running = api(&app, "/v1/responses", Some(start_body(true)), None).await;
    let response = api(
        &app,
        "/v1/codex/turns/turn_fake_1/interrupt",
        Some(serde_json::json!({})),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let snapshot =
        json_body(api(&app, "/v1/codex/turns/turn_fake_1/status", None, None).await).await;
    assert_eq!(snapshot["status"], "completed");
    assert!(response_text(running).await.contains("response.completed"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_identical_request_ids_reserve_only_one_execution() {
    let (app, runtime, store) =
        test_app_with_store(&["--silent-turn"], Duration::from_secs(30), None).await;
    let body = start_body(true);
    let (first, second) = tokio::join!(
        api(
            &app,
            "/v1/responses",
            Some(body.clone()),
            Some("simultaneous")
        ),
        api(&app, "/v1/responses", Some(body), Some("simultaneous"))
    );
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_ne!(
        first.headers().contains_key("x-codex-turn-id"),
        second.headers().contains_key("x-codex-turn-id")
    );
    assert_eq!(store.control.records().unwrap().len(), 1);
    runtime.shutdown().await.unwrap();
    drop((first, second));
}

#[tokio::test]
async fn monitor_reports_broadcast_gaps_instead_of_silently_losing_events() {
    let (app, runtime) = test_app_with_runtime(&["--silent-turn"], Duration::from_secs(30)).await;
    let running = api(&app, "/v1/responses", Some(start_body(true)), None).await;
    let observer = api(
        &app,
        "/v1/codex/turns/turn_fake_1/events/stream",
        None,
        None,
    )
    .await;
    for _ in 0..600 {
        runtime.publish(serde_json::json!({"method":"unrelated"}));
    }
    let mut body = observer.into_body();
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !text.contains("codex.events.gap") {
            let frame = body.frame().await.unwrap().unwrap();
            if let Ok(bytes) = frame.into_data() {
                text.push_str(std::str::from_utf8(&bytes).unwrap());
            }
        }
    })
    .await
    .unwrap();
    assert!(text.contains("resync_required"));
    runtime.shutdown().await.unwrap();
    drop((running, body));
}

#[tokio::test]
async fn tcp_disconnect_immediately_after_headers_interrupts_silent_turn() {
    let (app, runtime) = test_app_with_runtime(
        &["--silent-turn", "--interrupt-not-active-once"],
        Duration::from_secs(30),
    )
    .await;
    let mut events = runtime.subscribe();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{addr}/v1/responses"))
        .json(&start_body(true))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if events.recv().await.unwrap()["method"] == "test/interrupted" {
                break;
            }
        }
    })
    .await;
    runtime.shutdown().await.unwrap();
    server.abort();
    assert!(
        result.is_ok(),
        "TCP response body drop must interrupt without needing a model delta"
    );
}
