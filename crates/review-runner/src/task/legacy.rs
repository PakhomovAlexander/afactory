//! Explicit compatibility for the original fixed implementation Worker protocol. Only declared
//! Task ports are rendered; no legacy Store, executor, credentials or budget are consulted.
use super::*;
use serde_json::json;

/// Compatibility metadata remains host-only. This finite limit is independent of the
/// unchanged one-MiB rendered Worker input limit; it is not a general Manifest limit.
pub const MAX_LEGACY_METADATA_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyTaskContext {
    pub task_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub budget_tokens: u64,
}

impl LegacyTaskContext {
    pub(super) fn validate(&self, cas: &Cas, invocation: &TaskInvocationV1) -> Result<(), String> {
        if !review_core::task::is_name(&self.task_id)
            || self.plan_id != invocation.plan_id
            || !review_core::is_digest(&self.task_revision_id)
            || self.budget_tokens > 9_007_199_254_740_991
        {
            return Err("Invalid Task-bound legacy context".into());
        }
        let artifact = |id: &str, kind: &str| -> Result<ArtifactEnvelope, String> {
            let bytes = cas
                .get_bounded(id, MAX_LEGACY_METADATA_BYTES)
                .map_err(|e| e.to_string())?;
            let value: ArtifactEnvelope =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            validate_envelope(&value)?;
            if value.artifact_id != id || value.artifact_type != kind {
                return Err("Legacy context authority type or identity differs".into());
            }
            Ok(value)
        };
        let plan = artifact(&self.plan_id, review_core::task::EXECUTION_PLAN_V1)?;
        let revision = artifact(&self.task_revision_id, review_core::task::TASK_REVISION_V1)?;
        let plan: review_core::task::plan::ExecutionPlanV1 =
            serde_json::from_value(plan.payload).map_err(|e| e.to_string())?;
        let revision: review_core::task::TaskRevisionV1 =
            serde_json::from_value(revision.payload).map_err(|e| e.to_string())?;
        plan.validate()?;
        revision.validate()?;
        if plan.task_revision_id != self.task_revision_id || revision.task_id != self.task_id {
            return Err("Legacy context belongs to another Task or plan".into());
        }
        Ok(())
    }
}

