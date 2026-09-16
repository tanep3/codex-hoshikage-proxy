use codex_hoshikage_proxy::v2::{
    interactions, limits::Limits, mcp_grants, presentations as p, service::Service, store,
};
use serde_json::{Value, json};

struct Fixture {
    root: std::path::PathBuf,
    s: Service,
    rid: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("inline-{}", uuid::Uuid::new_v4()));
        let mut limits = Limits {
            mcp_turn_approval_enabled: true,
            ..Default::default()
        };
        limits
            .mcp_turn_grant_tools
            .insert("playwright".into(), vec!["browser_find".into()]);
        let s = Service::open_with_limits(&root.join("state"), &root.join("work"), limits).unwrap();
        let c = s
            .conversation(
                "c",
                &json!({"workspace":{"mode":"automatic"},"model":"chatgpt/test"}),
            )
            .unwrap();
        let (_,rid)=s.accept(c["resource"]["id"].as_str().unwrap(),"r",&json!({"input":"test","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"u","channel_id":"ch","run_id":"run"},"approval_presentation":{"mode":"source_conversation"}})).unwrap();
        let rid = rid.unwrap();
        s.store
            .update("response", &rid, |r| {
                r["thread_id"] = json!("thread");
                r["turn_id"] = json!("turn");
                r["phase"] = json!("started");
                Ok(())
            })
            .unwrap();
        Self { root, s, rid }
    }
    fn receive(&self, id: &str, tool: &str, args: Value) -> String {
        mcp_grants::observe(&self.s,&json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"type":"mcpToolCall","id":id,"server":"playwright","tool":tool,"arguments":args}}})).unwrap();
        let req = json!({"threadId":"thread","turnId":"turn","itemId":id,"questions":[{"id":format!("mcp_tool_call_approval_{id}"),"question":"Confirmation","header":"H","isOther":false,"isSecret":false,"options":[{"label":"Allow"},{"label":"Cancel"}]}]});
        assert!(
            interactions::receive(&self.s, &json!(id), "item/tool/requestUserInput", &req).unwrap()
        );
        let items = interactions::list(&self.s, &self.rid).unwrap();
        let i = items["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["operation"]["call_id"] == id)
            .unwrap();
        i["interaction_id"].as_str().unwrap().into()
    }
    fn catalog(&self, iid: &str, catalog: Value) {
        let generation = mcp_grants::operation(&self.s, iid).unwrap()["config_generation"]
            .as_str()
            .unwrap()
            .to_owned();
        p::register_catalog(&self.s, &generation, &catalog);
    }
    fn approve(&self, iid: &str, v: &Value, turn: bool) -> codex_hoshikage_proxy::v2::Result<()> {
        let mut body = json!({"expected_revision":v["revision"],"expected_scope_fingerprint":v["scope_fingerprint"],"approval_view":"source_conversation","expected_presentation_fingerprint":v["presentation_fingerprint"],"response":{"action":"accept","content":{}}});
        if turn {
            body["grant_scope"] = json!("turn_tool");
        }
        self.s.store.transaction(|tx| {
            let i = store::get(tx, "interaction", iid)?;
            let r = store::get(tx, "response", &self.rid)?;
            p::check_reply(&self.s, tx, &i, &r, &body)
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn catalog() -> Value {
    json!({"data":[{"name":"playwright","tools":{
        "browser_find":{"name":"browser_find","inputSchema":{"type":"object","properties":{"text":{"type":"string"},"regex":{"type":"string"}}}},
        "browser_navigate":{"name":"browser_navigate","inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}},
        "browser_tabs":{"name":"browser_tabs","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","new","close","select"]},"index":{"type":"integer"},"url":{"type":"string"}},"required":["action"]}}
    }}]})
}
#[test]
fn normal_query_regex_url_and_list_are_inline_without_persisting_values() {
    let f = Fixture::new();
    for (n, (tool, args, label, value, turn)) in [
        (
            "browser_find",
            json!({"text":"private business project notebook"}),
            "検索語",
            "private business project notebook",
            true,
        ),
        (
            "browser_find",
            json!({"regex":"link \\\"[1-5]位"}),
            "正規表現",
            "link \\\"[1-5]位",
            true,
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/search?q=notebook#results"}),
            "アクセス先URL",
            "https://example.com/search?q=notebook#results",
            false,
        ),
        (
            "browser_tabs",
            json!({"action":"list"}),
            "対象",
            "このMCP接続のブラウザー",
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let iid = f.receive(&n.to_string(), tool, args);
        f.catalog(&iid, catalog());
        let v = p::get(&f.s, &iid).unwrap();
        assert_eq!(v["state"], "inline", "{v}");
        assert_eq!(v["actions"]["allow_turn_tool"], turn);
        assert!(
            v["display"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["label"] == label && x["value"] == value)
        );
        assert_eq!(v, p::get(&f.s, &iid).unwrap());
        assert!(f.approve(&iid, &v, false).is_ok());
        assert_eq!(f.approve(&iid, &v, true).is_ok(), turn);
        let saved = f.s.store.get("mcp_presentation", &iid).unwrap();
        assert_eq!(saved["version_count"], 1);
        assert!(!saved.to_string().contains(value));
        assert!(saved.get("display").is_none());
    }
}
#[test]
fn credentials_encoding_code_and_unknown_inputs_never_leak_to_public_display() {
    for (tool, args, secret) in [
        (
            "browser_find",
            json!({"text":"search https://u:TOPSECRET@example.com/page"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/a/%2e%2e/TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://u:TOPSECRET@example.com/path"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/?access_token=TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/?X-Amz-Signature=TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/#token=TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/?%2561ccess_token=TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_navigate",
            json!({"url":"https://example.com/?redirect=https%3A%2F%2Fexample.org%2FTOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_find",
            json!({"text":"password=TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_find",
            json!({"regex":"Authorization: Bearer TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_find",
            json!({"text":"ordinary","token":"TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_find",
            json!({"text":"ordinary","unexpected":"TOPSECRET"}),
            "TOPSECRET",
        ),
        (
            "browser_evaluate",
            json!({"function":"() => 'TOPSECRET'"}),
            "TOPSECRET",
        ),
        (
            "browser_find",
            json!({"text":"a","regex":"TOPSECRET"}),
            "TOPSECRET",
        ),
        ("browser_find", json!({"text":null}), "TOPSECRET"),
    ] {
        let f = Fixture::new();
        let iid = f.receive("one", tool, args);
        f.catalog(&iid, catalog());
        let v = p::get(&f.s, &iid).unwrap();
        assert_eq!(v["state"], "private_required", "{v}");
        assert_eq!(v["actions"]["allow_once"], false);
        assert!(!v.to_string().contains(secret));
        assert_eq!(
            f.approve(&iid, &v, false).unwrap_err().code,
            "inline_approval_unavailable"
        );
    }
}
#[test]
fn unsupported_catalog_changed_schema_and_long_unicode_fall_back() {
    let f = Fixture::new();
    let iid = f.receive("one", "browser_find", json!({"text":"test"}));
    assert_eq!(
        p::get(&f.s, &iid).unwrap()["reason"],
        "unsupported_renderer"
    );
    let mut c = catalog();
    c["data"][0]["tools"]["browser_find"]["inputSchema"]["properties"]["code"] =
        json!({"type":"string"});
    f.catalog(&iid, c);
    assert_eq!(
        p::get(&f.s, &iid).unwrap()["reason"],
        "unsupported_renderer"
    );
    f.catalog(&iid, catalog());
    assert_eq!(p::get(&f.s, &iid).unwrap()["state"], "inline");
    let iid = f.receive("long", "browser_find", json!({"text":"🦀".repeat(800)}));
    assert_eq!(p::get(&f.s, &iid).unwrap()["reason"], "display_too_large");
}
#[test]
fn old_display_tokens_cross_calls_and_generation_changes_cannot_authorize() {
    let f = Fixture::new();
    let a = f.receive("a", "browser_find", json!({"text":"a"}));
    f.catalog(&a, catalog());
    let first = p::get(&f.s, &a).unwrap();
    let b = f.receive("b", "browser_find", json!({"text":"a"}));
    let other = p::get(&f.s, &b).unwrap();
    assert_ne!(
        first["presentation_fingerprint"],
        other["presentation_fingerprint"]
    );
    assert!(f.approve(&b, &first, false).is_err());
    let mut c = catalog();
    c["data"][0]["tools"]["browser_find"]["description"] = json!("updated definition");
    f.catalog(&a, c);
    assert_eq!(
        f.approve(&a, &first, false).unwrap_err().code,
        "presentation_conflict"
    );
    let second = p::get(&f.s, &a).unwrap();
    assert_ne!(
        first["presentation_fingerprint"],
        second["presentation_fingerprint"]
    );
    assert!(f.approve(&a, &first, false).is_err());
    assert!(f.approve(&a, &second, false).is_ok());
    mcp_grants::steer(&f.s, "thread", "turn").unwrap();
    assert!(f.approve(&a, &second, false).is_err());
    assert_eq!(p::get(&f.s, &a).unwrap()["state"], "unavailable");
}
#[test]
fn maximum_versions_do_not_reset_and_stop_invalidates_display() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"a"}));
    let mut last = Value::Null;
    for n in 0..4 {
        let mut c = catalog();
        c["data"][0]["tools"]["browser_find"]["description"] = json!(n.to_string());
        f.catalog(&iid, c);
        last = p::get(&f.s, &iid).unwrap();
        assert_eq!(last["state"], "inline");
    }
    f.catalog(&iid, catalog());
    assert_eq!(p::get(&f.s, &iid).unwrap()["reason"], "presentation_limit");
    assert!(f.approve(&iid, &last, false).is_err());
    assert_eq!(p::get(&f.s, &iid).unwrap()["reason"], "presentation_limit");
    f.s.stop("stop", &json!({"target":{"response_id":f.rid}}))
        .unwrap();
    assert_eq!(p::get(&f.s, &iid).unwrap()["state"], "unavailable");
}
#[test]
fn request_and_reply_fields_are_explicit_and_legacy_stays_private() {
    let f = Fixture::new();
    for body in [
        json!({"approval_presentation":null}),
        json!({"approval_presentation":{"mode":"source_conversation"}}),
        json!({"approval_context":{},"approval_presentation":{"mode":"public"}}),
    ] {
        assert!(p::validate_request(&body).is_err());
    }
    for body in [
        json!({"approval_view":"source_conversation"}),
        json!({"expected_presentation_fingerprint":"x"}),
        json!({"approval_view":"unknown","expected_presentation_fingerprint":"x"}),
    ] {
        assert!(p::validate_reply(&body).is_err());
    }
    assert!(p::validate_reply(&json!({"response":{"action":"accept"}})).is_ok());
    let iid = f.receive("a", "browser_find", json!({"text":"a"}));
    f.catalog(&iid, catalog());
    f.s.store
        .update("response", &f.rid, |r| {
            r["approval_presentation"] = Value::Null;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        p::get(&f.s, &iid).unwrap_err().code,
        "presentation_context_mismatch"
    );
    assert_eq!(
        mcp_grants::operation(&f.s, &iid).unwrap()["disclosure"],
        "requester_only"
    );
}
#[test]
fn sqlite_failure_does_not_issue_an_unrecorded_presentation() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"a"}));
    f.catalog(&iid, catalog());
    let db = rusqlite::Connection::open(f.root.join("state/metadata.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER block_presentation BEFORE INSERT ON records WHEN NEW.kind='mcp_presentation' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(
        p::get(&f.s, &iid).unwrap_err().code,
        "presentation_store_unavailable"
    );
    assert!(f.s.store.list("mcp_presentation").unwrap().is_empty());
    assert!(f.s.store.list("mcp_presentation_audit").unwrap().is_empty());
}
#[test]
fn transport_loss_and_reopening_store_cannot_restore_live_tokens() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"a"}));
    f.catalog(&iid, catalog());
    let first = p::get(&f.s, &iid).unwrap();
    mcp_grants::observe(&f.s, &json!({"kind":"transport_closed"})).unwrap();
    assert!(f.approve(&iid, &first, false).is_err());
    assert_eq!(p::get(&f.s, &iid).unwrap()["state"], "unavailable");
}

#[test]
fn catalog_revert_and_call_argument_replacement_cannot_revive_a_card() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"first"}));
    f.catalog(&iid, catalog());
    let old = p::get(&f.s, &iid).unwrap();
    f.catalog(&iid, json!({"data":[]}));
    f.catalog(&iid, catalog());
    assert!(f.approve(&iid, &old, false).is_err());
    let current = p::get(&f.s, &iid).unwrap();
    assert_ne!(
        old["presentation_fingerprint"],
        current["presentation_fingerprint"]
    );
    mcp_grants::observe(&f.s,&json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"type":"mcpToolCall","id":"a","server":"playwright","tool":"browser_find","arguments":{"text":"changed"}}}})).unwrap();
    assert!(f.approve(&iid, &current, false).is_err());
    assert_eq!(p::get(&f.s, &iid).unwrap()["state"], "unavailable");
}

