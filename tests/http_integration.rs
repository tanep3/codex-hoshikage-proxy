use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use codex_hoshikage_proxy::{
    config::{RawConfig, ValidatedConfig},
    http::{AppState, router},
    journal::EventJournal,
    model::ModelRegistry,
    runtime::CodexRuntime,
    store::ResponseStore,
};
use http_body_util::BodyExt;
use std::{env, path::PathBuf, sync::Arc, time::Duration};
use tower::ServiceExt;

async fn test_app(args: &[&str]) -> axum::Router {
    let mut raw = RawConfig::default();
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
        args.len()
    ));
    let runtime = CodexRuntime::launch(&config)
        .await
        .expect("fake runtime launches");
    let journal = Arc::new(EventJournal::open(&config.codex_home).await.unwrap());
    let responses = Arc::new(ResponseStore::open(&config.codex_home).await.unwrap());
    let models = ModelRegistry::from_config(&config.models).unwrap();
    router(AppState::new(
        runtime,
        models,
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
