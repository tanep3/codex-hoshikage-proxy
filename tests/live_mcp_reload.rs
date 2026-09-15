//! Explicit real-model test using a private home and local read-only MCP.
use codex_hoshikage_proxy::{
    config::{RawConfig, ValidatedConfig},
    runtime::CodexRuntime,
};
use serde_json::{Value, json};
use std::{fs, sync::Arc, time::Duration};
async fn turn(runtime: &Arc<CodexRuntime>, id: &Value, prompt: &str) -> Value {
    let mut events = runtime.subscribe();
    let started = runtime
        .request(
            "turn/start",
            json!({"threadId":id,"input":[{"type":"text","text":prompt}]}),
        )
        .await
        .unwrap();
    let tid = &started["turn"]["id"];
    tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            let event = events.recv().await.unwrap();
            if event["method"] == "turn/completed"
                && event["params"]["threadId"] == *id
                && event["params"]["turn"]["id"] == *tid
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let read = runtime
        .request("thread/read", json!({"threadId":id,"includeTurns":true}))
        .await
        .unwrap();
    read["thread"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == *tid)
        .unwrap()
        .clone()
}
#[tokio::test]
#[ignore = "real Codex model calls; run explicitly"]
async fn added_mcp_is_called_in_existing_thread_without_restart() {
    let root = std::env::temp_dir().join(format!("live-reload-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("user")).unwrap();
    fs::write(root.join("user/config.toml"), "").unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("live-reload-test".into());
    raw.server.default_cwd = Some(root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    raw.codex.user_home = Some(root.join("user"));
    let mut config = ValidatedConfig::from_raw(raw).unwrap();
    config.codex_home = root.join("private");
    config.prepare_codex_home().unwrap();
    fs::copy(
        "/home/tane/.config/codex-hoshikage-proxy/codex-home/auth.json",
        config.codex_home.join("auth.json"),
    )
    .unwrap();
    let runtime = CodexRuntime::launch(&config).await.unwrap();
    let live = runtime.clone();
    let work = root.clone();
    let result=tokio::spawn(async move {
        let thread=live.request("thread/start",json!({"cwd":work,"modelProvider":"openai","model":"gpt-5.6-luna","approvalPolicy":"never","sandbox":"workspace-write"})).await.unwrap();
        let id=&thread["thread"]["id"];
        let first=turn(&live,id,"Reply BEFORE_RELOAD. Do not call tools.").await;
        assert_eq!(first["status"],"completed");
        let fixture=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/reload_mcp.py");
        fs::write(work.join("user/config.toml"),format!("[mcp_servers.reload_test]\ncommand = 'python3'\nargs = ['{}']\n",fixture.display())).unwrap();
        let second=turn(&live,id,"Call the reload_test MCP reload_probe tool once. Do not run shell commands or other tools. Report its result; if unavailable say so.").await;
        assert_eq!(second["status"],"completed");
        assert!(second["items"].as_array().unwrap().iter().any(|i|i["type"]=="mcpToolCall" && i["server"]=="reload_test" && i["tool"]=="reload_probe" && i["status"]=="completed"),"new MCP was not called successfully");
        fs::write(work.join("user/config.toml"),"").unwrap();
        let third=turn(&live,id,"The MCP was removed. Reply AFTER_REMOVAL without calling tools.").await;
        assert_eq!(third["status"],"completed");
        let list=live.request("mcpServerStatus/list",json!({"limit":100})).await.unwrap();
        assert!(!list["data"].as_array().unwrap().iter().any(|s|s["name"]=="reload_test"));
        println!("PASS same thread {}: before / added tool call / removal; no process restart",id);
    }).await;
    let stopped = runtime.shutdown().await;
    fs::remove_dir_all(root).unwrap();
    stopped.unwrap();
    result.unwrap();
}
