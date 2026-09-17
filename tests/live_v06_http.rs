use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use codex_hoshikage_proxy::{
    catalog::ModelCatalogManager,
    config::{RawConfig, ValidatedConfig},
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
async fn live_app() -> (Router, Arc<Service>, Arc<CodexRuntime>, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("v06-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("user")).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(root.join("user/config.toml"),format!("[features]\napps=false\n[mcp_servers.playwright]\ncommand='python3'\nargs=['{}','{}']\nrequired=true\n[mcp_servers.playwright.tools.browser_find]\napproval_mode='prompt'\n",repo.join("scripts/fixtures/v06_read_mcp.py").display(),repo.join("tests/fixtures/mcp-v06-catalog.json").display())).unwrap();
    if std::env::var_os("CODEX_TEST_APPS").is_some() {
        let file = root.join("user/config.toml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(file, text.replace("apps=false", "apps=true")).unwrap();
    }
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("test-key".into());
    raw.server.default_cwd = Some(root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    raw.codex.user_home = Some(root.join("user"));
    raw.v2.mcp_turn_approval_enabled = true;
    let mut config = ValidatedConfig::from_raw(raw).unwrap();
    config.codex_home = root.join("private");
    config.prepare_codex_home().unwrap();
    std::fs::copy(
        std::env::var_os("CODEX_TEST_AUTH").expect("CODEX_TEST_AUTH required"),
        config.codex_home.join("auth.json"),
    )
    .unwrap();
    let runtime = CodexRuntime::launch(&config).await.unwrap();
    if std::env::var_os("CODEX_TEST_APPS").is_some() {
        let req = runtime
            .request("configRequirements/read", Value::Null)
            .await
            .unwrap();
        let cat = runtime
            .request(
                "mcpServerStatus/list",
                json!({"limit":100,"detail":"toolsAndAuthOnly"}),
            )
            .await
            .unwrap();
        let summary=cat["data"].as_array().unwrap().iter().map(|server| {
            let tools=server["tools"].as_object().unwrap();
            let target=tools.get("notion.notion-update-page");
            json!({"server":server["name"],"runtime":server["runtimeStatus"],"auth":server["authStatus"],"tools":tools.len(),
                "notion_present":target.is_some(),"notion_validated":target.is_some_and(|t|codex_hoshikage_proxy::v2::approval_config::Target::from_definition("codex_apps",t).is_ok())})
        }).collect::<Vec<_>>();
        println!(
            "guard preflight: requirements_null={}, servers={}",
            req["requirements"].is_null(),
            json!(summary)
        );
    }
    let mut state = AppState::new(
        runtime.clone(),
        ModelCatalogManager::new(config.models.clone(), runtime.clone()).unwrap(),
        config.cwd_policy,
        config.default_cwd,
        Some("test-key".into()),
        Duration::from_secs(150),
        Duration::from_secs(150),
        3,
        Duration::from_secs(1),
        "workspace-write".into(),
        Duration::from_secs(15),
        false,
        Arc::new(EventJournal::open(&root).await.unwrap()),
        Arc::new(ResponseStore::open(&root).await.unwrap()),
    );
    let service = Arc::new(
        Service::open_with_limits(&root.join("state"), &root.join("work"), config.v2_limits)
            .unwrap(),
    );
    service.mcp.lock().unwrap().config_paths = vec![
        config.codex_home.join("config.toml"),
        root.join("user/config.toml"),
    ];
    state.v2 = Some(service.clone());
    codex_hoshikage_proxy::v2::events::start_maintenance(state.clone(), service.clone());
    (router(state), service, runtime, root)
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
#[ignore = "real Codex/model with isolated local read-only MCP; CODEX_TEST_AUTH required"]
async fn live_v06_full_http_turn_grant_and_next_turn_isolation() {
    let (app, s, runtime, root) = live_app().await;
    let app_copy = app.clone();
    let svc = s.clone();
    let evidence = root.clone();
    let diagnostic_runtime = runtime.clone();
    let outcome=tokio::spawn(async move {
        let app=app_copy;let s=svc;
        let (_,cap)=call(&app,&s,"GET","capabilities",None,json!({})).await;
        assert_eq!(cap["mcp_approval_v06"]["enabled"],true);
        std::fs::write(evidence.join("capability.json"),cap.to_string()).unwrap();
        let (status,op)=call(&app,&s,"POST","conversations",Some("conversation"),json!({"workspace":{"mode":"automatic"},"model":"chatgpt/gpt-5.6-luna"})).await;
        assert_eq!(status,StatusCode::ACCEPTED,"{op}");let cid=op["resource"]["id"].as_str().unwrap();
        let mut previous_thread=Value::Null;
        for index in 0..2 {
            let count=if index==0 {5}else{1};
            let prompt=format!("This is an isolated acceptance test. Find mcp__playwright__browser_find using tool search if necessary. Call it exactly {count} times sequentially with text set to test1 through test{count}. It is a local read-only fixture that always returns LOCAL_READ_RESULT. Do not use shell, browser navigation or any other tool except tool search and browser_find. After all calls reply DONE.");
            let body=json!({"input":prompt,"interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"test","channel_id":"isolated","run_id":format!("run{index}")},"approval_presentation":{"profile":"source-conversation-v3","mode":"source_conversation"},"approval_policy":if index==0 {json!({"id":if std::env::var_os("CODEX_TEST_APPS").is_some(){"evaluated-turn-notion-guard"}else{"evaluated-turn"},"version":1})}else{Value::Null}});
            let (status,op)=call(&app,&s,"POST",&format!("conversations/{cid}/responses"),Some(&format!("run{index}")),body).await;
            assert_eq!(status,StatusCode::ACCEPTED,"{op}");let rid=op["resource"]["id"].as_str().unwrap();
            let mut manual=0;let mut seen=std::collections::HashSet::new();
            let terminal=tokio::time::timeout(Duration::from_secs(180),async {
                loop {
                    let (_,r)=call(&app,&s,"GET",&format!("responses/{rid}"),None,json!({})).await;
                    if matches!(r["phase"].as_str(),Some("finished"|"unknown"|"rejected"|"cancelled")) {break r;}
                    let (_,list)=call(&app,&s,"GET",&format!("responses/{rid}/interactions"),None,json!({})).await;
                    for i in list["data"].as_array().unwrap() {
                        let iid=i["interaction_id"].as_str().unwrap();
                        if i["state"]!="pending" || seen.contains(iid) || (index==0 && manual>0) {continue;}
                        let (status,view)=call(&app,&s,"GET",&format!("interactions/{iid}/presentation"),None,json!({})).await;
                        assert_eq!(status,StatusCode::OK,"{view}");
                        if view["state"]=="refreshing" {continue;}
                        assert_eq!(view["actions"]["allow_once"],true,"{view}");
                        assert_eq!(view["actions"]["allow_turn_tool"],index==0,"{view}");
                        std::fs::write(evidence.join(format!("view{index}.json")),view.to_string()).unwrap();
                        let mut reply=json!({"expected_revision":view["revision"],"expected_scope_fingerprint":view["scope_fingerprint"],"approval_view":"source_conversation","expected_presentation_fingerprint":view["presentation_fingerprint"],"expected_policy_binding_id":view["execution_policy"]["binding_id"],"expected_page_tokens":[view["page"]["token"]],"response":{"action":"accept","content":{}}});
                        if index==0 {reply["grant_scope"]=json!("turn_tool");}
                        let (status,result)=call(&app,&s,"POST",&format!("interactions/{iid}/reply"),Some(&format!("reply{index}")),reply).await;
                        assert_eq!(status,StatusCode::ACCEPTED,"{result}");manual+=1;seen.insert(iid.to_owned());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }).await.expect("real model execution deadline");
            if terminal["execution_status"]!="completed" && terminal["thread_id"].is_string() {
                for thread in [Value::Null,terminal["thread_id"].clone()] {
                    let mut params=json!({"limit":100,"detail":"toolsAndAuthOnly"});if !thread.is_null(){params["threadId"]=thread.clone();}
                    let cat=diagnostic_runtime.request("mcpServerStatus/list",params).await.unwrap();
                    let summary=cat["data"].as_array().unwrap().iter().map(|server| {
                        let tools=server["tools"].as_object().unwrap();let target=tools.get("notion.notion-update-page");
                        json!({"server":server["name"],"runtime":server["runtimeStatus"],"auth":server["authStatus"],"tools":tools.len(),"target":target.map(|t|codex_hoshikage_proxy::v2::approval_config::Target::from_definition("codex_apps",t).map(|t|t.definition).unwrap_or_else(|e|e.code.into()))})
                    }).collect::<Vec<_>>();println!("guard postflight thread={thread} summary={}",json!(summary));
                }
            }
            assert_eq!(terminal["execution_status"],"completed","{terminal}");assert_eq!(manual,1);
            assert_eq!(s.store.list("interaction").unwrap().iter().filter(|i|i["response_id"]==rid).count(),count);
            if index>0 {assert_eq!(previous_thread,terminal["thread_id"]);}
            previous_thread=terminal["thread_id"].clone();
            let (_,grants)=call(&app,&s,"GET",&format!("responses/{rid}/mcp-grants"),None,json!({})).await;
            if index==0 {assert_eq!(grants["data"][0]["application_count"],5,"{grants}");}else{assert_eq!(grants["data"],json!([]));}
            std::fs::write(evidence.join(format!("response{index}.json")),terminal.to_string()).unwrap();
            std::fs::write(evidence.join(format!("grants{index}.json")),grants.to_string()).unwrap();
        }
    }).await;
    let _ = runtime.shutdown().await;
    // Never leave copied credentials in test evidence, including on assertion failure.
    let _ = std::fs::remove_file(root.join("private/auth.json"));
    println!("isolated v06 evidence: {}", root.display());
    outcome.unwrap();
}
