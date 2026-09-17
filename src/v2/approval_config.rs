//! Prepare an explicit Notion guard without changing other tools' policies.
use super::{Error, Result};
use serde_json::{Value, json};

pub const NOTION_TOOL: &str = "notion.notion-update-page";
const NOTION_SCHEMA: &str = "91a1d310ad660a548e4be6393d0194b67aae48e4c4b64c494b2c376cff76f956";
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub connector: String,
    pub link: Option<String>,
    pub definition: String,
}
fn identifier(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .map(str::to_owned)
}
impl Target {
    pub fn from_definition(server: &str, definition: &Value) -> Result<Self> {
        if server != "codex_apps"
            || definition["name"] != NOTION_TOOL
            || crate::control::fingerprint(&definition["inputSchema"]) != NOTION_SCHEMA
        {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
        let meta = &definition["_meta"];
        let connector = identifier(&meta["connector_id"])
            .ok_or_else(|| Error::code(409, "policy_configuration_conflict"))?;
        let link = if meta["link_id"].is_null() {
            None
        } else {
            Some(
                identifier(&meta["link_id"])
                    .ok_or_else(|| Error::code(409, "policy_configuration_conflict"))?,
            )
        };
        if meta["resource_name"] != NOTION_TOOL {
            return Err(Error::code(409, "policy_configuration_conflict"));
        }
        Ok(Self {
            connector,
            link,
            definition: crate::control::fingerprint(definition),
        })
    }
}
/// `requirements` is the result object's requirements field. Non-null cannot
/// currently prove absence of managed Apps overrides in the pinned upstream API.
pub fn guard_overrides(config: &Value, requirements: &Value, target: &Target) -> Result<Value> {
    if !requirements.is_null() {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    let mut apps = match config.get("apps") {
        None | Some(Value::Null) => json!({}),
        Some(v) if v.is_object() => v.clone(),
        _ => return Err(Error::code(409, "policy_configuration_conflict")),
    };
    let app = &apps[&target.connector];
    let link = target
        .link
        .as_ref()
        .map(|id| &app["links"][id])
        .unwrap_or(&Value::Null);
    let reviewer = link
        .get("approvals_reviewer")
        .filter(|v| !v.is_null())
        .or_else(|| app.get("approvals_reviewer").filter(|v| !v.is_null()))
        .or_else(|| {
            apps["_default"]
                .get("approvals_reviewer")
                .filter(|v| !v.is_null())
        })
        .or_else(|| config.get("approvals_reviewer").filter(|v| !v.is_null()));
    if reviewer.is_some_and(|v| !v.is_null() && v != "user") {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    // Only this precise tool's approval mode changes. In particular do not set
    // app.enabled/tools.enabled or app/link reviewer defaults.
    if apps[&target.connector].is_null() {
        apps[&target.connector] = json!({});
    }
    if !apps[&target.connector].is_object() {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    let app = &mut apps[&target.connector];
    if app["tools"].is_null() {
        app["tools"] = json!({});
    }
    if !app["tools"].is_object() {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    if app["tools"][NOTION_TOOL].is_null() {
        app["tools"][NOTION_TOOL] = json!({});
    }
    if !app["tools"][NOTION_TOOL].is_object() {
        return Err(Error::code(409, "policy_configuration_conflict"));
    }
    app["tools"][NOTION_TOOL]["approval_mode"] = json!("prompt");
    Ok(json!({"apps":apps,"features.tool_call_mcp_elicitation":false}))
}
pub fn base_overrides() -> Value {
    json!({"features.tool_call_mcp_elicitation":false})
}
/// Safety restriction only, not semantic approval. A well-formed different
/// command is not made turn-eligible here.
pub fn guard_decision(server: &str, tool: &str, args: &Value) -> &'static str {
    if server != "codex_apps" || tool != NOTION_TOOL {
        return "not_blocked";
    }
    let Some(args) = args.as_object() else {
        return "unavailable";
    };
    let command = args.get("command").and_then(Value::as_str);
    if command == Some("update_content") {
        return "blocked";
    }
    if args
        .get("page_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || args.contains_key("content_updates")
        || args.keys().any(|k| {
            ![
                "page_id",
                "command",
                "properties",
                "new_str",
                "content",
                "position",
                "allow_deleting_content",
                "template_id",
                "verification_status",
                "verification_expiry_days",
                "icon",
                "cover",
                "is_skill",
                "allow_async",
            ]
            .contains(&k.as_str())
        })
        || [
            "new_str",
            "content",
            "template_id",
            "verification_status",
            "icon",
            "cover",
        ]
        .iter()
        .any(|k| args.get(*k).is_some_and(|v| !v.is_string()))
        || ["allow_deleting_content", "is_skill", "allow_async"]
            .iter()
            .any(|k| args.get(*k).is_some_and(|v| !v.is_boolean()))
        || args.get("properties").is_some_and(|v| !v.is_object())
        || args.get("position").is_some_and(|v| {
            v.as_object().is_none_or(|o| {
                o.len() != 1
                    || !matches!(o.get("type").and_then(Value::as_str), Some("start" | "end"))
            })
        })
        || args.get("verification_expiry_days").is_some_and(|v| {
            v.as_u64()
                .is_none_or(|n| !(1..=9_007_199_254_740_991).contains(&n))
        })
    {
        return "unavailable";
    }
    let valid = match command {
        Some("update_properties") => args.contains_key("properties"),
        Some("replace_content") => args.contains_key("new_str"),
        Some("insert_content") => args.contains_key("content"),
        Some("apply_template") => args
            .get("template_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        Some("update_verification") => matches!(
            args.get("verification_status").and_then(Value::as_str),
            Some("verified" | "unverified")
        ),
        _ => false,
    };
    if valid { "not_blocked" } else { "unavailable" }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> Target {
        Target {
            connector: "app_test".into(),
            link: Some("link_test".into()),
            definition: "test".into(),
        }
    }
    #[test]
    fn overrides_preserve_disabled_state_and_other_tools() {
        let input = json!({"approvals_reviewer":"user","apps":{"app_test":{"enabled":false,"default_tools_approval_mode":"approve","tools":{"notion.notion-update-page":{"enabled":false,"approval_mode":"approve"},"other":{"approval_mode":"approve"}},"links":{"link_test":{"approvals_reviewer":"user"}}},"other_app":{"enabled":true}}});
        let out = guard_overrides(&input, &Value::Null, &target()).unwrap();
        let mut expected = input["apps"].clone();
        expected["app_test"]["tools"][NOTION_TOOL]["approval_mode"] = json!("prompt");
        assert_eq!(out["apps"], expected);
        assert_eq!(
            input["apps"]["app_test"]["tools"][NOTION_TOOL]["approval_mode"],
            "approve"
        );
    }
    #[test]
    fn auto_review_and_unobservable_managed_policy_fail_before_execution() {
        for config in [
            json!({"approvals_reviewer":"auto_review"}),
            json!({"apps":{"app_test":{"approvals_reviewer":"auto_review"}}}),
            json!({"apps":{"app_test":{"links":{"link_test":{"approvals_reviewer":"auto_review"}}}}}),
            json!({"apps":{"app_test":{"approvals_reviewer":"auto_review","links":{"link_test":{"approvals_reviewer":null}}}}}),
            json!({"approvals_reviewer":"auto_review","apps":{"_default":{"approvals_reviewer":null},"app_test":{"approvals_reviewer":null,"links":{"link_test":{"approvals_reviewer":null}}}}}),
        ] {
            assert_eq!(
                guard_overrides(&config, &Value::Null, &target())
                    .unwrap_err()
                    .code,
                "policy_configuration_conflict"
            );
        }
        assert!(guard_overrides(&json!({}), &json!({}), &target()).is_err());
        assert_eq!(
            base_overrides(),
            json!({"features.tool_call_mcp_elicitation":false})
        );
    }
    #[test]
    fn guard_rejects_ambiguous_and_incomplete_other_commands() {
        for args in [
            json!({"page_id":"page","command":"replace_content"}),
            json!({"page_id":"page","command":"replace_content","new_str":"text","content_updates":[]}),
            json!({"page_id":"page","command":"replace_content","new_str":"text","future_flag":true}),
            json!({"page_id":"page","command":"insert_content","content":null}),
            json!({"page_id":"page","command":"update_verification","verification_status":"verified","verification_expiry_days":0}),
        ] {
            assert_eq!(
                guard_decision("codex_apps", NOTION_TOOL, &args),
                "unavailable",
                "{args}"
            );
        }
    }
    #[test]
    fn guard_does_not_guess_unavailable_command_or_enable_other_tools() {
        assert_eq!(
            guard_decision(
                "codex_apps",
                NOTION_TOOL,
                &json!({"command":"update_content"})
            ),
            "blocked"
        );
        assert_eq!(
            guard_decision("codex_apps", NOTION_TOOL, &json!({})),
            "unavailable"
        );
        assert_eq!(
            guard_decision(
                "codex_apps",
                NOTION_TOOL,
                &json!({"command":"future_unknown"})
            ),
            "unavailable"
        );
        assert_eq!(
            guard_decision(
                "codex_apps",
                NOTION_TOOL,
                &json!({"command":"replace_content","page_id":"page","new_str":"text"})
            ),
            "not_blocked"
        );
        assert_eq!(
            guard_decision("other", "read", &json!({"command":"update_content"})),
            "not_blocked"
        );
    }
}
