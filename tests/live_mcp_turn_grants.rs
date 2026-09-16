//! Isolated real-model acceptance. The local MCP only returns a fixed string.
use codex_hoshikage_proxy::{
    approval_manager::ApprovalManager,
    config::{RawConfig, ValidatedConfig},
    runtime::CodexRuntime,
    v2::{interactions, mcp_grants, service::Service},
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
#[tokio::test]
#[ignore = "real model; CODEX_TEST_AUTH must point to an existing auth.json"]
async fn real_model_five_calls_with_one_run_grant() {
    let auth = std::env::var_os("CODEX_TEST_AUTH").expect("set CODEX_TEST_AUTH");
    let root = std::env::temp_dir().join(format!("live-turn-grants-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("user")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/fixtures/turn_approval_mcp.py");
    fs::write(root.join("user/config.toml"),format!("[features]\napps=false\n[mcp_servers.turn_test]\ncommand='python3'\nargs=['{}']\nrequired=true\n[mcp_servers.turn_test.tools.read_test]\napproval_mode='prompt'\n",fixture.display())).unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("isolated-test".into());
    raw.server.default_cwd = Some(root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    raw.codex.user_home = Some(root.join("user"));
    raw.v2.mcp_turn_approval_enabled = true;
    raw.v2
        .mcp_turn_grant_tools
        .insert("turn_test".into(), vec!["read_test".into()]);
    let mut config = ValidatedConfig::from_raw(raw).unwrap();
    config.codex_home = root.join("private");
    config.prepare_codex_home().unwrap();
    fs::copy(auth, config.codex_home.join("auth.json")).unwrap();
    let runtime = CodexRuntime::launch(&config).await.unwrap();
    let live = runtime.clone();
    let work = root.clone();
    let result=tokio::spawn(async move {
        let s=Arc::new(Service::open_with_limits(&work.join("state"),&work.join("work"),config.v2_limits.clone()).unwrap());
        s.mcp.lock().unwrap().config_paths=vec![config.codex_home.join("config.toml"),work.join("user/config.toml")];
        let manager=ApprovalManager::new(live.clone(),Duration::from_secs(150),false);
        manager.attach_interaction_relay(&s);manager.start();
        let thread=live.request("thread/start",json!({"cwd":work,"modelProvider":"openai","model":"gpt-5.6-luna","approvalPolicy":"on-request","sandbox":"workspace-write","config":{"features.apps":false}})).await.unwrap();
        let tid=&thread["thread"]["id"];
        let c=s.conversation("c",&json!({"workspace":{"mode":"automatic"},"model":"chatgpt/gpt-5.6-luna"})).unwrap();
        let (_,rid)=s.accept(c["resource"]["id"].as_str().unwrap(),"r",&json!({"input":"test","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"test","channel_id":"isolated","run_id":"one"}})).unwrap();
        let rid=rid.unwrap();
        s.store.update("response",&rid,|r|{r["phase"]=json!("dispatching");r["thread_id"]=tid.clone();Ok(())}).unwrap();
        let mut events=live.subscribe();
        let started=live.request("turn/start",json!({"threadId":tid,"input":[{"type":"text","text":"Use tool search if necessary to find mcp__turn_test__read_test. Call that tool exactly five times sequentially, with function set to test1, test2, test3, test4, test5 respectively. The tool is a harmless fixture returning a fixed string. Do not use any other MCP tools, shell, or browser tools. After all five results reply DONE."}]})).await.unwrap();
        s.store.update("response",&rid,|r|{r["phase"]=json!("started");r["turn_id"]=started["turn"]["id"].clone();Ok(())}).unwrap();
        let mut clicks=0;let mut completed=0;
        tokio::time::timeout(Duration::from_secs(180),async {
            loop {
                if clicks==0 {
                    let list=interactions::list(&s,&rid).unwrap();
                    if let Some(i)=list["data"].as_array().unwrap().first() {
                        let iid=i["interaction_id"].as_str().unwrap();
                        let op=mcp_grants::operation(&s,iid).unwrap();
                        assert_eq!(op["tool"],"read_test");assert_eq!(op["turn_grant_eligible"],true);
                        let result=interactions::reply(&s,&live,iid,"explicit-one",&json!({"expected_revision":op["revision"],"expected_scope_fingerprint":op["scope_fingerprint"],"grant_scope":"turn_tool","response":{"action":"accept","content":{}}})).await.unwrap();
                        assert_eq!(result["state"],"succeeded");clicks+=1;
                    }
                }
                tokio::select! {
                    event=events.recv()=>{
                        let e=event.unwrap();
                        if e["method"]=="item/completed" && e["params"]["item"]["type"]=="mcpToolCall" {
                            assert_eq!(e["params"]["item"]["tool"],"read_test");
                            assert_eq!(e["params"]["item"]["status"],"completed");completed+=1;
                        }
                        if e["method"]=="turn/completed" {break;}
                    }
                    _=tokio::time::sleep(Duration::from_millis(20))=>{}
                }
            }
        }).await.expect("five tool calls must finish");
        assert_eq!(clicks,1);assert_eq!(completed,5);
        // Observer tasks may process the terminal notification just after this receiver.
        tokio::time::timeout(Duration::from_secs(2),async {
            loop {let grants=mcp_grants::list(&s,&rid).unwrap();
                assert_eq!(grants["data"][0]["application_count"],5);
                if grants["data"][0]["state"]=="expired" {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        println!("PASS real model: five completed local MCP calls, one explicit grant, expired at turn end");
    }).await;
    let stopped = runtime.shutdown().await;
    fs::remove_dir_all(root).unwrap();
    stopped.unwrap();
    result.unwrap();
}
