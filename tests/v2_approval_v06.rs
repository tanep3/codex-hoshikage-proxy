use codex_hoshikage_proxy::v2::{
    approval_policy, approval_v06, interactions, limits::Limits, mcp_grants, service::Service,
    store,
};
use serde_json::{Value, json};
struct Fixture {
    root: std::path::PathBuf,
    s: Service,
    rid: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("v06-{}", uuid::Uuid::new_v4()));
        let s = Service::open_with_limits(
            &root.join("state"),
            &root.join("work"),
            Limits {
                mcp_turn_approval_enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
        let c = s
            .conversation(
                "c",
                &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
            )
            .unwrap();
        let (_,rid)=s.accept(c["resource"]["id"].as_str().unwrap(),"r",&json!({"input":"test","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"u","channel_id":"ch","run_id":"run"}})).unwrap();
        let rid = rid.unwrap();
        let clock = approval_policy::Clock::read().unwrap();
        let (mut policy, mut preparation) = approval_policy::initial(None, &clock).unwrap();
        approval_policy::configuration_intent(
            &mut policy,
            &mut preparation,
            "test-runtime",
            &clock,
        )
        .unwrap();
        approval_policy::ready(&mut policy, &mut preparation, "test-proof", &clock).unwrap();
        approval_policy::turn_intent(&mut policy, &preparation, false, &clock).unwrap();
        approval_policy::started(&mut policy).unwrap();
        s.store
            .update("response", &rid, |r| {
                r["approval_presentation"] =
                    json!({"mode":"source_conversation","profile":approval_policy::PROFILE});
                r["approval_policy"] = policy;
                r["_approval_prepare"] = preparation;
                r["_approval_runtime"] = json!("test-runtime");
                r["thread_id"] = json!("thread");
                r["turn_id"] = json!("turn");
                r["phase"] = json!("started");
                Ok(())
            })
            .unwrap();
        Self { root, s, rid }
    }
    fn receive(&self, id: &str, args: Value) -> String {
        self.receive_tool(id, "unknown", "unreviewed", args)
    }
    fn receive_tool(&self, id: &str, server: &str, tool: &str, args: Value) -> String {
        mcp_grants::observe(&self.s,&json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"type":"mcpToolCall","id":id,"server":server,"tool":tool,"arguments":args}}})).unwrap();
        let req = json!({"threadId":"thread","turnId":"turn","itemId":id,"questions":[{"id":format!("mcp_tool_call_approval_{id}"),"question":"Confirmation","header":"H","isOther":false,"isSecret":false,"options":[{"label":"Allow"},{"label":"Cancel"}]}]});
        assert!(
            interactions::receive(&self.s, &json!(id), "item/tool/requestUserInput", &req).unwrap()
        );
        let list = interactions::list(&self.s, &self.rid).unwrap();
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["operation"]["call_id"] == id)
            .unwrap()["interaction_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn reply(&self, iid: &str, body: &Value) -> codex_hoshikage_proxy::v2::Result<()> {
        self.s.store.transaction(|tx| {
            let i = store::get(tx, "interaction", iid)?;
            let r = store::get(tx, "response", &self.rid)?;
            approval_v06::check_reply(&self.s, tx, &i, &r, body)
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn reply(view: &Value, tokens: Vec<Value>) -> Value {
    json!({"expected_revision":view["revision"],"expected_scope_fingerprint":view["scope_fingerprint"],"approval_view":"requester",
        "expected_presentation_fingerprint":view["presentation_fingerprint"],"expected_policy_binding_id":view["execution_policy"]["binding_id"],"expected_page_tokens":tokens,"response":{"action":"accept","content":{}}})
}
#[test]
fn unknown_semantics_allow_only_after_complete_private_display() {
    let f = Fixture::new();
    let iid = f.receive(
        "call",
        json!({"password":"TEST_SECRET_ONLY","number":9007199254740993u64}),
    );
    let public = approval_v06::get(&f.s, &iid, "source_conversation", 0, None).unwrap();
    assert!(
        public["audience"]
            .as_object()
            .unwrap()
            .contains_key("principal_id")
    );
    assert!(public["audience"]["principal_id"].is_null());
    assert_eq!(public["state"], "private_required");
    assert_eq!(public["reason"], public["diagnostic"]["code"]);
    assert!(public["presentation_id"].is_string());
    assert!(public["presentation_fingerprint"].is_string());
    assert_eq!(public["page"]["count"], 1);
    assert!(public["page"]["token"].is_string());
    let same = approval_v06::get(
        &f.s,
        &iid,
        "source_conversation",
        0,
        public["presentation_id"].as_str(),
    )
    .unwrap();
    assert_eq!(public, same);
    assert_eq!(public["actions"]["allow_once"], false);
    if let Ok(path) = std::env::var("V06_PRIVATE_EVIDENCE") {
        std::fs::write(path, public.to_string()).unwrap();
    }
    assert!(!public.to_string().contains("TEST_SECRET_ONLY"));
    let view = approval_v06::get(&f.s, &iid, "requester", 0, None).unwrap();
    assert_eq!(view["state"], "ready");
    assert_eq!(view["semantic_assessment"]["status"], "unavailable");
    assert_eq!(view["semantic_assessment"]["reason"], "catalog_unavailable");
    assert!(view.to_string().contains("TEST_SECRET_ONLY"));
    let body = reply(&view, vec![view["page"]["token"].clone()]);
    f.reply(&iid, &body).unwrap();
    let mut wrong = body.clone();
    wrong["expected_policy_binding_id"] = json!("other-run");
    assert_eq!(
        f.reply(&iid, &wrong).unwrap_err().code,
        "approval_policy_binding_mismatch"
    );
    let mut turn = body;
    turn["grant_scope"] = json!("turn_tool");
    assert_eq!(
        f.reply(&iid, &turn).unwrap_err().code,
        "turn_grant_ineligible"
    );
    let rows = f.s.store.list("mcp_presentation_v06").unwrap();
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("TEST_SECRET_ONLY")
    );
}
#[test]
fn all_pages_required_and_large_arguments_use_page_delivery() {
    let f = Fixture::new();
    let iid = f.receive("call", json!({"text":"字😀".repeat(12_000)}));
    let op = approval_v06::operation_details(&f.s, &iid).unwrap();
    assert_eq!(op["arguments_delivery"], "presentation_pages");
    assert_eq!(op["argument_integrity"]["status"], "complete");
    assert!(op["arguments"].is_null());
    let first = approval_v06::get(&f.s, &iid, "requester", 0, None).unwrap();
    assert_eq!(first["state"], "ready");
    let mut tokens = vec![first["page"]["token"].clone()];
    assert_eq!(
        f.reply(&iid, &reply(&first, tokens.clone()))
            .unwrap_err()
            .code,
        "presentation_incomplete"
    );
    for n in 1..first["page"]["count"].as_u64().unwrap() {
        let page = approval_v06::get(
            &f.s,
            &iid,
            "requester",
            n as usize,
            first["presentation_id"].as_str(),
        )
        .unwrap();
        assert_eq!(
            page["presentation_fingerprint"],
            first["presentation_fingerprint"]
        );
        tokens.push(page["page"]["token"].clone());
    }
    f.reply(&iid, &reply(&first, tokens.clone())).unwrap();
    tokens.reverse();
    assert!(f.reply(&iid, &reply(&first, tokens)).is_err());
}
#[test]
fn stop_and_input_change_invalidate_display_and_decline_is_independent() {
    let f = Fixture::new();
    let iid = f.receive("call", json!({"text":"test"}));
    let view = approval_v06::get(&f.s, &iid, "requester", 0, None).unwrap();
    let body = reply(&view, vec![view["page"]["token"].clone()]);
    f.s.store
        .update("response", &f.rid, |r| {
            r["input_generation"] = json!(1);
            Ok(())
        })
        .unwrap();
    assert!(f.reply(&iid, &body).is_err());
    f.reply(
        &iid,
        &json!({"expected_revision":1,"response":{"action":"decline"}}),
    )
    .unwrap();
    f.s.store
        .update("response", &f.rid, |r| {
            r["stop_requested"] = json!(true);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        approval_v06::get(&f.s, &iid, "requester", 0, None).unwrap()["actions"]["allow_once"],
        false
    );
}

#[test]
fn requester_query_requires_exact_page_binding() {
    assert!(approval_v06::query(Some("audience=requester&page=1")).is_err());
    assert!(approval_v06::query(Some("page=0&page=1&presentation_id=p")).is_err());
    assert!(approval_v06::query(Some("audience=other")).is_err());
    assert!(approval_v06::query(Some("audience=requester&page=64&presentation_id=p")).is_err());
    assert_eq!(
        approval_v06::query(Some("audience=requester&page=1&presentation_id=p")).unwrap(),
        ("requester".into(), 1, Some("p".into()))
    );
}

#[test]
fn selected_basic_policy_preserves_complete_single_approval_without_catalog() {
    let f = Fixture::new();
    f.s.store
        .update("response", &f.rid, |r| {
            r["approval_policy"]["selection"] = json!({"id":"evaluated-turn","version":1});
            r["approval_policy"]["generation"] = json!("generation");
            r["_approval_runtime"] = json!("runtime");
            r["_approval_prepare"] = json!({"configuration_confirmed":true,"runtime_id":"runtime",
            "config_generation":"temporary"});
            Ok(())
        })
        .unwrap();
    let iid = f.receive("call", json!({"input":"normal"}));
    let i = f.s.store.get("interaction", &iid).unwrap();
    f.s.store
        .update("response", &f.rid, |r| {
            r["_approval_prepare"]["config_generation"] =
                i["operation"]["config_generation"].clone();
            Ok(())
        })
        .unwrap();
    let view = approval_v06::get(&f.s, &iid, "requester", 0, None).unwrap();
    assert_eq!(view["actions"]["allow_once"], true);
    assert_eq!(view["actions"]["allow_turn_tool"], false);
    assert_eq!(view["tool_policy"]["decision"], "not_blocked");
    f.reply(&iid, &reply(&view, vec![view["page"]["token"].clone()]))
        .unwrap();
    f.s.store
        .update("response", &f.rid, |r| {
            r["_approval_prepare"]["runtime_id"] = json!("restarted");
            Ok(())
        })
        .unwrap();
    let stale = approval_v06::get(&f.s, &iid, "requester", 0, None).unwrap();
    assert_eq!(stale["state"], "unavailable");
    assert_eq!(stale["reason"], "policy_check_unavailable");
    assert_eq!(stale["actions"]["decline"], true);
}

#[tokio::test]
async fn evaluated_grant_is_explicit_and_rechecks_each_call_and_run_scope() {
    use codex_hoshikage_proxy::{
        config::{RawConfig, ValidatedConfig},
        runtime::CodexRuntime,
        v2::catalog,
    };
    let f = Fixture::new();
    let mut raw = RawConfig::default();
    raw.server.v2_enabled = false;
    raw.security.allowed_cwds = vec![f.root.to_string_lossy().into_owned()];
    let mut config = ValidatedConfig::from_raw(raw).unwrap();
    config.codex_command = env!("CARGO_BIN_EXE_fake_codex").into();
    config.codex_args = vec!["--v06-evaluated-catalog".into()];
    config.codex_home = f.root.join("fake-home");
    let runtime = CodexRuntime::launch(&config).await.unwrap();
    let first = f.receive_tool(
        "first",
        "playwright",
        "browser_find",
        json!({"text":"normal"}),
    );
    let stored = f.s.store.get("interaction", &first).unwrap();
    f.s.store
        .update("response", &f.rid, |r| {
            r["approval_policy"]["selection"] = json!({"id":"evaluated-turn","version":1});
            r["approval_policy"]["generation"] = json!("policy-generation");
            r["_approval_runtime"] = json!(runtime.id());
            r["_approval_prepare"]["runtime_id"] = json!(runtime.id());
            r["_approval_prepare"]["config_generation"] =
                stored["operation"]["config_generation"].clone();
            Ok(())
        })
        .unwrap();
    let r = f.s.store.get("response", &f.rid).unwrap();
    let key = approval_v06::catalog_key(&f.s, &r).unwrap();
    let receiver = f.s.catalog.request(key.clone(), runtime.clone()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match catalog::Manager::view(receiver.clone()).await {
                catalog::Status::Ready { .. } => break,
                catalog::Status::Failed(reason) => panic!("{reason}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let view = approval_v06::get(&f.s, &first, "source_conversation", 0, None).unwrap();
    assert_eq!(view["actions"]["allow_turn_tool"], true, "{view}");
    assert_eq!(view["renderer"], "evaluated-operation-v1");
    assert_eq!(view["semantic_assessment"]["status"], "evaluated");
    let examples: Value =
        serde_json::from_str(include_str!("../docs/mcp-approval/api-v06-examples.json")).unwrap();
    let actual = view["tool_policy"]
        .as_object()
        .unwrap()
        .keys()
        .collect::<Vec<_>>();
    let expected = examples["presentation_turn_eligible"]["tool_policy"]
        .as_object()
        .unwrap()
        .keys()
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert_eq!(view["tool_policy"]["version"], 1);
    let mut body = reply(&view, vec![view["page"]["token"].clone()]);
    body["approval_view"] = json!("source_conversation");
    f.s.store
        .transaction(|tx| {
            let mut i = store::get(tx, "interaction", &first)?;
            approval_v06::check_reply(&f.s, tx, &i, &r, &body)?;
            mcp_grants::create(&f.s, tx, &r, &mut i, &body)?;
            assert!(i["grant_id"].is_null());
            Ok(())
        })
        .unwrap();
    body["grant_scope"] = json!("turn_tool");
    let gid =
        f.s.store
            .transaction(|tx| {
                let mut i = store::get(tx, "interaction", &first)?;
                approval_v06::check_reply(&f.s, tx, &i, &r, &body)?;
                mcp_grants::create(&f.s, tx, &r, &mut i, &body)?;
                store::put(tx, "interaction", &first, &i)?;
                mcp_grants::finish(tx, &i, true)?;
                Ok(i["grant_id"].as_str().unwrap().to_owned())
            })
            .unwrap();
    let second = f.receive_tool(
        "second",
        "playwright",
        "browser_find",
        json!({"text":"another query"}),
    );
    assert_eq!(
        mcp_grants::auto_grant(&f.s, &second).unwrap(),
        Some(gid.clone())
    );
    let secret = f.receive_tool(
        "secret",
        "playwright",
        "browser_find",
        json!({"text":"password: test"}),
    );
    assert_eq!(mcp_grants::auto_grant(&f.s, &secret).unwrap(), None);
    let extra = f.receive_tool(
        "extra",
        "playwright",
        "browser_find",
        json!({"text":"normal","extra":true}),
    );
    assert_eq!(mcp_grants::auto_grant(&f.s, &extra).unwrap(), None);
    f.s.store
        .update("response", &f.rid, |r| {
            r["approval_context"]["run_id"] = json!("another-run");
            Ok(())
        })
        .unwrap();
    assert_eq!(mcp_grants::auto_grant(&f.s, &second).unwrap(), None);
    let g = f.s.store.get("mcp_grant", &gid).unwrap();
    assert_eq!(g["state"], "revoked");
    assert_eq!(g["grant_policy"]["policy_version"], 1);
    assert!(!g.to_string().contains("another query"));
    runtime.shutdown().await.unwrap();
}

#[test]
fn guard_never_relays_unbound_native_approval_through_legacy_user_input() {
    let f = Fixture::new();
    f.s.store
        .update("response", &f.rid, |r| {
            r["approval_policy"]["selection"] =
                json!({"id":"evaluated-turn-notion-guard","version":1});
            r["interaction_capabilities"] = json!(["mcp_form", "user_input"]);
            Ok(())
        })
        .unwrap();
    let params = json!({"threadId":"thread","turnId":"turn","itemId":"lost-call", "questions":[{"id":"mcp_tool_call_approval_lost-call","header":"Tool","question":"Allow?","isOther":false,"isSecret":false,"options":[{"label":"Allow"},{"label":"Cancel"}]}]});
    let e = interactions::receive(
        &f.s,
        &json!("unbound"),
        "item/tool/requestUserInput",
        &params,
    )
    .unwrap_err();
    assert_eq!(e.code, "policy_check_unavailable");
    assert!(f.s.store.list("interaction").unwrap().is_empty());
}
