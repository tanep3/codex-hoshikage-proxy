//! Real browser comparison on an operator-prepared public ranking page.
//! Opens the fixed public page in a private MCP session, then reads with browser_find.
use codex_hoshikage_proxy::{
    approval_manager::ApprovalManager,
    config::{RawConfig, ValidatedConfig},
    runtime::CodexRuntime,
    v2::{interactions, mcp_grants, service::Service},
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
#[tokio::test]
#[ignore = "real browser/model; operator must prepare the public ranking page"]
async fn real_browser_ranking_single_vs_turn_grant() {
    for turn_grant in [false, true] {
        run_case(turn_grant).await;
    }
}
async fn run_case(turn_grant: bool) {
    let auth = std::env::var_os("CODEX_TEST_AUTH").expect("set CODEX_TEST_AUTH");
    let root = std::env::temp_dir().join(format!("live-turn-grants-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("user")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = std::env::var_os("CODEX_TEST_MCP_CONFIG").expect("set CODEX_TEST_MCP_CONFIG");
    let config: toml::Value = toml::from_str(&fs::read_to_string(source).unwrap()).unwrap();
    let mut server = config["mcp_servers"]["playwright"].clone();
    server.as_table_mut().unwrap().insert(
        "enabled_tools".into(),
        toml::Value::try_from(vec!["browser_find", "browser_navigate"]).unwrap(),
    );
    server.as_table_mut().unwrap().remove("disabled_tools");
    server.as_table_mut().unwrap().insert(
        "tools".into(),
        toml::Value::try_from(json!({"browser_find":{"approval_mode":"prompt"},"browser_navigate":{"approval_mode":"prompt"}})).unwrap(),
    );
    let cfg = toml::Value::try_from(
        json!({"features":{"apps":false},"mcp_servers":{"playwright":server}}),
    )
    .unwrap();
    fs::write(
        root.join("user/config.toml"),
        toml::to_string(&cfg).unwrap(),
    )
    .unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("isolated-test".into());
    raw.server.default_cwd = Some(root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    raw.codex.user_home = Some(root.join("user"));
    raw.v2.mcp_turn_approval_enabled = true;
    raw.v2
        .mcp_turn_grant_tools
        .insert("playwright".into(), vec!["browser_find".into()]);
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
        let catalog=live.request("mcpServerStatus/list",json!({"limit":100})).await.unwrap();
        let server=catalog["data"].as_array().unwrap().iter().find(|s|s["name"]=="playwright").expect("playwright must be available");
        assert_eq!(server["tools"].as_object().unwrap().len(),2,"only browser_find and browser_navigate may be exposed");
        let c=s.conversation("c",&json!({"workspace":{"mode":"automatic"},"model":"chatgpt/gpt-5.6-luna"})).unwrap();
        let (_,rid)=s.accept(c["resource"]["id"].as_str().unwrap(),"r",&json!({"input":"test","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"test","channel_id":"isolated","run_id":"one"}})).unwrap();
        let rid=rid.unwrap();
        s.store.update("response",&rid,|r|{r["phase"]=json!("dispatching");r["thread_id"]=tid.clone();Ok(())}).unwrap();
        let mut events=live.subscribe();
        let started=live.request("turn/start",json!({"threadId":tid,"input":[{"type":"text","text":"最初にmcp__playwright__browser_navigateでhttps://kakaku.com/pc/note-pc/ranking_0020/を1回だけ開いてください。その後、この価格.comのノートパソコン人気売れ筋ランキングの上位5件を調べてください。mcp__playwright__browser_findを必要ならtool searchで探し、順番に5回だけ使ってください。各回はregexで link \"1位 、link \"2位 、link \"3位 、link \"4位 、link \"5位 をそれぞれ検索してください。最初の指定URL以外へのページ遷移、クリック、コード評価、shell、他ツール実行はしないでください。最後に取得できた1〜5位の商品名を順位順に回答してください。価格.comのランキングが得られなければ中止してください。"}]})).await.unwrap();
        s.store.update("response",&rid,|r|{r["phase"]=json!("started");r["turn_id"]=started["turn"]["id"].clone();Ok(())}).unwrap();
        let mut clicks=0;let mut navigation_replies=0;let mut completed=0;let mut answer=String::new();
        tokio::time::timeout(Duration::from_secs(180),async {
            loop {
                if !turn_grant || clicks==0 {
                    let list=interactions::list(&s,&rid).unwrap();
                    if let Some(i)=list["data"].as_array().unwrap().iter().find(|i|i["state"]=="pending") {
                        let iid=i["interaction_id"].as_str().unwrap();
                        let op=mcp_grants::operation(&s,iid).unwrap();
                        let navigate=op["tool"]=="browser_navigate";
                        if navigate {assert_eq!(navigation_replies,0);assert_eq!(op["arguments"]["url"],"https://kakaku.com/pc/note-pc/ranking_0020/");assert_eq!(op["turn_grant_eligible"],false);}
                        else {assert_eq!(op["tool"],"browser_find");assert_eq!(op["turn_grant_eligible"],true);}
                        let mut body=json!({"expected_revision":op["revision"],"expected_scope_fingerprint":op["scope_fingerprint"],"response":{"action":"accept","content":{}}});
                        if turn_grant && !navigate {body["grant_scope"]=json!("turn_tool");}
                        let result=interactions::reply(&s,&live,iid,&format!("manual-{navigation_replies}-{clicks}"),&body).await.unwrap();
                        assert_eq!(result["state"],"succeeded");if navigate {navigation_replies+=1;} else {clicks+=1;}
                    }
                }
                tokio::select! {
                    event=events.recv()=>{
                        let e=event.unwrap();
                        if e["method"]=="item/completed" && e["params"]["item"]["type"]=="mcpToolCall" {
                            if e["params"]["item"]["tool"]=="browser_navigate" {assert_eq!(e["params"]["item"]["status"],"completed");continue;}
                            assert_eq!(e["params"]["item"]["tool"],"browser_find");
                            assert_eq!(e["params"]["item"]["status"],"completed"); completed+=1;
                        }
                        if e["method"]=="item/agentMessage/delta" {answer.push_str(e["params"]["delta"].as_str().unwrap_or(""));}
                        if e["method"]=="turn/completed" {break;}
                    }
                    _=tokio::time::sleep(Duration::from_millis(20))=>{}
                }
            }
        }).await.expect("five tool calls must finish");
        assert_eq!(navigation_replies,1);assert_eq!(clicks,if turn_grant{1}else{5});assert_eq!(completed,5);
        for expected in ["OmniBook 3 16","LAVIE Direct N15","Yoga Tab","FMV Note E","OmniBook 3 14"] {assert!(answer.contains(expected),"ranking answer missing {expected}; actual answer: {answer}");}
        if turn_grant {
        // Observer tasks may process the terminal notification just after this receiver.
        tokio::time::timeout(Duration::from_secs(2),async {
            loop {let grants=mcp_grants::list(&s,&rid).unwrap();
                assert_eq!(grants["data"][0]["application_count"],5);
                if grants["data"][0]["state"]=="expired" {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        } else {assert!(mcp_grants::list(&s,&rid).unwrap()["data"].as_array().unwrap().is_empty());}
        if turn_grant {
            // This harness drives runtime directly, so record the already observed
            // terminal state before accepting the next durable Response.
            s.store.update("response",&rid,|r|{r["phase"]=json!("finished");r["hold_state"]=json!("released");Ok(())}).unwrap();
            let (_,next)=s.accept(c["resource"]["id"].as_str().unwrap(),"next-user-input",&json!({"input":"again","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"test","channel_id":"isolated","run_id":"two"}})).unwrap();
            let next=next.unwrap();
            s.store.update("response",&next,|r|{r["phase"]=json!("dispatching");r["thread_id"]=tid.clone();Ok(())}).unwrap();
            let turn=live.request("turn/start",json!({"threadId":tid,"input":[{"type":"text","text":"同じページの第1位をもう一度mcp__playwright__browser_findで1回だけ検索して商品名を回答してください。他ツールは使わないでください。"}]})).await.unwrap();
            s.store.update("response",&next,|r|{r["phase"]=json!("started");r["turn_id"]=turn["turn"]["id"].clone();Ok(())}).unwrap();
            let first=tokio::time::timeout(Duration::from_secs(90),async {
                loop {let list=interactions::list(&s,&next).unwrap();if let Some(i)=list["data"].as_array().unwrap().first(){break i.clone();}tokio::time::sleep(Duration::from_millis(25)).await;}
            }).await.unwrap();
            let iid=first["interaction_id"].as_str().unwrap();
            assert_eq!(first["state"],"pending");assert!(first["grant_id"].is_null());
            assert_eq!(mcp_grants::auto_grant(&s,iid).unwrap(),None);
            let d=mcp_grants::operation(&s,iid).unwrap();assert_eq!(d["tool"],"browser_find");
            let op=interactions::reply(&s,&live,iid,"next-single-confirmation",&json!({"expected_revision":d["revision"],"expected_scope_fingerprint":d["scope_fingerprint"],"response":{"action":"accept","content":{}}})).await.unwrap();
            assert_eq!(op["state"],"succeeded");
            tokio::time::timeout(Duration::from_secs(90),async {loop {let e=events.recv().await.unwrap();if e["method"]=="turn/completed" && e["params"]["turn"]["id"]==turn["turn"]["id"]{break;}}}).await.unwrap();
            assert!(mcp_grants::list(&s,&next).unwrap()["data"].as_array().unwrap().is_empty());
            assert_eq!(mcp_grants::list(&s,&rid).unwrap()["data"][0]["application_count"],5);
            println!("PASS next user turn: new explicit confirmation required; prior grant not reused");
        }
        println!("PASS actual ranking browser_find: completed={completed}, explicit_replies={clicks}, turn_grant={turn_grant}, top_five_verified=true");
    }).await;
    let stopped = runtime.shutdown().await;
    fs::remove_dir_all(root).unwrap();
    stopped.unwrap();
    result.unwrap();
}