#[test]
fn copied_database_reopen_has_no_live_presentation_or_private_display() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"restart-target"}));
    f.catalog(&iid, catalog());
    let v = p::get(&f.s, &iid).unwrap();
    let reopened_root = f.root.join("reopened");
    std::fs::create_dir_all(&reopened_root).unwrap();
    let db = rusqlite::Connection::open(f.root.join("state/metadata.sqlite3")).unwrap();
    db.execute(
        "VACUUM INTO ?1",
        [reopened_root.join("metadata.sqlite3").to_str().unwrap()],
    )
    .unwrap();
    drop(db);
    let reopened =
        Service::open_with_limits(&reopened_root, &f.root.join("work"), f.s.limits.clone())
            .unwrap();
    let after = p::get(&reopened, &iid).unwrap();
    assert_eq!(after["state"], "unavailable");
    assert!(after["presentation_fingerprint"].is_null());
    assert!(!after.to_string().contains("restart-target"));
    let body = json!({"approval_view":"source_conversation","expected_presentation_fingerprint":v["presentation_fingerprint"],"expected_scope_fingerprint":v["scope_fingerprint"],"response":{"action":"accept"}});
    assert!(
        reopened
            .store
            .transaction(|tx| p::check_reply(
                &reopened,
                tx,
                &store::get(tx, "interaction", &iid)?,
                &store::get(tx, "response", &f.rid)?,
                &body
            ))
            .is_err()
    );
}

