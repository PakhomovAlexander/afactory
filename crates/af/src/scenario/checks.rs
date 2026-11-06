//! Bounded exact-head outcomes from the installed production GitHub observer.
use super::contract::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Outcome {
    schema: String,
    subject: String,
    revision: String,
    pub state: String,
    checks: Vec<Check>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Check {
    identity: Identity,
    id: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    suite_id: Option<u64>,
    outcome: String,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Identity {
    Check { name: String, app_id: u64 },
    Status { context: String, creator_id: u64 },
}
impl Outcome {
    pub fn parse(o: &Observation) -> Result<Self> {
        o.validate()?;
        require(
            o.condition == "checks_completed",
            "Expected completed-check observation",
        )?;
        let value: Self = serde_json::from_str(&o.evidence).map_err(|e| e.to_string())?;
        require(
            value.schema == "af/github-check-outcome@1"
                && value.subject == o.subject
                && value.revision == o.revision
                && value.revision.len() == 40
                && value.revision.bytes().all(|b| b.is_ascii_hexdigit())
                && value
                    .subject
                    .strip_prefix("github-pr:")
                    .is_some_and(review_core::is_digest)
                && value.checks.len() <= 32,
            "Invalid exact-head check outcome",
        )?;
        let mut identities = std::collections::BTreeSet::new();
        for check in &value.checks {
            let (name, id, suite) = match &check.identity {
                Identity::Check { name, app_id } => (name, *app_id, true),
                Identity::Status {
                    context,
                    creator_id,
                } => (context, *creator_id, false),
            };
            text(name)?;
            require(
                name.len() <= 200
                    && id > 0
                    && id <= review_core::json::SAFE_INTEGER_MAX as u64
                    && check.id > 0
                    && check.id <= review_core::json::SAFE_INTEGER_MAX as u64
                    && (if suite {
                        check.suite_id.is_some_and(|i| {
                            i > 0 && i <= review_core::json::SAFE_INTEGER_MAX as u64
                        })
                    } else {
                        check.suite_id.is_none()
                    })
                    && matches!(check.outcome.as_str(), "pending" | "ready" | "repairable")
                    && identities
                        .insert(serde_json::to_string(&check.identity).map_err(|e| e.to_string())?),
                "Invalid required-check identity or outcome",
            )?;
        }
        require(
            matches!(value.state.as_str(), "pending" | "ready" | "repairable")
                && o.satisfied == (value.state != "pending"),
            "Check completion differs",
        )?;
        if o.satisfied {
            require(
                !value.checks.is_empty()
                    && value.checks.iter().all(|c| c.outcome != "pending")
                    && (value.state == "repairable")
                        == value.checks.iter().any(|c| c.outcome == "repairable"),
                "Incomplete terminal check evidence",
            )?;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn outcome_projection_binds_exact_head_and_refuses_unfinished_or_forged_terminal_evidence() {
        let subject = format!("github-pr:{}", digest(b"publication"));
        let mut packet = json!({"schema":"af/github-check-outcome@1","subject":subject,
            "revision":"b".repeat(40),"state":"repairable","checks":[{
                "identity":{"kind":"check","name":"CI","app_id":4},
                "id":50,"suite_id":40,"outcome":"repairable"}]});
        crate::scenario::tests::valid("github-check-outcome-v1.json", &packet);
        let observation = |p: &serde_json::Value| Observation {
            subject: subject.clone(),
            revision: "b".repeat(40),
            condition: "checks_completed".into(),
            satisfied: true,
            evidence: serde_json::to_string(p).unwrap(),
        };
        assert_eq!(
            Projection::ChecksState
                .apply(Datum::Observation(observation(&packet)))
                .unwrap(),
            Datum::Text("repairable".into())
        );
        for field in ["revision", "subject", "schema", "state"] {
            let mut forged = packet.clone();
            forged[field] = json!("unrelated");
            assert!(Outcome::parse(&observation(&forged)).is_err());
        }
        packet["checks"][0]["outcome"] = json!("pending");
        assert!(Outcome::parse(&observation(&packet)).is_err());
        packet["checks"] = json!([]);
        assert!(Outcome::parse(&observation(&packet)).is_err());
        let mut pending = observation(&packet);
        packet["state"] = json!("pending");
        pending.evidence = packet.to_string();
        pending.satisfied = false;
        assert_eq!(Outcome::parse(&pending).unwrap().state, "pending");
    }
}
