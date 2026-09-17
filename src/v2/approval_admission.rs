//! Versioned admission, canonical idempotency input, and wire reservations.
use super::{
    Error, Result,
    approval_policy::{self, Selection},
    approval_v06,
};
use serde_json::{Value, json};
pub fn canonical(body: &Value) -> Value {
    let mut out = body.clone();
    if out["approval_policy"].is_null()
        && let Some(o) = out.as_object_mut()
    {
        o.remove("approval_policy");
    }
    out
}
pub fn validate(body: &Value, enabled: bool) -> Result<Option<Selection>> {
    if !approval_v06::selected(body) {
        if !body["approval_policy"].is_null() {
            return Err(Error::code(400, "invalid_approval_policy"));
        }
        super::presentations::validate_request(body)?;
        return Ok(None);
    }
    let p = &body["approval_presentation"];
    if p.as_object().is_none_or(|o| o.len() != 2)
        || p["mode"] != "source_conversation"
        || !body["approval_context"].is_object()
        || !body["interaction_capabilities"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "mcp_form"))
    {
        return Err(Error::code(400, "invalid_approval_presentation"));
    }
    super::mcp_grants::validate_context(body.get("approval_context"))?;
    Selection::parse(&body["approval_policy"], enabled)
}
pub fn bounded(value: Value, max: usize) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() > max {
        return Err(Error::code(503, "approval_response_too_large"));
    }
    Ok(value)
}
pub fn public_response(mut value: Value) -> Value {
    if let Some(o) = value.as_object_mut() {
        o.remove("input");
        o.remove("request_key");
        o.retain(|key, _| !key.starts_with('_'));
    }
    value
}
pub fn reserve_response(value: &Value) -> Result<()> {
    if !value["approval_policy"].is_null() {
        bounded(value["approval_policy"].clone(), 4096)
            .map_err(|_| Error::code(503, "approval_metadata_capacity"))?;
    }
    let mut v = public_response(value.clone());
    // Reserve later policy fields, thread/turn IDs, generated images and output metadata.
    v["_future_reservation"] = json!("x".repeat(65536));
    if serde_json::to_vec(&v)?.len() > 196608 {
        return Err(Error::code(503, "approval_metadata_capacity"));
    }
    Ok(())
}
pub fn capability(enabled: bool) -> Result<Value> {
    let policies = json!([
        {"id":"evaluated-turn","version":1,"enabled":enabled,"label":"評価済み通常操作の依頼中許可","restrictions":[],"upstream_overrides":[]},
        {"id":"evaluated-turn-notion-guard","version":1,"enabled":enabled,"label":"評価済み通常操作の依頼中許可とNotion部分置換制限","restrictions":["deny-unbound-notion-partial-replacement"],"upstream_overrides":["この実行のNotionページ更新を毎回Proxyの検査へ通します"]}
    ]);
    for p in policies.as_array().unwrap() {
        bounded(p.clone(), 2048)?;
    }
    bounded(
        json!({"enabled":true,"profile":approval_policy::PROFILE,"renderers":["raw-arguments-v1","evaluated-operation-v1"],
        "max_response_bytes":65536,"max_argument_bytes":262144,"max_display_text_utf16_units":4800,"max_display_fields":24,
        "max_presentations_per_interaction_per_audience":4,"max_get_wait_ms":250,"retry_after_ms":2000,
        "private_details":true,"max_private_pages":64,"max_private_display_bytes":262144,"policies":policies,
        "policy_preparation_timeout_ms":60000,"response_limits":{"capabilities":1048576,"capability_extension":65536,
        "response":262144,"presentation":65536,"operation_details":65536,"interaction":262144,"interaction_list":67108864,
        "grant_list":1048576,"control":65536,"max_interactions":256,"max_pending_interactions":16,"max_grants":16}}),
        65536,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contract_examples_have_exact_capability_fields_and_null_is_omission() {
        let examples: Value = serde_json::from_str(include_str!(
            "../../docs/mcp-approval/api-v06-examples.json"
        ))
        .unwrap();
        assert_eq!(
            capability(true).unwrap(),
            examples["capability"]["mcp_approval_v06"]
        );
        let input = examples["request_no_policy"].clone();
        assert!(validate(&input, false).unwrap().is_none());
        assert_eq!(canonical(&input), canonical(&canonical(&input)));
        assert_eq!(
            validate(&examples["request_guard"], false)
                .unwrap_err()
                .code,
            "approval_policy_unavailable"
        );
    }
}
