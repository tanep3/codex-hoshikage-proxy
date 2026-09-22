use codex_hoshikage_proxy::{
    config::{RawConfig, ValidatedConfig},
    runtime::CodexRuntime,
};
use serde_json::json;
use std::fs;
#[tokio::test]
async fn refresh_same_thread_retries_failed_reload_and_preserves_private_config() {
    let root = std::env::temp_dir().join(format!("mcp-refresh-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("user")).unwrap();
    let source = root.join("user/config.toml");
    fs::write(&source, "").unwrap();
    let mut raw = RawConfig::default();
    raw.server.default_cwd = Some(root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    raw.codex.user_home = Some(root.join("user"));
    let mut c = ValidatedConfig::from_raw(raw).unwrap();
    c.codex_home = root.join("private");
    c.codex_command = env!("CARGO_BIN_EXE_fake_codex").into();
    c.codex_args = vec!["--fail-first-mcp-reload".into()];
    c.prepare_codex_home().unwrap();
    let runtime = CodexRuntime::launch(&c).await.unwrap();
    let t = runtime.request("thread/start", json!({})).await.unwrap();
    let id = t["thread"]["id"].clone();
    fs::write(
        &source,
        "sandbox_mode='danger-full-access'\n[mcp_servers.new]\ncommand='test'\n",
    )
    .unwrap();
    assert!(
        runtime
            .request("thread/resume", json!({"threadId":id}))
            .await
            .is_err()
    );
    let status = runtime
        .request("test/reload-status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["reloads"], 1);
    assert_eq!(status["threads"], 1);
    runtime
        .request("thread/resume", json!({"threadId":id}))
        .await
        .unwrap();
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(c.codex_home.join("config.toml")).unwrap()).unwrap();
    assert_eq!(config["sandbox_mode"].as_str(), Some("workspace-write"));
    assert_eq!(
        config["mcp_servers"]["new"]["command"].as_str(),
        Some("test")
    );
    runtime
        .request("thread/resume", json!({"threadId":id}))
        .await
        .unwrap();
    assert_eq!(
        runtime
            .request("test/reload-status", json!({}))
            .await
            .unwrap()["reloads"],
        2
    );
    fs::write(&source, "token='secret-broken\n").unwrap();
    let error = runtime
        .request("thread/resume", json!({"threadId":id}))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("secret-broken"));
    assert_eq!(
        runtime
            .request("test/reload-status", json!({}))
            .await
            .unwrap()["reloads"],
        2
    );
    fs::write(&source, "").unwrap();
    runtime
        .request("thread/resume", json!({"threadId":id}))
        .await
        .unwrap();
    assert_eq!(
        runtime
            .request("test/reload-status", json!({}))
            .await
            .unwrap()["reloads"],
        3
    );
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(c.codex_home.join("config.toml")).unwrap()).unwrap();
    assert!(config.get("mcp_servers").is_none());
    runtime.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}