#[tokio::test]
#[ignore = "isolated real Codex catalog verification; no AI turn or tool approval"]
async fn live_catalog_matches_evaluated_browser_renderers() {
    use codex_hoshikage_proxy::{
        config::{RawConfig, ValidatedConfig},
        runtime::CodexRuntime,
    };
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    std::fs::set_permissions(&f.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let auth = std::env::var_os("CODEX_TEST_AUTH").expect("CODEX_TEST_AUTH");
    let path = std::env::var_os("CODEX_TEST_MCP_CONFIG").expect("CODEX_TEST_MCP_CONFIG");
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    // Preserve the actual approval policy; do not manufacture prompts for normally allowed tools.
    let isolated=toml::Value::try_from(json!({"mcp_servers":{"playwright":source["mcp_servers"]["playwright"]},"features":{"apps":false}})).unwrap();
    let user = f.root.join("user");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("config.toml"),
        toml::to_string(&isolated).unwrap(),
    )
    .unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("test-only".into());
    raw.server.default_cwd = Some(f.root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![f.root.to_string_lossy().into()];
    raw.codex.user_home = Some(user);
    raw.v2.mcp_turn_approval_enabled = true;
    let mut cfg = ValidatedConfig::from_raw(raw).unwrap();
    cfg.codex_home = f.root.join("codex");
    cfg.prepare_codex_home().unwrap();
    std::fs::copy(auth, cfg.codex_home.join("auth.json")).unwrap();
    let live = CodexRuntime::launch(&cfg).await.unwrap();
    let outcome = async {
        live.request("thread/start",json!({"cwd":f.root,"modelProvider":"openai","model":"gpt-5.6-luna","approvalPolicy":"on-request","sandbox":"workspace-write"})).await.unwrap();
        p::refresh_catalog(&f.s, &live).await;
        for (id, tool, args) in [
            ("find", "browser_find", json!({"text":"notebook"})),
            (
                "navigate",
                "browser_navigate",
                json!({"url":"https://example.com/"}),
            ),
            ("tabs", "browser_tabs", json!({"action":"list"})),
        ] {
            let iid = f.receive(id, tool, args);
            let v = p::get(&f.s, &iid).unwrap();
            assert_eq!(v["state"], "inline", "live catalog incompatible: {v}");
        }
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(45), outcome).await;
    live.shutdown().await.unwrap();
    result.unwrap();
    println!(
        "PASS real Codex catalog: find/navigate/tabs schemas; no AI turn, browser action, or approval-policy changes"
    );
}

#[test]
fn changed_principal_channel_run_scope_never_reuses_public_consent() {
    for field in ["principal_id", "channel_id", "run_id"] {
        let f = Fixture::new();
        let iid = f.receive("a", "browser_find", json!({"text":"a"}));
        f.catalog(&iid, catalog());
        let view = p::get(&f.s, &iid).unwrap();
        f.s.store
            .update("response", &f.rid, |r| {
                r["approval_context"][field] = json!("other");
                Ok(())
            })
            .unwrap();
        assert!(f.approve(&iid, &view, false).is_err());
        assert_eq!(p::get(&f.s, &iid).unwrap()["state"], "unavailable");
    }
}

#[test]
fn persisted_deadline_cannot_be_extended_by_reissuing_display() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"a"}));
    f.catalog(&iid, catalog());
    let view = p::get(&f.s, &iid).unwrap();
    f.s.store
        .update("mcp_presentation", &iid, |v| {
            v["expires_at_ms"] = json!(0);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.approve(&iid, &view, false).unwrap_err().code,
        "presentation_expired"
    );
    // An ended original interaction can never obtain a fresh lifetime via GET.
    f.s.store
        .update("interaction", &iid, |i| {
            i["expires_at_ms"] = json!(0);
            Ok(())
        })
        .unwrap();
    let expired = p::get(&f.s, &iid).unwrap();
    assert_eq!(expired["state"], "unavailable");
    assert!(expired["presentation_fingerprint"].is_null());
}

