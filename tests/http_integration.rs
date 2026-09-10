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
    let mut raw = RawConfig::default();
    raw.providers.get_mut("chatgpt").unwrap().enabled = false;
    raw.providers.get_mut("hoshikage").unwrap().base_url = None;
    raw.models.insert(
        "hoshikage/unsloth-gemma4-12b-qat-thinking-off".into(),
        RawModelConfig::default(),
    );
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
    let responses = Arc::new(ResponseStore::open(&config.codex_home).await.unwrap());
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
        true,
        journal,
        responses,
    ));
    (app, runtime)
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

    let approval_path = "/v1/codex/approvals/approval_1";
    let mut approval_response = None;
    for _ in 0..20 {
        let response = app
            .clone()
            .oneshot(Request::get(approval_path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            approval_response = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let response = approval_response.expect("approval request is registered");
    assert_eq!(response.status(), StatusCode::OK);
    let first = app
        .clone()
        .oneshot(
            Request::post(approval_path)
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
            Request::post(approval_path)
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
