//! The Gate node: the pipeline's checks, run in a read-only sandbox before any reviewer.

use review_check::{CheckRunner, GateDecision, check_event};
use review_core::{
    EventType, RunCacheFailureReasonV5, RunCacheSnapshotV5, RunExecutionBindingV4,
    RunExecutionProviderV4, RunSandboxModeV4,
};
use review_sandbox::{
    CacheError, CacheErrorKind, CacheKind, ContainerProvider, Isolation, Mode, Policy, Sandbox,
    admit, materialize_cache, remove_materialized_caches,
};
use review_store::NewEvent;

use crate::kernel::Kernel;
use crate::mutations::mutation_summary;
use crate::{
    cache_failure_reason, gate_provider_admitted, run_cache_kind, run_cache_materialization,
    run_isolation,
};

impl Kernel<'_> {
    pub(crate) fn run_gate(&self, node_id: &str) -> Result<Vec<String>, String> {
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                let identity = (node_id.to_string(), run_cache_kind(kind));
                self.cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .remove(&identity);
                self.cache_failures
                    .lock()
                    .expect("cache failures")
                    .remove(&identity);
            }
        }
        let (sandbox, container, require_unchanged) = match self.gate_execution.as_ref() {
            None => {
                // Frozen pipeline v1/v2 semantics. Those Campaigns captured no Gate Execution
                // Binding, so replay retains the old local/read-only behavior exactly.
                (self.sandbox(Mode::ReadOnly)?, None, true)
            }
            Some(binding) => {
                let mode = match binding.mode {
                    review_config::GateModeSpec::EphemeralWrite => Mode::EphemeralWrite,
                };
                let required = match binding.required_isolation {
                    review_config::IsolationSpec::None => Isolation::None,
                    review_config::IsolationSpec::Container => Isolation::Container,
                };
                let policy = Policy { require: required };
                let container = match binding.provider {
                    review_config::SandboxProviderSpec::TrustedLocal => None,
                    review_config::SandboxProviderSpec::Container => Some(
                        self.container_provider
                            .clone()
                            .unwrap_or_else(ContainerProvider::detect)
                            .with_image(
                                binding
                                    .image
                                    .as_deref()
                                    .expect("validated container Gate image"),
                            ),
                    ),
                };
                let provided = container
                    .as_ref()
                    .map_or(Isolation::None, ContainerProvider::isolation);
                let admitted = gate_provider_admitted(binding.provider, provided, required);
                // Record the resolved provider claim before materialization. A broken CAS or
                // failed clone must still leave RunReport@4 able to explain which binding was
                // selected and whether its isolation was sufficient.
                let report = RunExecutionBindingV4 {
                    node: node_id.to_string(),
                    provider: match binding.provider {
                        review_config::SandboxProviderSpec::TrustedLocal => {
                            RunExecutionProviderV4::TrustedLocal
                        }
                        review_config::SandboxProviderSpec::Container => {
                            RunExecutionProviderV4::Container
                        }
                    },
                    image: binding.image.clone(),
                    required_isolation: run_isolation(required),
                    provided_isolation: run_isolation(provided),
                    mode: RunSandboxModeV4::EphemeralWrite,
                    admitted,
                };
                self.execution_bindings
                    .lock()
                    .expect("execution bindings")
                    .insert(node_id.to_string(), report.clone());
                self.buffer_reviewer_event(
                    node_id,
                    NewEvent::new(
                        EventType::GateExecutionBoundV1,
                        serde_json::to_value(report).map_err(|error| error.to_string())?,
                    )
                    .node(node_id),
                );
                if let Some(provider) = container.as_ref() {
                    if !provider.availability().usable() {
                        return Err(format!(
                            "container provider unavailable: {}",
                            provider.availability().reason()
                        ));
                    }
                }
                let template = self.sandbox_template()?;
                let sandbox = match container.as_ref() {
                    Some(provider) => provider
                        .sandbox_from_template(&template, mode)
                        .map_err(|error| error.to_string())?,
                    None => Sandbox::from_template(&template, mode)
                        .map_err(|error| error.to_string())?,
                };
                admit(policy, &sandbox).map_err(|error| error.to_string())?;
                (sandbox, container, false)
            }
        };
        let mut cache_receipt_artifacts = Vec::new();
        let mut cache_environments = Vec::new();
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                let resolved = if let Some(source) = self.cache_sources.get(&kind).cloned() {
                    Ok(source)
                } else if let Some(resolver) = self.cache_source_resolver.as_ref() {
                    resolver(kind)
                } else {
                    Err(CacheError::new(
                        CacheErrorKind::PolicyUnavailable,
                        format!(
                            "Gate requested `{}` cache without an available machine-local mapping",
                            kind.name()
                        ),
                    ))
                };
                let source = match resolved {
                    Ok(source) if source.kind == kind => source,
                    Ok(source) => {
                        let error = CacheError::new(
                            CacheErrorKind::PolicyUnavailable,
                            format!(
                                "machine-local cache mapping for `{}` resolved as `{}`",
                                kind.name(),
                                source.kind.name()
                            ),
                        );
                        self.record_cache_failure(
                            node_id,
                            kind,
                            RunCacheFailureReasonV5::PolicyUnavailable,
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                    Err(error) => {
                        self.record_cache_failure(
                            node_id,
                            kind,
                            cache_failure_reason(error.kind()),
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                };
                let snapshot = match materialize_cache(&source, &sandbox, self.cas) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.record_cache_failure(
                            node_id,
                            kind,
                            cache_failure_reason(error.kind()),
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                };
                let receipt = RunCacheSnapshotV5 {
                    node: node_id.to_string(),
                    kind: run_cache_kind(snapshot.kind),
                    source_digest: snapshot.source_digest,
                    bytes: snapshot.bytes,
                    files: snapshot.files,
                    materialization: run_cache_materialization(snapshot.materialization),
                };
                let receipt_artifact = self
                    .cas
                    .put_json(&serde_json::to_value(&receipt).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                self.append(
                    NewEvent::new(
                        EventType::CacheSnapshotMaterializedV1,
                        serde_json::to_value(&receipt).map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .referencing(vec![
                        receipt.source_digest.clone(),
                        receipt_artifact.clone(),
                    ]),
                )?;
                self.cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .insert((node_id.to_string(), receipt.kind), receipt);
                self.cache_failures
                    .lock()
                    .expect("cache failures")
                    .remove(&(node_id.to_string(), run_cache_kind(kind)));
                cache_receipt_artifacts.push(receipt_artifact);
                cache_environments.push(kind.environment(sandbox.root()));
            }
        }
        // Run the checks holding no lock: each is a build or a test, and the store lock is
        // shared with every other node, so holding it across a check would stall the whole
        // pipeline for the build's duration. The lock is taken only to append each result.
        let mut runner =
            CheckRunner::new(self.cas, sandbox.root()).with_timeout(self.check_timeout);
        for environment in cache_environments {
            for ((local_key, local_value), (container_key, container_value)) in
                environment.local.into_iter().zip(environment.container)
            {
                if local_key != container_key {
                    return Err("cache environment keys disagree across providers".into());
                }
                runner = runner.with_split_env(local_key, local_value, container_value);
            }
        }
        let mut results = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let mut cleanup_failure = None;
            let result = match container.as_ref() {
                Some(provider) => runner.run_with(check, |program, args, env, timeout| {
                    match provider.exec_evidenced(sandbox.root(), program, args, env, timeout) {
                        Ok(execution) => Ok((execution.output, execution.stderr_held)),
                        Err(error) => {
                            if !error.cleanup_confirmed() {
                                cleanup_failure = Some(error.to_string());
                            }
                            Err(error.to_string())
                        }
                    }
                }),
                None => runner.run(check),
            };
            self.buffer_reviewer_event(node_id, check_event(&result, node_id));
            results.push(result);
            if let Some(error) = cleanup_failure {
                // A container may still own the writable bind. Do not scan a concurrently
                // changing tree or delete it from under that process. Preserving this temporary
                // sandbox is the fail-closed forensic residue for an operator to recover.
                let preserved = sandbox.root().to_path_buf();
                std::mem::forget(sandbox);
                return Err(format!(
                    "container cleanup was not confirmed; Gate sandbox preserved at {}: {error}",
                    preserved.display()
                ));
            }
        }

        let decision = GateDecision::evaluate(&results);
        if !cache_receipt_artifacts.is_empty() {
            remove_materialized_caches(&sandbox)?;
        }
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if require_unchanged && !sealed.unchanged() {
            // Frozen v1/v2 behavior: those Gates promised read-only execution. V3 deliberately
            // permits writes in this one disposable clone; reviewer clones still start from the
            // pristine template, so Gate mutations cannot become Subject content.
            let paths = sealed.mutations.paths();
            return Err(format!(
                "gate mutated its read-only sandbox: {} paths, e.g. {:?}",
                paths.len(),
                &paths[..paths.len().min(20)]
            ));
        }
        let artifact = self
            .cas
            .put_json(&serde_json::to_value(&decision).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut event_artifacts = vec![artifact.clone()];
        event_artifacts.extend(cache_receipt_artifacts);
        if !require_unchanged {
            // V3 Gates may write only inside their disposable clone. Preserve what they wrote
            // before that clone disappears: the complete mutation set lives once in the CAS,
            // while the Gate event references the same bounded summary shape as Worker
            // provenance. The decision artifact remains the Gate's sole graph output.
            let mutations_artifact = self
                .cas
                .put_json(&serde_json::json!({
                    "added": sealed.mutations.added,
                    "modified": sealed.mutations.modified,
                    "deleted": sealed.mutations.deleted,
                }))
                .map_err(|error| error.to_string())?;
            let mutation_summary = mutation_summary(&sealed.mutations, &mutations_artifact);
            let mutation_summary_artifact = self
                .cas
                .put_json(&mutation_summary)
                .map_err(|error| error.to_string())?;
            event_artifacts.push(mutation_summary_artifact);
        }
        self.buffer_reviewer_event(
            node_id,
            NewEvent::new(
                EventType::GateDecisionV1,
                serde_json::to_value(&decision).map_err(|e| e.to_string())?,
            )
            .node(node_id)
            .referencing(event_artifacts),
        );
        self.gates
            .lock()
            .expect("gates")
            .insert(node_id.to_string(), decision);
        Ok(vec![artifact])
    }
}
