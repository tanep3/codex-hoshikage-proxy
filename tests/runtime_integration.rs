use codex_hoshikage_proxy::{
    config::{RawConfig, ValidatedConfig},
    domain::RuntimeState,
    runtime::CodexRuntime,
};
use std::{env, path::PathBuf, time::Duration};

fn fake_config(args: &[&str]) -> ValidatedConfig {
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
        "/tmp/codex-hoshikage-proxy-runtime-test-{}",
        std::process::id()
    ));
    config
}

#[tokio::test]
async fn graceful_shutdown_reaps_fake_codex() {
    let runtime = CodexRuntime::launch(&fake_config(&[]))
        .await
        .expect("fake runtime launches");
    assert_eq!(runtime.snapshot().await, RuntimeState::Ready);

    tokio::time::timeout(Duration::from_secs(2), runtime.shutdown())
        .await
        .expect("shutdown does not hang")
        .expect("shutdown succeeds");
    assert_eq!(runtime.snapshot().await, RuntimeState::Stopped);
}

#[tokio::test]
async fn exited_codex_fails_pending_request() {
    let runtime = CodexRuntime::launch(&fake_config(&["--exit-after-initialize"]))
        .await
        .expect("fake runtime initializes before exit");

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.request("thread/start", serde_json::json!({})),
    )
    .await
    .expect("pending request resolves after transport exit");
    assert!(result.is_err());
}
