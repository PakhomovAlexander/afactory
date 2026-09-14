//! Versioned captured admission costs. Existing authority keeps its original fixed allowance.
use super::{Cas, RunAuthority, TaskCatalog};
use review_core::json::SAFE_INTEGER_MAX;
use review_graph::task::OperatorAttemptCost;

fn validate(cost: &OperatorAttemptCost) -> Result<(), String> {
    if cost.tokens == 0
        || cost.wall_ms == 0
        || cost.tokens > SAFE_INTEGER_MAX as u64
        || cost.wall_ms > SAFE_INTEGER_MAX as u64
    {
        return Err("Provider admission requires positive bounded tokens and wall_ms".into());
    }
    Ok(())
}

pub(super) fn catalog_cost(catalog: &TaskCatalog) -> Result<Option<OperatorAttemptCost>, String> {
    match (catalog.schema.as_str(), &catalog.provider_admission) {
        ("af.task-catalog/1", None) => Ok(None),
        ("af.task-catalog/2", Some(cost)) => {
            validate(cost)?;
            Ok(Some(cost.clone()))
        }
        _ => {
            Err("Task catalog V1 forbids provider_admission; V2 requires its explicit cost".into())
        }
    }
}

pub(super) fn restore_cost(
    cas: &Cas,
    authority: &RunAuthority,
) -> Result<OperatorAttemptCost, String> {
    match (authority.schema.as_str(), &authority.provider_admission) {
        // Do not reparse or reinterpret frozen V1 catalog bytes during restoration.
        ("af.task-run-authority/1", None) => Ok(OperatorAttemptCost {
            tokens: 4096,
            wall_ms: 45000,
        }),
        ("af.task-run-authority/2", Some(cost)) => {
            validate(cost)?;
            let bytes = cas.get(&authority.catalog_id).map_err(|e| e.to_string())?;
            let catalog: TaskCatalog = super::parse(std::path::Path::new("catalog.toml"), &bytes)?;
            if catalog_cost(&catalog)?.as_ref() != Some(cost) {
                return Err("Provider admission cost differs from its captured V2 catalog".into());
            }
            Ok(cost.clone())
        }
        _ => Err(
            "Task authority V1 forbids provider_admission; V2 requires its captured cost".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const V1_CATALOG: &str = r#"{"schema":"af.task-catalog/1","no_match":"refuse","packages":{},"independence":{"command_workers_by_package":true,"distinct_principals":true,"distinct_providers":false,"distinct_models":false},"providers":{}}"#;
    const V1_AUTHORITY: &str = r#"{"schema":"af.task-run-authority/1","engine_id":"engine","catalog_id":"catalog","no_match":"refuse","packages":{},"independence":{"command_workers_by_package":true,"distinct_principals":true,"distinct_providers":false,"distinct_models":false},"providers":{}}"#;

    fn catalog(value: Value) -> Result<Option<OperatorAttemptCost>, String> {
        let catalog = serde_json::from_value(value).map_err(|e| e.to_string())?;
        catalog_cost(&catalog)
    }

    #[test]
    fn v1_bytes_and_cost_stay_frozen_and_v2_fields_cannot_override_them() {
        let parsed: TaskCatalog = serde_json::from_str(V1_CATALOG).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), V1_CATALOG);
        assert_eq!(catalog_cost(&parsed).unwrap(), None);
        let mut authority: RunAuthority = serde_json::from_str(V1_AUTHORITY).unwrap();
        assert_eq!(serde_json::to_string(&authority).unwrap(), V1_AUTHORITY);
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path()).unwrap();
        // Frozen restoration verified the blob identity, without parsing its syntax again.
        authority.catalog_id = cas.put(b"old captured catalog bytes").unwrap();
        cas.verify(&authority.catalog_id).unwrap();
        assert_eq!(
            restore_cost(&cas, &authority).unwrap(),
            OperatorAttemptCost {
                tokens: 4096,
                wall_ms: 45000
            }
        );
        authority.provider_admission = Some(OperatorAttemptCost {
            tokens: 32768,
            wall_ms: 45000,
        });
        assert!(restore_cost(&cas, &authority).is_err());
        let mut altered: Value = serde_json::from_str(V1_CATALOG).unwrap();
        altered["provider_admission"] = json!({"tokens":32768,"wall_ms":45000});
        assert!(catalog(altered).is_err());
    }

    #[test]
    fn v2_requires_bounded_explicit_cost_and_exact_captured_catalog() {
        let mut value: Value = serde_json::from_str(V1_CATALOG).unwrap();
        value["schema"] = json!("af.task-catalog/2");
        assert!(catalog(value.clone()).is_err());
        for cost in [
            Value::Null,
            json!({"tokens":0,"wall_ms":45000}),
            json!({"tokens":32768,"wall_ms":0}),
            json!({"tokens":9007199254740992_u64,"wall_ms":45000}),
            json!({"tokens":32768,"wall_ms":9007199254740992_u64}),
            json!({"tokens":-1,"wall_ms":45000}),
            json!({"tokens":32768}),
            json!({"tokens":32768,"wall_ms":45000,"extra":1}),
        ] {
            value["provider_admission"] = cost;
            assert!(catalog(value.clone()).is_err(), "{value}");
        }
        value["provider_admission"] = json!({"tokens":32768,"wall_ms":45000});
        let cost = catalog(value.clone()).unwrap().unwrap();
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path()).unwrap();
        let mut authority: RunAuthority = serde_json::from_str(V1_AUTHORITY).unwrap();
        authority.schema = "af.task-run-authority/2".into();
        authority.catalog_id = cas
            .put(toml::to_string(&value).unwrap().as_bytes())
            .unwrap();
        assert!(restore_cost(&cas, &authority).is_err());
        authority.provider_admission = Some(cost.clone());
        assert_eq!(restore_cost(&cas, &authority).unwrap(), cost);
        authority.provider_admission.as_mut().unwrap().tokens += 1;
        assert!(
            restore_cost(&cas, &authority)
                .unwrap_err()
                .contains("captured V2 catalog")
        );
        authority.provider_admission = Some(cost);
        authority.catalog_id = cas.put(b"schema = 'af.task-catalog/1'\n").unwrap();
        assert!(restore_cost(&cas, &authority).is_err());
    }
    #[test]
    fn catalog_two_does_not_select_review_generation() {
        let mut value: Value = serde_json::from_str(V1_CATALOG).unwrap();
        value["schema"] = json!("af.task-catalog/2");
        value["provider_admission"] = json!({"tokens":32768,"wall_ms":45000});
        value["review"] = json!({"reviewers":{"correctness":"required"},"gate":"major","clean_rounds":1,"max_rounds":2});
        let old: TaskCatalog = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(old.review.as_ref().unwrap().policy_generation().unwrap(), 1);
        let original_cost = catalog_cost(&old).unwrap();
        value["review"]["generation"] = json!(2);
        let next: TaskCatalog = serde_json::from_value(value).unwrap();
        assert_eq!(
            next.review.as_ref().unwrap().policy_generation().unwrap(),
            2
        );
        assert_eq!(catalog_cost(&next).unwrap(), original_cost);
    }
}