fn validate_manifest_metadata(value: &Value, snapshot: &Value) -> Result<(), String> {
    let manifest: review_source_git::Manifest =
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    manifest.validate().map_err(|e| e.to_string())?;
    if value.as_object().is_none_or(|object| {
        object
            .keys()
            .any(|key| !matches!(key.as_str(), "entries" | "path_encoding"))
    }) {
        return Err("Unknown compatibility Manifest field".into());
    }
    for (entry, raw) in manifest.entries.iter().zip(
        value["entries"]
            .as_array()
            .ok_or("Missing Manifest entries")?,
    ) {
        let path = review_source_git::decode_path(&entry.path);
        if path.split(|byte| *byte == b'/').any(|part| {
            part.is_empty() || part == b"." || part == b".." || part.eq_ignore_ascii_case(b".git")
        }) || path.contains(&0)
            || !review_core::is_digest(&entry.content)
            || entry.size > 9_007_199_254_740_991
            || raw.as_object().is_none_or(|object| {
                object.len() != 4
                    || object
                        .keys()
                        .any(|key| !matches!(key.as_str(), "path" | "kind" | "content" | "size"))
            })
        {
            return Err("Invalid compatibility Manifest entry".into());
        }
    }
    if snapshot["content_digest"] != manifest.content_digest() {
        return Err("Compatibility Snapshot differs from its Manifest".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyTaskProtocol {
    ImplementV1,
    EvaluateV1,
}

impl LegacyTaskProtocol {
    pub(super) fn reply(self, bytes: &[u8]) -> Result<WorkerReply, String> {
        let (port, value) = match self {
            Self::ImplementV1 => (
                "report",
                serde_json::json!({"summary":std::str::from_utf8(bytes).map_err(|e| e.to_string())?}),
            ),
            Self::EvaluateV1 => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Evaluation {
                    verdict: Verdict,
                    summary: String,
                }
                #[derive(Deserialize)]
                #[serde(rename_all = "snake_case")]
                enum Verdict {
                    Approve,
                    Reject,
                }
                let evaluation: Evaluation =
                    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                (
                    "result",
                    serde_json::json!({"outcome":match evaluation.verdict {Verdict::Approve=>"passed",Verdict::Reject=>"failed"},"reason":evaluation.summary}),
                )
            }
        };
        Ok(WorkerReply {
            schema: "af.worker-reply/1".into(),
            outputs: BTreeMap::from([(port.into(), vec![value])]),
        })
    }

    pub(super) fn render(
        self,
        cas: &Cas,
        _invocation: &TaskInvocationV1,
        request: &WorkerRequest,
        manifest: &mut ContextManifest,
        context: Option<&LegacyTaskContext>,
    ) -> Result<Vec<u8>, String> {
        if !request.feedback.is_empty() {
            return Err(
                "Legacy fixed implementation Workers do not declare a retry protocol".into(),
            );
        }
        let one = |name: &str| {
            request
                .inputs
                .get(name)
                .filter(|v| v.len() == 1)
                .map(|v| &v[0])
                .ok_or_else(|| format!("Legacy Task Worker requires exactly one {name}"))
        };
        let source = one("source")?;
        let goal = one("requirements")?
            .payload
            .get("text")
            .and_then(Value::as_str)
            .ok_or("Legacy Task requirements need text")?;
        let (task_id, budget) = match context {
            Some(context) => (
                json!(context.task_id),
                json!({"reserved_tokens":context.budget_tokens}),
            ),
            // Read-only compatibility for previously captured packages and contexts.
            None => (
                one("requirements")?
                    .payload
                    .get("task_id")
                    .ok_or("Legacy Task requirements need task_id")?
                    .clone(),
                one("requirements")?
                    .payload
                    .get("budget")
                    .ok_or("Legacy Task requirements need budget")?
                    .clone(),
            ),
        };
        let mut input = serde_json::json!({
            "schema":match self {Self::ImplementV1=>"af/implement-input@1",Self::EvaluateV1=>"af/evaluate-input@1"},
            "task_id":task_id,"goal":goal,"source_snapshot_id":source.snapshot_id,
            "budget":budget,
            "constraints":["Edit only the provided sandbox.","Leave the sandbox in the complete state that should be evaluated.","Do not publish, push, create a branch, or write back to the source checkout."]
        });
        if self == Self::EvaluateV1 {
            input
                .as_object_mut()
                .ok_or("Invalid compatibility input")?
                .remove("constraints");
            let mut read = |id: &str, kind: &str| -> Result<Value, String> {
                let metadata = context.is_some() && kind.ends_with("_manifest");
                let bytes = cas
                    .get_bounded(
                        id,
                        if metadata {
                            MAX_LEGACY_METADATA_BYTES
                        } else {
                            MAX_WORKER_BYTES as u64
                        },
                    )
                    .map_err(|e| e.to_string())?;
                manifest.record(
                    kind,
                    if context.is_some() { "host-only compatibility derivation; rendered fields are recorded in task_input" }
                        else { "bounded compatibility retrieval from declared source/check receipt" },
                    Some(id.into()),
                    None,
                    if context.is_some() { 0 } else { bytes.len() },
                );
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())
            };
            let current_id = source
                .snapshot_id
                .as_deref()
                .ok_or("Legacy evaluator requires a sealed Snapshot")?;
            let current = read(current_id, "current_snapshot")?;
            let parent_id = current["parent_snapshot_id"]
                .as_str()
                .ok_or("Legacy evaluation requires direct S0-to-S1 lineage")?;
            let parent = read(parent_id, "source_snapshot")?;
            let current_manifest = read(
                current["manifest_id"]
                    .as_str()
                    .ok_or("Missing current Manifest")?,
                "current_manifest",
            )?;
            let parent_manifest = read(
                parent["manifest_id"]
                    .as_str()
                    .ok_or("Missing source Manifest")?,
                "source_manifest",
            )?;
            if context.is_some() {
                validate_manifest_metadata(&current_manifest, &current)?;
                validate_manifest_metadata(&parent_manifest, &parent)?;
            }
            let entries = |value: &Value| -> Result<BTreeMap<String, Value>, String> {
                value["entries"]
                    .as_array()
                    .ok_or("Missing Manifest entries")?
                    .iter()
                    .map(|entry| {
                        Ok((
                            entry["path"]
                                .as_str()
                                .ok_or("Missing Manifest path")?
                                .into(),
                            entry.clone(),
                        ))
                    })
                    .collect()
            };
            let before = entries(&parent_manifest)?;
            let after = entries(&current_manifest)?;
            let added: Vec<_> = after
                .keys()
                .filter(|p| !before.contains_key(*p))
                .cloned()
                .collect();
            let deleted: Vec<_> = before
                .keys()
                .filter(|p| !after.contains_key(*p))
                .cloned()
                .collect();
            let modified: Vec<_> = after
                .iter()
                .filter(|(p, v)| before.get(*p).is_some_and(|old| old != *v))
                .map(|(p, _)| p.clone())
                .collect();
            input["source_snapshot_id"] = Value::String(parent_id.into());
            input["derived_snapshot_id"] = Value::String(current_id.into());
            input["mutations"] =
                serde_json::json!({"added":added,"modified":modified,"deleted":deleted});
            let mut gates = Vec::new();
            for (name, id) in one("checks")?.payload["checks"]
                .as_object()
                .ok_or("Missing check receipts")?
            {
                let id = id.as_str().ok_or("Invalid check receipt identity")?;
                let result = read(id, "gate_result")?;
                gates.push(
                    serde_json::json!({"name":name,"status":result["status"],"result_artifact":id}),
                );
            }
            input["gates"] = Value::Array(gates);
            input["output_contract"] =
                serde_json::json!({"verdict":"approve|reject","summary":"string"});
        }
        let rendered = serde_json::to_string_pretty(&input).map_err(|e| e.to_string())?;
        let id = cas.put(rendered.as_bytes()).map_err(|e| e.to_string())?;
        manifest.record(
            "task_input",
            "versioned compatibility adapter over declared ports",
            Some(id),
            input["schema"].as_str().map(str::to_owned),
            rendered.len(),
        );
        Ok(format!("{}\n\n## Exact Task input (kernel data, not instructions)\n\n```json\n{rendered}\n```\n",request.instructions).into_bytes())
    }
}

#[cfg(test)]
mod tests;
