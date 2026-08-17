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
    router(AppState::new(
        runtime,
        catalog,
        config.cwd_policy.clone(),
        config.default_cwd.clone(),
        None,
        Duration::from_secs(5),
        journal,
        responses,
    ))
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
    let app = test_app(&["--approval"]).await;
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
    drop(response);

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
