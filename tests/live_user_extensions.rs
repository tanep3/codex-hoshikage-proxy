//! Opt-in discovery against the user's configured MCP servers; no model call.
use codex_hoshikage_proxy::{
    config::{RawConfig, ValidatedConfig},
    runtime::CodexRuntime,
};
use serde_json::json;
#[tokio::test]
#[ignore = "live user MCP/plugin discovery, run explicitly"]
async fn discover_global_extensions() {
    let root = std::env::temp_dir().join(format!("extension-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("work")).unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("test".into());
    raw.server.default_cwd = Some(root.join("work").to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    let source = root.join("user");
    std::fs::create_dir_all(source.join("skills/disabled-probe")).unwrap();
    std::fs::write(source.join("skills/disabled-probe/SKILL.md"), "---\nname: disabled-probe\ndescription: Test disabled skill inheritance.\n---\nDo not run.\n").unwrap();
    for entry in std::fs::read_dir("/home/tane/.codex/skills").unwrap() {
        let entry = entry.unwrap();
        std::os::unix::fs::symlink(entry.path(), source.join("skills").join(entry.file_name()))
            .unwrap();
    }
    for folder in ["plugins", ".tmp"] {
        std::os::unix::fs::symlink(
            std::path::Path::new("/home/tane/.codex").join(folder),
            source.join(folder),
        )
        .unwrap();
    }
    let mut shared: toml::Value =
        toml::from_str(&std::fs::read_to_string("/home/tane/.codex/config.toml").unwrap()).unwrap();
    shared.as_table_mut().unwrap().insert(
        "skills".into(),
        toml::toml! { config = [{path = "skills/disabled-probe/SKILL.md", enabled = false}] }
            .into(),
    );
    if let Some(markets) = shared
        .get_mut("marketplaces")
        .and_then(toml::Value::as_table_mut)
    {
        for (_, market) in markets.iter_mut() {
            if let Some(p) = market.get("source").and_then(toml::Value::as_str)
                && let Ok(r) = std::path::Path::new(p).strip_prefix("/home/tane/.codex")
            {
                market["source"] = toml::Value::String(source.join(r).to_string_lossy().into());
            }
        }
    }
    std::fs::write(
        source.join("config.toml"),
        toml::to_string(&shared).unwrap(),
    )
    .unwrap();
    raw.codex.user_home = Some(source);
    let mut c = ValidatedConfig::from_raw(raw).unwrap();
    c.codex_home = root.join("codex");
    c.prepare_codex_home().unwrap();
    std::fs::copy(
        "/home/tane/.config/codex-hoshikage-proxy/codex-home/auth.json",
        c.codex_home.join("auth.json"),
    )
    .unwrap();
    let runtime = CodexRuntime::launch(&c).await.unwrap();
    let live_runtime = runtime.clone();
    let live_root = root.clone();
    let mut task = tokio::spawn(async move {
        let runtime = live_runtime;
        let root = live_root;
        let effective = runtime
            .request("config/read", json!({"includeLayers":false}))
            .await
            .unwrap();
        println!(
            "EFFECTIVE_EXTENSION_CONFIG {}",
            json!({"marketplaces":effective["config"]["marketplaces"],"plugins":effective["config"]["plugins"]})
        );
        let skills = runtime
            .request(
                "skills/list",
                json!({"cwds":[root.join("work")],"forceReload":true}),
            )
            .await
            .unwrap();
        let skill_names: Vec<_> = skills["data"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["skills"].as_array().unwrap())
            .map(|s| json!({"name":s["name"],"enabled":s["enabled"]}))
            .collect();
        println!("SKILLS {}", json!(skill_names));
        let plugins = runtime
            .request(
                "plugin/list",
                json!({"cwds":[root.join("work")],"marketplaceKinds":["local","vertical"],"forceRefetch":false}),
            )
            .await
            .unwrap();
        let plugin_names: Vec<_> = plugins["marketplaces"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["plugins"].as_array().unwrap())
            .map(|p| json!({"name":p["name"],"enabled":p["enabled"],"installed":p["installed"]}))
            .collect();
        println!("PLUGINS {}", json!(plugin_names));
        println!("PLUGIN_ERRORS {}", plugins["marketplaceLoadErrors"]);
        let mut mcp = runtime
            .request("mcpServerStatus/list", json!({"limit":100}))
            .await
            .unwrap();
        for _ in 0..30 {
            if mcp["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == "serena")
                .and_then(|s| s["tools"].as_object())
                .is_some_and(|t| !t.is_empty())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            mcp = runtime
                .request("mcpServerStatus/list", json!({"limit":100}))
                .await
                .unwrap();
        }
        for name in ["lightpanda", "node_repl", "serena"] {
            assert!(
                mcp["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["name"] == name
                        && s["tools"].as_object().is_some_and(|t| !t.is_empty())),
                "MCP not ready: {name}"
            );
        }
        let browser = mcp["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "lightpanda")
            .unwrap();
        println!(
            "LIGHTPANDA_TOOLS {}",
            json!(
                browser["tools"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>()
            )
        );
        let thread=runtime.request("thread/start",json!({"cwd":root.join("work"),"modelProvider":"openai","model":"gpt-5.6-luna","approvalPolicy":"on-request","sandbox":"workspace-write"})).await.unwrap();
        let called=runtime.request("mcpServer/tool/call",json!({"threadId":thread["thread"]["id"],"server":"lightpanda","tool":"session_list","arguments":{}})).await.unwrap();
        assert_ne!(called["isError"], true);
        println!("MCP_CALL session_list PASS (result content omitted)");
        assert!(
            skill_names
                .iter()
                .any(|s| s["name"] == "disabled-probe" && s["enabled"] == false),
            "disabled skill must remain disabled"
        );
        assert!(skill_names.iter().any(|s| s["name"] == "talmon-browser"));
        assert!(
            skill_names
                .iter()
                .any(|s| s["name"] == "sites:sites-building")
        );
        let servers:Vec<_>=mcp["data"].as_array().unwrap().iter().map(|s|json!({"name":s["name"],"auth":s["authStatus"],"runtime":s["runtimeStatus"],"tools":s["tools"].as_object().map(|t|t.len())})).collect();
        println!("MCP {}", json!(servers));
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), &mut task).await;
    if result.is_err() {
        task.abort();
        let _ = task.await;
    }
    let shutdown = runtime.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
    shutdown.unwrap();
    assert!(
        matches!(result, Ok(Ok(()))),
        "live extension probe failed: {result:?}"
    );
}
