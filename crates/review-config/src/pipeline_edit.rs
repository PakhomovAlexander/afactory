//! Typed, formatting-preserving pipeline edits for interactive configuration proposals.

use review_graph::{Node, Pipeline, Port};
use toml_edit::{DocumentMut, Item, Table, Value, value};

use crate::lock::{Lockfile, Registry};
use crate::{Definition, NodeKindSpec, SeveritySpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineView {
    pub subject: review_core::SubjectKind,
    pub nodes: Vec<PipelineNode>,
    pub edges: Vec<PipelineEdge>,
    pub policy: PipelinePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineNode {
    pub id: String,
    pub kind: String,
    pub package: Option<String>,
    pub gated_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineEdge {
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelinePolicy {
    pub attempt_budget: Option<u64>,
    pub run_budget: Option<u64>,
    pub clean_rounds: u32,
    pub max_rounds: u32,
    pub gate: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineSetting {
    AttemptBudget,
    RunBudget,
    CleanRounds,
    MaxRounds,
    Gate,
}

pub fn pipeline_view(text: &str) -> Result<PipelineView, String> {
    let definition: Definition =
        toml::from_str(text).map_err(|error| format!("pipeline definition: {error}"))?;
    let subject = match (definition.version, definition.subject.as_ref()) {
        (1, None) => review_core::SubjectKind::WholeTree,
        (_, Some(subject)) => subject.kind,
        _ => return Err("pipeline subject does not match its format version".to_string()),
    };
    Ok(PipelineView {
        subject,
        nodes: definition
            .nodes
            .iter()
            .map(|node| PipelineNode {
                id: node.id.clone(),
                kind: node_kind(node.kind).to_string(),
                package: node.package.clone(),
                gated_by: node.gated_by.clone(),
            })
            .collect(),
        edges: definition
            .edges
            .iter()
            .map(|edge| PipelineEdge {
                from_node: edge.from.node.clone(),
                from_port: edge.from.port.clone(),
                to_node: edge.to.node.clone(),
                to_port: edge.to.port.clone(),
            })
            .collect(),
        policy: PipelinePolicy {
            attempt_budget: definition.budgets.map(|budgets| budgets.attempt),
            run_budget: definition.budgets.map(|budgets| budgets.run),
            clean_rounds: definition.convergence.clean_rounds,
            max_rounds: definition.convergence.max_rounds,
            gate: severity(definition.convergence.gate).to_string(),
        },
    })
}

pub fn validate_pipeline(text: &str, lock: &Lockfile, registry: &Registry) -> Result<(), String> {
    let definition: Definition =
        toml::from_str(text).map_err(|error| format!("pipeline definition: {error}"))?;
    definition
        .load_with(lock, registry)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub fn validate_pipeline_structure(text: &str) -> Result<(), String> {
    let definition: Definition =
        toml::from_str(text).map_err(|error| format!("pipeline definition: {error}"))?;
    if definition.nodes.is_empty() {
        return Err("pipeline defines no nodes".to_string());
    }
    if !definition
        .nodes
        .iter()
        .any(|node| node.kind == NodeKindSpec::Reviewer)
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

pub fn update_pipeline_setting(
    text: &str,
    setting: PipelineSetting,
    input: &str,
) -> Result<String, String> {
    let mut document = parse_document(text)?;
    match setting {
        PipelineSetting::AttemptBudget | PipelineSetting::RunBudget => {
            let number = positive_u64(input, "budget")?;
            let field = match setting {
                PipelineSetting::AttemptBudget => "attempt",
                PipelineSetting::RunBudget => "run",
                _ => unreachable!(),
            };
            let budgets = document
                .get_mut("budgets")
                .and_then(Item::as_table_mut)
                .ok_or("uncapped pipelines must add [budgets] in TOML before TUI editing")?;
            budgets[field] =
                value(i64::try_from(number).map_err(|_| "budget exceeds TOML integer range")?);
        }
        PipelineSetting::CleanRounds | PipelineSetting::MaxRounds => {
            let number = input
                .parse::<u32>()
                .map_err(|_| "round count must be a positive integer")?;
            if number == 0 {
                return Err("round count must be a positive integer".into());
            }
            let field = match setting {
                PipelineSetting::CleanRounds => "clean_rounds",
                PipelineSetting::MaxRounds => "max_rounds",
                _ => unreachable!(),
            };
            document["convergence"][field] = value(i64::from(number));
        }
        PipelineSetting::Gate => {
            if !matches!(input, "blocker" | "major" | "minor") {
                return Err("convergence gate must be blocker, major, or minor".into());
            }
            document["convergence"]["gate"] = value(input);
        }
    }
    let rendered = render(document)?;
    let policy = pipeline_view(&rendered)?.policy;
    if let (Some(attempt), Some(run)) = (policy.attempt_budget, policy.run_budget)
        && attempt > run
    {
        return Err(format!(
            "the attempt cap ({attempt}) exceeds the run cap ({run})"
        ));
    }
    if policy.clean_rounds > policy.max_rounds {
        return Err(format!(
            "clean rounds ({}) exceed max rounds ({})",
            policy.clean_rounds, policy.max_rounds
        ));
    }
    Ok(rendered)
}

pub fn rebind_reviewer(text: &str, node_id: &str, package: &str) -> Result<String, String> {
    safe_name(package, "package")?;
    let mut document = parse_document(text)?;
    let node = find_node_mut(&mut document, node_id)?;
    if node.get("kind").and_then(Item::as_str) != Some("reviewer") || node.get("package").is_none()
    {
        return Err(format!("node `{node_id}` is not a package-backed reviewer"));
    }
    node["package"] = value(package);
    render(document)
}

pub fn add_reviewer(text: &str, package: &str) -> Result<String, String> {
    safe_name(package, "reviewer package")?;
    let mut document = parse_document(text)?;
    let nodes = document
        .get("nodes")
        .and_then(Item::as_array_of_tables)
        .ok_or("pipeline has no editable [[nodes]] array")?;
    if nodes
        .iter()
        .any(|node| node.get("id").and_then(Item::as_str) == Some(package))
    {
        return Err(format!("pipeline already contains node `{package}`"));
    }
    let (template_id, mut reviewer) = nodes
        .iter()
        .find_map(|node| {
            if node.get("kind").and_then(Item::as_str) == Some("reviewer")
                && node.get("package").and_then(Item::as_str).is_some()
            {
                Some((node.get("id")?.as_str()?.to_string(), node.clone()))
            } else {
                None
            }
        })
        .ok_or("membership editing requires an existing package-backed reviewer template")?;
    reviewer["id"] = value(package);
    reviewer["package"] = value(package);

    let edges = document
        .get("edges")
        .and_then(Item::as_array_of_tables)
        .ok_or("pipeline has no editable [[edges]] array")?;
    let mut cloned_edges: Vec<Table> = edges
        .iter()
        .filter(|edge| edge_touches(edge, &template_id))
        .cloned()
        .collect();
    let outgoing: Vec<(String, String)> = cloned_edges
        .iter()
        .filter_map(|edge| {
            let from = edge_ref(edge, "from")?;
            let to = edge_ref(edge, "to")?;
            (from.0 == template_id).then_some(to)
        })
        .collect();
    if outgoing.len() != 1 {
        return Err(
            "membership editing requires one unambiguous reviewer result destination".into(),
        );
    }
    let (sink_id, sink_port) = &outgoing[0];
    let sink = find_node_mut(&mut document, sink_id)?;
    let inputs = sink
        .get_mut("inputs")
        .and_then(Item::as_array_mut)
        .ok_or_else(|| format!("reviewer result destination `{sink_id}` has no inputs array"))?;
    let mut sink_input = inputs
        .iter()
        .find(|input| inline_name(input) == Some(sink_port.as_str()))
        .cloned()
        .ok_or_else(|| {
            format!("reviewer result destination port `{sink_id}.{sink_port}` is absent")
        })?;
    sink_input
        .as_inline_table_mut()
        .expect("cloned an inline input port")
        .insert("name", Value::from(package));
    if inputs
        .iter()
        .any(|input| inline_name(input) == Some(package))
    {
        return Err(format!(
            "destination `{sink_id}` already has input `{package}`"
        ));
    }
    inputs.push(sink_input);

    for edge in &mut cloned_edges {
        remap_edge_node(edge, "from", &template_id, package)?;
        remap_edge_node(edge, "to", &template_id, package)?;
        if let Some((node, port)) = edge_ref(edge, "to")
            && node == *sink_id
            && port == *sink_port
        {
            set_edge_field(edge, "to", "port", package)?;
        }
    }
    document
        .get_mut("nodes")
        .and_then(Item::as_array_of_tables_mut)
        .expect("nodes were checked above")
        .push(reviewer);
    let edges = document
        .get_mut("edges")
        .and_then(Item::as_array_of_tables_mut)
        .expect("edges were checked above");
    for edge in cloned_edges {
        edges.push(edge);
    }
    render(document)
}

pub fn remove_reviewer(text: &str, node_id: &str) -> Result<String, String> {
    let mut document = parse_document(text)?;
    let destinations: Vec<(String, String)> = document
        .get("edges")
        .and_then(Item::as_array_of_tables)
        .ok_or("pipeline has no editable [[edges]] array")?
        .iter()
        .filter_map(|edge| {
            let from = edge_ref(edge, "from")?;
            let to = edge_ref(edge, "to")?;
            (from.0 == node_id).then_some(to)
        })
        .collect();
    let nodes = document
        .get_mut("nodes")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or("pipeline has no editable [[nodes]] array")?;
    let index = nodes
        .iter()
        .position(|node| {
            node.get("id").and_then(Item::as_str) == Some(node_id)
                && node.get("kind").and_then(Item::as_str) == Some("reviewer")
        })
        .ok_or_else(|| format!("node `{node_id}` is not a reviewer"))?;
    nodes.remove(index);

    let edges = document
        .get_mut("edges")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or("pipeline has no editable [[edges]] array")?;
    for index in (0..edges.len()).rev() {
        let edge = edges.get(index).expect("index came from this array length");
        if edge_touches(edge, node_id) {
            edges.remove(index);
        }
    }

    for (sink_id, sink_port) in destinations {
        let still_used = document
            .get("edges")
            .and_then(Item::as_array_of_tables)
            .is_some_and(|edges| {
                edges
                    .iter()
                    .filter_map(|edge| edge_ref(edge, "to"))
                    .any(|target| target == (sink_id.clone(), sink_port.clone()))
            });
        if !still_used {
            let sink = find_node_mut(&mut document, &sink_id)?;
            let inputs = sink
                .get_mut("inputs")
                .and_then(Item::as_array_mut)
                .ok_or_else(|| format!("reviewer result destination `{sink_id}` has no inputs"))?;
            for index in (0..inputs.len()).rev() {
                if inputs.get(index).and_then(inline_name) == Some(&sink_port) {
                    inputs.remove(index);
                }
            }
        }
    }
    render(document)
}

fn inline_name(value: &Value) -> Option<&str> {
    value
        .as_inline_table()
        .and_then(|table| table.get("name"))
        .and_then(Value::as_str)
}

fn edge_ref(edge: &Table, side: &str) -> Option<(String, String)> {
    let item = edge.get(side)?;
    if let Some(reference) = item.as_inline_table() {
        return Some((
            reference.get("node")?.as_str()?.to_string(),
            reference.get("port")?.as_str()?.to_string(),
        ));
    }
    let reference = item.as_table()?;
    Some((
        reference.get("node")?.as_str()?.to_string(),
        reference.get("port")?.as_str()?.to_string(),
    ))
}

fn set_edge_field(
    edge: &mut Table,
    side: &str,
    field: &str,
    field_value: &str,
) -> Result<(), String> {
    let item = edge
        .get_mut(side)
        .ok_or_else(|| format!("edge has no `{side}` endpoint"))?;
    if let Some(reference) = item.as_inline_table_mut() {
        reference.insert(field, Value::from(field_value));
        return Ok(());
    }
    let reference = item
        .as_table_mut()
        .ok_or_else(|| format!("edge `{side}` is not a port reference"))?;
    reference[field] = value(field_value);
    Ok(())
}

fn edge_touches(edge: &Table, node_id: &str) -> bool {
    ["from", "to"]
        .iter()
        .filter_map(|side| edge_ref(edge, side))
        .any(|reference| reference.0 == node_id)
}

fn remap_edge_node(
    edge: &mut Table,
    side: &str,
    old_node: &str,
    new_node: &str,
) -> Result<(), String> {
    if edge_ref(edge, side).is_some_and(|reference| reference.0 == old_node) {
        set_edge_field(edge, side, "node", new_node)?;
    }
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

fn find_node_mut<'a>(document: &'a mut DocumentMut, id: &str) -> Result<&'a mut Table, String> {
    document
        .get_mut("nodes")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|nodes| {
            nodes
                .iter_mut()
                .find(|node| node.get("id").and_then(Item::as_str) == Some(id))
        })
        .ok_or_else(|| format!("pipeline has no node `{id}`"))
}

fn positive_u64(input: &str, label: &str) -> Result<u64, String> {
    let value = input
        .parse::<u64>()
        .map_err(|_| format!("{label} must be a positive integer"))?;
    if value == 0 {
        return Err(format!("{label} must be a positive integer"));
    }
    Ok(value)
}

fn safe_name(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
        })
    {
        return Err(format!("{label} `{value}` is unsafe"));
    }
    Ok(())
}

fn node_kind(kind: NodeKindSpec) -> &'static str {
    match kind {
        NodeKindSpec::Generation => "generation",
        NodeKindSpec::Gate => "gate",
        NodeKindSpec::Reviewer => "reviewer",
        NodeKindSpec::Gather => "gather",
        NodeKindSpec::Ledger => "ledger",
    }
}

fn severity(severity: SeveritySpec) -> &'static str {
    match severity {
        SeveritySpec::Blocker => "blocker",
        SeveritySpec::Major => "major",
        SeveritySpec::Minor => "minor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIPELINE: &str = r#"version = 2
[subject]
kind = "diff"

[[nodes]]
id = "gate"
kind = "gate"
outputs = [{ name = "decision", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }, { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = [{ name = "gate", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }, { name = "prior_findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }, { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
package = "architecture"

[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "architecture", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "architecture", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "change_set" }
to = { node = "architecture", port = "change_set" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }
[[edges]]
from = { node = "architecture", port = "result" }
to = { node = "gather", port = "architecture" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }

[budgets]
unit = "tokens"
attempt = 100
run = 500

[convergence]
clean_rounds = 1
max_rounds = 3
gate = "major"
"#;

    #[test]
    fn policy_edits_preserve_the_graph() {
        let updated = update_pipeline_setting(PIPELINE, PipelineSetting::RunBudget, "800").unwrap();
        let view = pipeline_view(&updated).unwrap();
        assert_eq!(view.nodes.len(), 5);
        assert_eq!(view.edges.len(), 5);
        assert_eq!(view.policy.run_budget, Some(800));
        assert!(update_pipeline_setting(PIPELINE, PipelineSetting::CleanRounds, "4").is_err());
        let invalid = PIPELINE.replace("clean_rounds = 1", "clean_rounds = 4");
        let definition: Definition = toml::from_str(&invalid).unwrap();
        let error = match definition.load() {
            Ok(_) => panic!("invalid convergence policy loaded"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("requires 4 clean rounds but permits only 3 rounds")
        );
    }

    #[test]
    fn reviewer_membership_has_canonical_typed_wiring() {
        let added = add_reviewer(PIPELINE, "contracts").unwrap();
        let view = pipeline_view(&added).unwrap();
        assert!(view.nodes.iter().any(|node| node.id == "contracts"));
        assert_eq!(
            view.edges
                .iter()
                .filter(|edge| edge.from_node == "contracts" || edge.to_node == "contracts")
                .count(),
            4
        );
        let removed = remove_reviewer(&added, "contracts").unwrap();
        let view = pipeline_view(&removed).unwrap();
        assert!(!view.nodes.iter().any(|node| node.id == "contracts"));
        assert!(
            !view
                .edges
                .iter()
                .any(|edge| { edge.from_node == "contracts" || edge.to_node == "contracts" })
        );

        let renamed = PIPELINE
            .replace("id = \"gate\"", "id = \"preflight\"")
            .replace("gated_by = \"gate\"", "gated_by = \"preflight\"")
            .replace("node = \"gate\"", "node = \"preflight\"")
            .replace("id = \"generation\"", "id = \"history\"")
            .replace("node = \"generation\"", "node = \"history\"")
            .replace("id = \"gather\"", "id = \"collect\"")
            .replace("node = \"gather\"", "node = \"collect\"");
        let added = add_reviewer(&renamed, "contracts").unwrap();
        let view = pipeline_view(&added).unwrap();
        assert!(view.edges.iter().any(|edge| {
            edge.from_node == "preflight" && edge.to_node == "contracts" && edge.to_port == "gate"
        }));
        assert!(view.edges.iter().any(|edge| {
            edge.from_node == "contracts"
                && edge.to_node == "collect"
                && edge.to_port == "contracts"
        }));

        let with_second = add_reviewer(PIPELINE, "contracts").unwrap();
        let subtable_edges = with_second.replace(
            "from = { node = \"architecture\", port = \"result\" }\nto = { node = \"gather\", port = \"architecture\" }",
            "[edges.from]\nnode = \"architecture\"\nport = \"result\"\n[edges.to]\nnode = \"gather\"\nport = \"architecture\"",
        );
        let removed = remove_reviewer(&subtable_edges, "architecture").unwrap();
        let view = pipeline_view(&removed).unwrap();
        assert!(!view.nodes.iter().any(|node| node.id == "architecture"));
        assert!(view.nodes.iter().any(|node| node.id == "contracts"));
    }

    #[test]
    fn reviewer_rebinding_preserves_comments_and_topology() {
        let commented = PIPELINE.replace(
            "package = \"architecture\"",
            "# owner choice\npackage = \"architecture\"",
        );
        let updated = rebind_reviewer(&commented, "architecture", "contracts").unwrap();
        assert!(updated.contains("# owner choice"));
        assert_eq!(
            pipeline_view(&updated).unwrap().nodes[2].package.as_deref(),
            Some("contracts")
        );
    }
}