#[test]
fn malformed_presentation_record_is_not_silently_replaced() {
    let f = Fixture::new();
    let iid = f.receive("a", "browser_find", json!({"text":"a"}));
    f.catalog(&iid, catalog());
    p::get(&f.s, &iid).unwrap();
    let db = rusqlite::Connection::open(f.root.join("state/metadata.sqlite3")).unwrap();
    db.execute(
        "UPDATE records SET value='not-json' WHERE kind='mcp_presentation'",
        [],
    )
    .unwrap();
    assert_eq!(p::get(&f.s, &iid).unwrap_err().code, "store_corrupt");
}

#[test]
fn disabling_feature_preserves_same_key_acceptance_but_rejects_new_declarations() {
    let mut f = Fixture::new();
    let r = f.s.store.get("response", &f.rid).unwrap();
    let cid = r["conversation_id"].as_str().unwrap();
    f.s.limits.mcp_turn_approval_enabled = false;
    let body = json!({"input":"test","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"u","channel_id":"ch","run_id":"run"},"approval_presentation":{"mode":"source_conversation"}});
    let (op, dispatch) = f.s.accept(cid, "r", &body).unwrap();
    assert_eq!(op["resource"]["id"], f.rid);
    assert!(dispatch.is_none());
    assert_eq!(
        f.s.accept(cid, "new-declaration", &body).unwrap_err().code,
        "inline_approval_disabled"
    );
}
