//! One captured Provider admission cost. An omitted catalog cost means the fixed default.
use super::{Cas, RunAuthority, TaskCatalog};
use review_core::json::SAFE_INTEGER_MAX;
use review_graph::task::OperatorAttemptCost;

pub(super) const TASK_CATALOG_SCHEMA: &str = "af.task-catalog/2";
pub(super) const RUN_AUTHORITY_SCHEMA: &str = "af.task-run-authority/2";

/// The allowance a catalog that declares no explicit Provider admission cost receives.
const DEFAULT_ADMISSION: OperatorAttemptCost = OperatorAttemptCost {
    tokens: 4096,
    wall_ms: 45000,
};

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

pub(super) fn catalog_cost(catalog: &TaskCatalog) -> Result<OperatorAttemptCost, String> {
    if catalog.schema != TASK_CATALOG_SCHEMA {
        return Err(format!(
            "Task catalog requires schema {TASK_CATALOG_SCHEMA}"
        ));
    }
    let Some(cost) = &catalog.provider_admission else {
        return Ok(DEFAULT_ADMISSION);
    };
    validate(cost)?;
    Ok(cost.clone())
}

pub(super) fn restore_cost(
    cas: &Cas,
    authority: &RunAuthority,
) -> Result<OperatorAttemptCost, String> {
    if authority.schema != RUN_AUTHORITY_SCHEMA {
        return Err(format!(
            "Task run authority requires schema {RUN_AUTHORITY_SCHEMA}"
        ));
    }
    validate(&authority.provider_admission)?;
    let bytes = cas.get(&authority.catalog_id).map_err(|e| e.to_string())?;
    let catalog: TaskCatalog = super::parse(std::path::Path::new("catalog.toml"), &bytes)?;
    if catalog_cost(&catalog)? != authority.provider_admission {
        return Err("Provider admission cost differs from its captured catalog".into());
    }
    Ok(authority.provider_admission.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const CATALOG: &str = r#"{"schema":"af.task-catalog/2","no_match":"refuse","packages":{},"independence":{"command_workers_by_package":true,"distinct_principals":true,"distinct_providers":false,"distinct_models":false},"providers":{}}"#;
    const AUTHORITY: &str = r#"{"schema":"af.task-run-authority/2","provider_admission":{"tokens":4096,"wall_ms":45000},"engine_id":"engine","catalog_id":"catalog","no_match":"refuse","packages":{},"independence":{"command_workers_by_package":true,"distinct_principals":true,"distinct_providers":false,"distinct_models":false},"providers":{}}"#;

    fn catalog(value: Value) -> Result<OperatorAttemptCost, String> {
        let catalog = serde_json::from_value(value).map_err(|e| e.to_string())?;
        catalog_cost(&catalog)
    }

    #[test]
    fn an_omitted_catalog_cost_is_the_fixed_default_allowance() {
        let parsed: TaskCatalog = serde_json::from_str(CATALOG).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), CATALOG);
        assert_eq!(catalog_cost(&parsed).unwrap(), DEFAULT_ADMISSION);
        assert_eq!(
            DEFAULT_ADMISSION,
            OperatorAttemptCost {
                tokens: 4096,
                wall_ms: 45000
            }
        );
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path()).unwrap();
        let mut authority: RunAuthority = serde_json::from_str(AUTHORITY).unwrap();
        assert_eq!(serde_json::to_string(&authority).unwrap(), AUTHORITY);
        authority.catalog_id = cas
            .put(
                toml::to_string(&serde_json::from_str::<Value>(CATALOG).unwrap())
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        assert_eq!(restore_cost(&cas, &authority).unwrap(), DEFAULT_ADMISSION);
        // Restoration re-parses the captured catalog and refuses a widened allowance.
        authority.provider_admission = OperatorAttemptCost {
            tokens: 32768,
            wall_ms: 45000,
        };
        assert!(
            restore_cost(&cas, &authority)
                .unwrap_err()
                .contains("captured catalog")
        );
        // An unreadable or retired catalog schema is refused, never silently defaulted.
        authority.catalog_id = cas.put(b"schema = 'af.task-catalog/1'\n").unwrap();
        assert!(restore_cost(&cas, &authority).is_err());
        let mut retired: Value = serde_json::from_str(CATALOG).unwrap();
        retired["schema"] = json!("af.task-catalog/1");
        assert!(catalog(retired).is_err());
    }

    #[test]
    fn an_explicit_cost_is_bounded_and_matches_its_exact_captured_catalog() {
        let mut value: Value = serde_json::from_str(CATALOG).unwrap();
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
        let cost = catalog(value.clone()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path()).unwrap();
        let mut authority: RunAuthority = serde_json::from_str(AUTHORITY).unwrap();
        authority.catalog_id = cas
            .put(toml::to_string(&value).unwrap().as_bytes())
            .unwrap();
        assert!(restore_cost(&cas, &authority).is_err());
        authority.provider_admission = cost.clone();
        assert_eq!(restore_cost(&cas, &authority).unwrap(), cost);
        authority.provider_admission.tokens += 1;
        assert!(
            restore_cost(&cas, &authority)
                .unwrap_err()
                .contains("captured catalog")
        );
        authority.schema = "af.task-run-authority/1".into();
        assert!(restore_cost(&cas, &authority).is_err());
    }
}
