//! Formatting-preserving format upgrades for pinned legacy pipelines.

use review_graph::{Node, Pipeline, Port};
use toml_edit::{DocumentMut, InlineTable, Item, Value};

use crate::{Definition, NodeKindSpec, PortContractSpec};

/// A format upgrade that a pinned legacy pipeline may need before the current kernel accepts
/// it. Every upgrade is additive and idempotent: it never touches reviewer packages, budgets,
/// convergence, checks, or edges, so a consumer can apply it without re-deciding policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyUpgrade {
    /// Since M4 the Ledger node must emit `review.kernel/DemandSet@1` beside its Finding Set;
    /// `af review plan` rejects a pipeline without it.
    LedgerDemandSetOutput,
}

impl LegacyUpgrade {
    /// Every known upgrade, in the order they are applied.
    pub const ALL: [LegacyUpgrade; 1] = [LegacyUpgrade::LedgerDemandSetOutput];

    /// One line naming the change, for previews and reports.
    pub fn describe(self) -> &'static str {
        match self {
            LegacyUpgrade::LedgerDemandSetOutput => {
                "add a review.kernel/DemandSet@1 output to the Ledger node"
            }
        }
    }

    fn needed(self, definition: &Definition) -> bool {
        match self {
            LegacyUpgrade::LedgerDemandSetOutput => {
                let mut ledgers = definition
                    .nodes
                    .iter()
                    .filter(|node| matches!(node.kind, NodeKindSpec::Ledger))
                    .peekable();
                ledgers.peek().is_some()
                    && !ledgers.any(|node| {
                        node.outputs.iter().any(|port| {
                            matches!(port, PortContractSpec::Typed(typed)
                                if typed.artifact_type == review_core::contract::DEMAND_SET_V1)
                        })
                    })
            }
        }
    }

    fn apply(self, document: &mut DocumentMut) -> Result<(), String> {
        match self {
            LegacyUpgrade::LedgerDemandSetOutput => add_ledger_demand_set_output(document),
        }
    }
}

/// The upgrades a pipeline text still needs, in application order. Empty means the pipeline is
/// current for this kernel's format.
pub fn legacy_upgrades(text: &str) -> Result<Vec<LegacyUpgrade>, String> {
    let definition: Definition =
        toml::from_str(text).map_err(|error| format!("pipeline definition: {error}"))?;
    Ok(LegacyUpgrade::ALL
        .into_iter()
        .filter(|upgrade| upgrade.needed(&definition))
        .collect())
}

/// Applies every needed upgrade while preserving the pipeline's formatting and comments. Returns
/// `None` when nothing was needed, so a caller never rewrites a current file.
pub fn apply_legacy_upgrades(text: &str) -> Result<Option<String>, String> {
    let needed = legacy_upgrades(text)?;
    if needed.is_empty() {
        return Ok(None);
    }
    let mut document = parse_document(text)?;
    for upgrade in needed {
        upgrade.apply(&mut document)?;
    }
    let rendered = render(document)?;
    if !legacy_upgrades(&rendered)?.is_empty() {
        return Err("legacy upgrade did not converge; the pipeline still needs changes".into());
    }
    Ok(Some(rendered))
}

fn add_ledger_demand_set_output(document: &mut DocumentMut) -> Result<(), String> {
    let nodes = document
        .get_mut("nodes")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "pipeline has no nodes".to_string())?;
    let ledger = nodes
        .iter_mut()
        .find(|node| node.get("kind").and_then(Item::as_str) == Some("ledger"))
        .ok_or_else(|| "pipeline has no ledger node".to_string())?;
    let outputs = ledger
        .get_mut("outputs")
        .and_then(Item::as_array_mut)
        .ok_or_else(|| "ledger node has no outputs array".to_string())?;
    // Mirror the Finding Set output's cardinality and affinity so both Ledger outputs agree.
    let template = outputs
        .iter()
        .filter_map(Value::as_inline_table)
        .find(|table| {
            table.get("type").and_then(Value::as_str) == Some(review_core::contract::FINDING_SET_V1)
        })
        .cloned();
    let mut port = InlineTable::new();
    port.insert("name", Value::from("demands"));
    port.insert("type", Value::from(review_core::contract::DEMAND_SET_V1));
    for (key, default) in [
        ("cardinality", Value::from("one")),
        ("optional", Value::from(false)),
        ("snapshot_affinity", Value::from("same_subject")),
    ] {
        let mut copied = template
            .as_ref()
            .and_then(|table| table.get(key))
            .cloned()
            .unwrap_or(default);
        copied.decor_mut().clear();
        port.insert(key, copied);
    }
    port.fmt();
    outputs.push(Value::InlineTable(port));
    Ok(())
}

