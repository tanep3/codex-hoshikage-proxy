//! Explicit per-response policy binding and durable preparation state machine.
//! Callers persist transitions with stop/turn intent under the store transaction.
use super::{Error, Result, id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROFILE: &str = "source-conversation-v3";
pub const PREPARATION_MS: u64 = 60_000;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub id: String,
    pub version: u64,
}
impl Selection {
    pub fn parse(value: &Value, enabled: bool) -> Result<Option<Self>> {
        if value.is_null() {
            return Ok(None);
        }
        let choice: Self = serde_json::from_value(value.clone())
            .map_err(|_| Error::code(400, "invalid_approval_policy"))?;
        if choice.version == 0
            || choice.id.is_empty()
            || choice.id.len() > 128
            || !choice.id.is_ascii()
        {
            return Err(Error::code(400, "invalid_approval_policy"));
        }
        if choice.version != 1
            || !["evaluated-turn", "evaluated-turn-notion-guard"].contains(&choice.id.as_str())
        {
            return Err(Error::code(422, "approval_policy_unknown"));
        }
        if !enabled {
            return Err(Error::code(503, "approval_policy_unavailable"));
        }
        Ok(Some(choice))
    }
    fn restrictions(&self) -> Value {
        if self.id == "evaluated-turn-notion-guard" {
            json!(["deny-unbound-notion-partial-replacement"])
        } else {
            json!([])
        }
    }
    fn overrides(&self) -> Value {
        if self.id == "evaluated-turn-notion-guard" {
            json!(["この実行のNotionページ更新を毎回Proxyの検査へ通します"])
        } else {
            json!([])
        }
    }
}
#[derive(Clone, Debug)]
pub struct Clock {
    pub utc_ms: u64,
    pub boot_ms: u64,
    pub boot_id: String,
}
impl Clock {
    pub fn read() -> Result<Self> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 || ts.tv_sec < 0 {
            return Err(Error::code(503, "policy_clock_unavailable"));
        }
        Ok(Self {
            utc_ms: super::now(),
            boot_ms: (ts.tv_sec as u64).saturating_mul(1000) + (ts.tv_nsec as u64) / 1_000_000,
            boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .into(),
        })
    }
}
fn timestamp(ms: u64) -> Result<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map_err(|_| Error::code(503, "policy_clock_unavailable"))?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| Error::code(503, "policy_clock_unavailable"))
}
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    ResumePreparation,
    ReconcileConfiguration,
    ReconcileTurn,
    Terminal,
}
pub fn initial(selection: Option<Selection>, clock: &Clock) -> Result<(Value, Value)> {
    let deadline = clock
        .utc_ms
        .checked_add(PREPARATION_MS)
        .ok_or_else(|| Error::code(503, "policy_clock_unavailable"))?;
    let public = json!({"selection":selection,"binding_id":id("binding"),"generation":null,"state":"preparing","reason":null,
        "restrictions":selection.as_ref().map(Selection::restrictions).unwrap_or_else(||json!([])),
        "upstream_overrides":selection.as_ref().map(Selection::overrides).unwrap_or_else(||json!([])),
        "preparation":{"started_at":timestamp(clock.utc_ms)?,"deadline_at":timestamp(deadline)?,"recovery_state":"none","turn_start_status":"not_sent","configuration_isolation":"not_required"}});
    let private = json!({"utc_start":clock.utc_ms,"utc_deadline":deadline,"boot_id":clock.boot_id,
        "boot_start":clock.boot_ms,"boot_deadline":clock.boot_ms.saturating_add(PREPARATION_MS),
        "configuration_intent":false,"configuration_confirmed":false,"runtime_id":null,"proof":null});
    Ok((public, private))
}
pub fn expired(private: &Value, clock: &Clock) -> bool {
    private["boot_id"] != clock.boot_id
        || clock.utc_ms < private["utc_start"].as_u64().unwrap_or(u64::MAX)
        || clock.utc_ms >= private["utc_deadline"].as_u64().unwrap_or(0)
        || clock.boot_ms < private["boot_start"].as_u64().unwrap_or(u64::MAX)
        || clock.boot_ms >= private["boot_deadline"].as_u64().unwrap_or(0)
}
pub fn configuration_intent(
    public: &mut Value,
    private: &mut Value,
    runtime: &str,
    clock: &Clock,
) -> Result<()> {
    if public["state"] != "preparing" || private["configuration_intent"] == true {
        return Err(Error::code(409, "approval_policy_not_ready"));
    }
    if expired(private, clock) {
        return Err(Error::code(409, "policy_setup_timeout"));
    }
    private["configuration_intent"] = json!(true);
    private["runtime_id"] = json!(runtime);
    public["preparation"]["configuration_isolation"] = json!("pending");
    Ok(())
}
pub fn ready(public: &mut Value, private: &mut Value, proof: &str, clock: &Clock) -> Result<()> {
    if public["state"] != "preparing" || private["configuration_intent"] != true || proof.is_empty()
    {
        return Err(Error::code(409, "approval_policy_not_ready"));
    }
    if expired(private, clock) {
        return Err(Error::code(409, "policy_setup_timeout"));
    }
    private["configuration_confirmed"] = json!(true);
    private["proof"] = json!(proof);
    public["state"] = json!("ready");
    public["reason"] = Value::Null;
    public["generation"] = if public["selection"].is_null() {
        Value::Null
    } else {
        json!(id("policygen"))
    };
    public["preparation"]["configuration_isolation"] = json!("confirmed");
    Ok(())
}
pub fn turn_intent(
    public: &mut Value,
    private: &Value,
    stopped: bool,
    clock: &Clock,
) -> Result<()> {
    if stopped {
        return Err(Error::code(409, "execution_cancelled"));
    }
    if public["state"] != "ready"
        || private["configuration_confirmed"] != true
        || public["preparation"]["turn_start_status"] != "not_sent"
    {
        return Err(Error::code(409, "approval_policy_not_ready"));
    }
    if expired(private, clock) {
        return Err(Error::code(409, "policy_setup_timeout"));
    }
    public["preparation"]["turn_start_status"] = json!("intent_recorded");
    Ok(())
}
pub fn started(public: &mut Value) -> Result<()> {
    if public["preparation"]["turn_start_status"] != "intent_recorded" {
        return Err(Error::code(409, "approval_policy_not_ready"));
    }
    public["preparation"]["turn_start_status"] = json!("confirmed");
    Ok(())
}
/// Only explicit, independently verified isolation may release a configuration
/// RPC of unknown outcome. No transition here sends or retries upstream input.
pub fn failure(public: &mut Value, reason: &str, isolated: bool) -> Result<()> {
    if public["preparation"]["turn_start_status"] != "not_sent" {
        return Err(Error::code(409, "execution_unknown"));
    }
    if ![
        "policy_setup_failed",
        "policy_setup_unknown",
        "policy_configuration_conflict",
        "policy_setup_timeout",
    ]
    .contains(&reason)
    {
        return Err(Error::code(503, "store_corrupt"));
    }
    if public["state"] != "closed" && public["state"] != "failed" {
        public["state"] = json!("failed");
        public["reason"] = json!(reason);
    }
    public["preparation"]["configuration_isolation"] =
        json!(if isolated { "confirmed" } else { "pending" });
    Ok(())
}
pub fn stop(public: &mut Value, private: &Value) {
    if public["preparation"]["turn_start_status"] != "not_sent" {
        return;
    }
    if public["state"] != "failed" {
        public["state"] = json!("closed");
        public["reason"] = json!("policy_setup_cancelled");
    }
    let known =
        private["configuration_intent"] != true || private["configuration_confirmed"] == true;
    public["preparation"]["configuration_isolation"] =
        json!(if known { "confirmed" } else { "pending" });
}
pub fn stop_response(response: &mut Value) -> Result<bool> {
    if response["approval_presentation"]["profile"] != PROFILE
        || matches!(
            response["phase"].as_str(),
            Some("finished" | "cancelled" | "rejected")
        )
    {
        return Ok(false);
    }
    if !response["_approval_prepare"].is_object() || !response["approval_policy"].is_object() {
        return Err(Error::code(503, "store_corrupt"));
    }
    if response["approval_policy"]["preparation"]["turn_start_status"] != "not_sent" {
        return Ok(false);
    }
    let mut policy = response["approval_policy"].clone();
    stop(&mut policy, &response["_approval_prepare"]);
    response["stop_requested"] = json!(true);
    response["execution_status"] = json!("not_started");
    response["dispatch_eligible"] = json!(false);
    if policy["preparation"]["configuration_isolation"] == "confirmed" {
        response["phase"] = json!(if policy["state"] == "failed" {
            "rejected"
        } else {
            "cancelled"
        });
        response["hold_state"] = json!("released");
        response["input"] = Value::Null;
        response["output"] = json!({"state":"unavailable"});
    } else {
        response["phase"] = json!("unknown");
    }
    response["approval_policy"] = policy;
    Ok(true)
}
pub fn recover(public: &mut Value, private: &mut Value, clock: &Clock) -> Recovery {
    if public["preparation"]["turn_start_status"] != "not_sent" {
        public["preparation"]["recovery_state"] = json!("reconciling");
        return Recovery::ReconcileTurn;
    }
    if public["state"] == "failed" || public["state"] == "closed" {
        return if public["preparation"]["configuration_isolation"] == "pending" {
            Recovery::ReconcileConfiguration
        } else {
            Recovery::Terminal
        };
    }
    public["preparation"]["recovery_state"] = json!("reconciling");
    if public["state"] == "ready" && private["configuration_confirmed"] == true {
        private["configuration_intent"] = json!(false);
        private["configuration_confirmed"] = json!(false);
        private["proof"] = Value::Null;
        public["preparation"]["configuration_isolation"] = json!("not_required");
    }
    if private["configuration_intent"] == true {
        // Readiness cannot survive loss of the runtime proof. Never retry the
        // config write to manufacture proof after a lost acknowledgement.
        public["state"] = json!("failed");
        public["reason"] = json!("policy_setup_unknown");
        public["preparation"]["configuration_isolation"] = json!("pending");
        return Recovery::ReconcileConfiguration;
    }
    if expired(private, clock) {
        public["state"] = json!("failed");
        public["reason"] = json!("policy_setup_timeout");
        public["preparation"]["configuration_isolation"] = json!("confirmed");
        Recovery::Terminal
    } else {
        public["state"] = json!("preparing");
        public["generation"] = Value::Null;
        Recovery::ResumePreparation
    }
}

