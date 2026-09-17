//! Definition-bound semantics. Neither privacy nor successful catalog retrieval
//! constitutes a semantic review of an arbitrary tool.
use super::{approval_config, approval_privacy, catalog::Status};
use serde_json::{Value, json};

pub const FIND: &str = "8349478abf2e116251c070bc40a4320a403d970b708f27db3bc856b5b2a2f221";
pub const NAVIGATE: &str = "4bd2f88138e75bb639e57c781bacc3465723eff08879f84976b76d21a4a66295";
pub const TABS: &str = "4668dd615d73617235e242931431a896d10084cf112d655d5fac8bd59a3c21c6";

pub struct Evaluation {
    pub assessment: Value,
    pub tool_policy: Value,
    pub definition_generation: Value,
    pub description: Option<&'static str>,
}
struct Known {
    description: &'static str,
    effect: &'static str,
    operation: &'static str,
}
fn expected(tool: &str) -> Option<&'static str> {
    match tool {
        "browser_find" => Some(FIND),
        "browser_navigate" => Some(NAVIGATE),
        "browser_tabs" => Some(TABS),
        _ => None,
    }
}
fn known(tool: &str, args: &Value) -> Option<Known> {
    let args = args.as_object()?;
    match tool {
        "browser_find"
            if args.len() == 1
                && args.iter().all(|(k, v)| {
                    ["text", "regex"].contains(&k.as_str())
                        && v.as_str().is_some_and(|s| !s.is_empty())
                }) =>
        {
            Some(Known {
                description: "現在のページ内を検索します",
                effect: "read",
                operation: "find",
            })
        }
        "browser_navigate"
            if args.len() == 1
                && args.get("url").and_then(Value::as_str).is_some_and(|s| {
                    reqwest::Url::parse(s).is_ok_and(|u| {
                        matches!(u.scheme(), "http" | "https") && u.host_str().is_some()
                    })
                }) =>
        {
            Some(Known {
                description: "指定したURLへ移動します",
                effect: "session_change",
                operation: "navigate",
            })
        }
        "browser_tabs" if args.len() == 1 && args.get("action") == Some(&json!("list")) => {
            Some(Known {
                description: "ブラウザーのタブ一覧を取得します",
                effect: "read",
                operation: "list",
            })
        }
        _ => None,
    }
}
pub fn evaluate(
    response: &Value,
    op: &Value,
    catalog: &Status,
    config_generation: &str,
) -> Evaluation {
    let selection = &response["approval_policy"]["selection"];
    let server_name = op["server"].as_str().unwrap_or("");
    let tool = op["tool"].as_str().unwrap_or("");
    let mut definition = None;
    let mut epoch = None;
    let mut reason = "catalog_unavailable";
    if let Status::Ready {
        snapshot,
        epoch: current,
    } = catalog
        && let Some(server) = snapshot.servers.get(server_name)
        && server.available().is_ok()
    {
        definition = server.tools.get(tool);
        epoch = Some(current);
        reason = "tool_not_evaluated";
    }
    let mut reviewed = None;
    if server_name == "playwright"
        && let Some(expected) = expected(tool)
        && let Some(actual) = definition
    {
        if actual == expected {
            reviewed = known(tool, &op["arguments"]);
        } else {
            reason = "definition_changed";
        }
    }
    let generation = definition
        .zip(epoch)
        .map(|(definition, epoch)| json!(crate::control::fingerprint(&json!([definition, epoch]))))
        .unwrap_or(Value::Null);
    let assessment = if let Some(ref known) = reviewed {
        json!({"status":"evaluated","reason":null,"effects":[known.effect]})
    } else {
        json!({"status":if reason=="catalog_unavailable"{"unavailable"}else{"unreviewed"},
            "reason":if selection.is_null() && reason=="tool_not_evaluated"{"policy_not_selected"}else{reason},"effects":null})
    };
    let description = reviewed.as_ref().map(|known| known.description);
    let mut tool_policy = Value::Null;
    if !selection.is_null() {
        let id = selection["id"].as_str().unwrap_or("");
        let mut decision = "not_blocked";
        if selection["version"] != 1
            || !["evaluated-turn", "evaluated-turn-notion-guard"].contains(&id)
            || response["approval_policy"]["state"] != "ready"
            || response["_approval_prepare"]["configuration_confirmed"] != true
            || response["_approval_prepare"]["runtime_id"] != response["_approval_runtime"]
            || !response["_approval_runtime"].is_string()
            || response["_approval_prepare"]["config_generation"] != config_generation
            || !response["approval_policy"]["generation"].is_string()
        {
            decision = "unavailable";
        } else if id == "evaluated-turn-notion-guard"
            && server_name == "codex_apps"
            && tool == approval_config::NOTION_TOOL
        {
            let proof = &response["_approval_prepare"]["guard_definition"];
            decision = if definition.is_none()
                || !proof.is_string()
                || definition.is_some_and(|d| proof != d.as_str())
            {
                "unavailable"
            } else {
                approval_config::guard_decision(server_name, tool, &op["arguments"])
            };
        }
        let credential_free = approval_privacy::credential_free(&op["arguments"]);
        let eligible = decision == "not_blocked" && reviewed.is_some() && credential_free;
        let ineligible = match decision {
            "blocked" => Some("policy_denied"),
            "unavailable" => Some("policy_check_unavailable"),
            _ if reviewed.is_none() => Some(if reason == "definition_changed" {
                "config_changed"
            } else {
                reason
            }),
            _ if !credential_free => Some("always_confirm_effect"),
            _ => None,
        };
        tool_policy = json!({"policy_id":selection["id"],"version":selection["version"],
            "policy_generation":response["approval_policy"]["generation"],
            "definition_generation":if reviewed.is_some(){generation.clone()}else{Value::Null},
            "effects":assessment["effects"],"turn_eligible":eligible,"reason":ineligible,
            "grant_scope":if eligible{Some("turn_tool")}else{None},
            "eligible_operations":reviewed.as_ref().map(|k|json!([k.operation])).unwrap_or_else(||json!([])),
            "always_confirm_operations":[],"decision":decision});
    }
    Evaluation {
        assessment,
        tool_policy,
        definition_generation: if reviewed.is_some() {
            generation
        } else {
            Value::Null
        },
        description,
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog::{Server, Snapshot};
    use super::*;
    use std::{collections::BTreeMap, sync::Arc};
    fn catalog(hash: &str) -> Status {
        Status::Ready {
            epoch: "epoch".into(),
            snapshot: Arc::new(Snapshot {
                guard_target: None,
                servers: BTreeMap::from([(
                    "playwright".into(),
                    Server {
                        runtime_status: "connected".into(),
                        auth_status: "unsupported".into(),
                        tools: BTreeMap::from([("browser_find".into(), hash.into())]),
                    },
                )]),
            }),
        }
    }
    fn response(id: &str) -> Value {
        json!({"_approval_runtime":"runtime","_approval_prepare":{"configuration_confirmed":true,
            "runtime_id":"runtime","config_generation":"config"},"approval_policy":{"selection":{"id":id,"version":1},"state":"ready","generation":"generation"}})
    }
    #[test]
    fn definitions_arguments_and_credentials_are_independent_gates() {
        let r = response("evaluated-turn");
        let mut op = json!({"server":"playwright","tool":"browser_find","arguments":{"text":"alice@example.com"}});
        let result = evaluate(&r, &op, &catalog(FIND), "config");
        assert_eq!(result.tool_policy["turn_eligible"], true);
        assert_eq!(result.assessment["status"], "evaluated");
        op["arguments"] = json!({"text":"password: secret"});
        assert_eq!(
            evaluate(&r, &op, &catalog(FIND), "config").tool_policy["turn_eligible"],
            false
        );
        op["arguments"] = json!({"text":"normal","unexpected":true});
        assert_eq!(
            evaluate(&r, &op, &catalog(FIND), "config").assessment["status"],
            "unreviewed"
        );
        assert_eq!(
            evaluate(&r, &op, &catalog("changed"), "config").assessment["reason"],
            "definition_changed"
        );
    }
    #[test]
    fn unknown_meaning_does_not_block_single_approval_but_lost_preparation_does() {
        let mut r = response("evaluated-turn");
        let op = json!({"server":"other","tool":"future","arguments":{"a":1}});
        let result = evaluate(&r, &op, &Status::Failed("catalog_timeout"), "config");
        assert_eq!(result.assessment["reason"], "catalog_unavailable");
        assert_eq!(result.tool_policy["decision"], "not_blocked");
        assert_eq!(result.tool_policy["turn_eligible"], false);
        r["_approval_prepare"]["runtime_id"] = json!("different");
        assert_eq!(
            evaluate(&r, &op, &catalog(FIND), "config").tool_policy["decision"],
            "unavailable"
        );
        r["approval_policy"]["selection"] = Value::Null;
        assert!(
            evaluate(&r, &op, &catalog(FIND), "config")
                .tool_policy
                .is_null()
        );
    }
    #[test]
    fn notion_restriction_is_definition_bound_and_policy_local() {
        let op = json!({"server":"codex_apps","tool":approval_config::NOTION_TOOL,"arguments":{"command":"update_content"}});
        let snapshot = Status::Ready {
            epoch: "epoch".into(),
            snapshot: Arc::new(Snapshot {
                guard_target: None,
                servers: BTreeMap::from([(
                    "codex_apps".into(),
                    Server {
                        runtime_status: "connected".into(),
                        auth_status: "oAuth".into(),
                        tools: BTreeMap::from([(
                            approval_config::NOTION_TOOL.into(),
                            "def".into(),
                        )]),
                    },
                )]),
            }),
        };
        let mut r = response("evaluated-turn-notion-guard");
        assert_eq!(
            evaluate(&r, &op, &snapshot, "config").tool_policy["decision"],
            "unavailable"
        );
        r["_approval_prepare"]["guard_definition"] = json!("def");
        assert_eq!(
            evaluate(&r, &op, &snapshot, "config").tool_policy["decision"],
            "blocked"
        );
        assert_eq!(
            evaluate(&r, &op, &snapshot, "changed").tool_policy["decision"],
            "unavailable"
        );
        r["approval_policy"]["selection"]["id"] = json!("evaluated-turn");
        assert_eq!(
            evaluate(&r, &op, &snapshot, "config").tool_policy["decision"],
            "not_blocked"
        );
    }
}