fn parse_document(text: &str) -> Result<DocumentMut, String> {
    text.parse()
        .map_err(|error| format!("pipeline formatting: {error}"))
}

fn render(document: DocumentMut) -> Result<String, String> {
    let rendered = document.to_string();
    let _: Definition =
        toml::from_str(&rendered).map_err(|error| format!("updated pipeline: {error}"))?;
    validate_pipeline_structure(&rendered)?;
    Ok(rendered)
}

fn validate_pipeline_structure(text: &str) -> Result<(), String> {
    let definition: Definition =
        toml::from_str(text).map_err(|error| format!("pipeline definition: {error}"))?;
    if definition.nodes.is_empty() {
        return Err("pipeline defines no nodes".to_string());
    }
    if !definition
        .nodes
        .iter()
        .any(|node| matches!(node.kind, NodeKindSpec::Reviewer | NodeKindSpec::Scatter))
    {
        return Err("pipeline defines no reviewer".to_string());
    }
    let mut pipeline = Pipeline::default();
    for spec in &definition.nodes {
        let mut node = Node::new(&spec.id, spec.kind.into())
            .accepting_contracts(spec.inputs.iter().map(|port| port.build()).collect())
            .emitting_contracts(spec.outputs.iter().map(|port| port.build()).collect());
        if let Some(gate) = &spec.gated_by {
            node = node.gated_by(gate);
        }
        pipeline = pipeline.node(node);
    }
    for edge in &definition.edges {
        pipeline = pipeline.edge(
            Port::new(&edge.from.node, &edge.from.port),
            Port::new(&edge.to.node, &edge.to.port),
        );
    }
    pipeline
        .plan()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod legacy_upgrade_tests {
    use super::{LegacyUpgrade, apply_legacy_upgrades, legacy_upgrades};

    const HUB_PIPELINE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/consumers/hub/.af/pipelines/review.toml"
    ));

    fn without_demand_set(text: &str) -> String {
        let line = text
            .lines()
            .find(|line| line.contains("review.kernel/DemandSet@1"))
            .expect("the hub fixture declares a DemandSet@1 output");
        text.replace(&format!("{line}\n"), "")
    }

    #[test]
    fn a_current_pipeline_needs_nothing_and_is_not_rewritten() {
        assert!(legacy_upgrades(HUB_PIPELINE).unwrap().is_empty());
        assert_eq!(apply_legacy_upgrades(HUB_PIPELINE).unwrap(), None);
    }

    #[test]
    fn a_pre_m4_ledger_gains_the_demand_set_output_and_keeps_everything_else() {
        let outdated = without_demand_set(HUB_PIPELINE);
        assert_eq!(
            legacy_upgrades(&outdated).unwrap(),
            vec![LegacyUpgrade::LedgerDemandSetOutput]
        );
        let upgraded = apply_legacy_upgrades(&outdated).unwrap().unwrap();
        assert!(upgraded.contains("review.kernel/DemandSet@1"));
        assert!(legacy_upgrades(&upgraded).unwrap().is_empty());
        // Additive: comments, checks, reviewers, budgets, and convergence survive verbatim.
        for line in outdated.lines() {
            assert!(upgraded.contains(line), "line lost by the upgrade: {line}");
        }
        assert_eq!(
            apply_legacy_upgrades(&upgraded).unwrap(),
            None,
            "idempotent"
        );
    }
}