/// Recover only durable v0.6 preparation records. Legacy executions retain their
/// original recovery rules. Never create an execution or a new acceptance lease.
pub fn recover_response(response: &mut Value, clock: &Clock) -> Result<bool> {
    if response["approval_presentation"]["profile"] != PROFILE {
        return Ok(false);
    }
    if !response["_approval_prepare"].is_object() || !response["approval_policy"].is_object() {
        return Err(Error::code(503, "store_corrupt"));
    }
    if matches!(
        response["phase"].as_str(),
        Some("finished" | "cancelled" | "rejected")
    ) {
        return Ok(true);
    }
    let mut policy = response["approval_policy"].clone();
    let mut private = response["_approval_prepare"].clone();
    if response["stop_requested"] == true {
        stop(&mut policy, &private);
    }
    match recover(&mut policy, &mut private, clock) {
        Recovery::ResumePreparation => {
            response["phase"] = json!("accepted");
            response["execution_status"] = json!("not_started");
        }
        Recovery::ReconcileConfiguration => {
            response["phase"] = json!("unknown");
            response["execution_status"] = json!("not_started");
            response["stop_requested"] = json!(true);
        }
        Recovery::ReconcileTurn => {
            response["phase"] = json!("unknown");
            response["execution_status"] = json!("unknown");
            response["stop_requested"] = json!(true);
        }
        Recovery::Terminal => {
            response["phase"] = json!(if policy["state"] == "closed" {
                "cancelled"
            } else {
                "rejected"
            });
            response["execution_status"] = json!("not_started");
            response["hold_state"] = json!("released");
            response["input"] = Value::Null;
            response["output"] = json!({"state":"unavailable"});
        }
    }
    if response["phase"] == "unknown" {
        response["hold_revision"] = json!(response["hold_revision"].as_u64().unwrap_or(0) + 1);
        if response["interrupt_delivery"] == "dispatching" {
            response["interrupt_delivery"] = json!("unknown");
        }
    }
    response["dispatch_eligible"] = json!(response["phase"] == "accepted");
    response["approval_policy"] = policy;
    response["_approval_prepare"] = private;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clock() -> Clock {
        Clock {
            utc_ms: 1_800_000_000_000,
            boot_ms: 1000,
            boot_id: "boot1".into(),
        }
    }
    #[test]
    fn selection_is_explicit_strict_and_never_inherited() {
        assert!(Selection::parse(&Value::Null, false).unwrap().is_none());
        assert_eq!(
            Selection::parse(
                &json!({"id":"evaluated-turn","version":1,"extra":true}),
                true
            )
            .unwrap_err()
            .code,
            "invalid_approval_policy"
        );
        assert_eq!(
            Selection::parse(&json!({"id":"evaluated-turn","version":2}), true)
                .unwrap_err()
                .code,
            "approval_policy_unknown"
        );
        assert_eq!(
            Selection::parse(&json!({"id":"evaluated-turn","version":1}), false)
                .unwrap_err()
                .code,
            "approval_policy_unavailable"
        );
    }
    #[test]
    fn stop_wins_before_turn_intent_and_cannot_be_undone_by_ready() {
        let c = clock();
        let (mut p, mut internal) = initial(None, &c).unwrap();
        configuration_intent(&mut p, &mut internal, "runtime", &c).unwrap();
        stop(&mut p, &internal);
        assert!(ready(&mut p, &mut internal, "proof", &c).is_err());
        assert_eq!(p["preparation"]["configuration_isolation"], "pending");
        assert!(turn_intent(&mut p, &internal, true, &c).is_err());
    }
    #[test]
    fn deadlines_do_not_extend_across_restart_or_clock_changes() {
        let c = clock();
        let (mut p, mut internal) = initial(None, &c).unwrap();
        let original = p.clone();
        let mut later = c.clone();
        later.boot_ms += PREPARATION_MS;
        assert_eq!(recover(&mut p, &mut internal, &later), Recovery::Terminal);
        assert_eq!(
            p["preparation"]["deadline_at"],
            original["preparation"]["deadline_at"]
        );
        assert_eq!(p["binding_id"], original["binding_id"]);
        later = c.clone();
        later.utc_ms -= 1;
        assert!(expired(&internal, &later));
        later = c;
        later.boot_id = "newboot".into();
        assert!(expired(&internal, &later));
    }
    #[test]
    fn configuration_unknown_is_not_turn_unknown_and_never_reexecutes() {
        let c = clock();
        let (mut p, mut internal) = initial(None, &c).unwrap();
        configuration_intent(&mut p, &mut internal, "runtime", &c).unwrap();
        assert_eq!(
            recover(&mut p, &mut internal, &c),
            Recovery::ReconcileConfiguration
        );
        assert_eq!(p["preparation"]["turn_start_status"], "not_sent");
        stop(&mut p, &internal);
        assert_eq!(p["reason"], "policy_setup_unknown");
        failure(&mut p, "policy_setup_unknown", true).unwrap();
        assert_eq!(recover(&mut p, &mut internal, &c), Recovery::Terminal);
        assert!(ready(&mut p, &mut internal, "late-success", &c).is_err());
    }
    #[test]
    fn recorded_turn_intent_is_never_reclassified_as_not_started() {
        let c = clock();
        let (mut p, mut internal) = initial(None, &c).unwrap();
        configuration_intent(&mut p, &mut internal, "runtime", &c).unwrap();
        ready(&mut p, &mut internal, "proof", &c).unwrap();
        turn_intent(&mut p, &internal, false, &c).unwrap();
        stop(&mut p, &internal);
        assert_eq!(recover(&mut p, &mut internal, &c), Recovery::ReconcileTurn);
        assert!(failure(&mut p, "policy_setup_timeout", true).is_err());
        started(&mut p).unwrap();
        assert_eq!(p["preparation"]["turn_start_status"], "confirmed");
    }
    #[test]
    fn acknowledged_setup_revalidates_same_binding_without_reusing_proof() {
        let c = clock();
        let (mut p, mut private) = initial(
            Some(Selection {
                id: "evaluated-turn".into(),
                version: 1,
            }),
            &c,
        )
        .unwrap();
        configuration_intent(&mut p, &mut private, "runtime1", &c).unwrap();
        ready(&mut p, &mut private, "proof1", &c).unwrap();
        let binding = p["binding_id"].clone();
        let deadline = p["preparation"]["deadline_at"].clone();
        assert_eq!(
            recover(&mut p, &mut private, &c),
            Recovery::ResumePreparation
        );
        assert_eq!(p["binding_id"], binding);
        assert_eq!(p["preparation"]["deadline_at"], deadline);
        assert!(p["generation"].is_null());
        assert!(private["proof"].is_null());
        assert!(turn_intent(&mut p, &private, false, &c).is_err());
        configuration_intent(&mut p, &mut private, "runtime2", &c).unwrap();
        ready(&mut p, &mut private, "proof2", &c).unwrap();
        turn_intent(&mut p, &private, false, &c).unwrap();
    }
}
