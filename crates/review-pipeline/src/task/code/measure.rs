//! The installed `measure` and `compare` operators (ADR-0132). A measure runs one declared
//! command of the captured code policy, repetition by repetition, against a fresh read-only
//! materialization of the exact source Snapshot and a private runtime directory. Everything a
//! Measurement carries is the kernel's own observation, except the metrics the command prints on
//! its last stdout line, which are admitted only with the keys and units the policy declared. A
//! comparison is the pure fold of `review_core::task::measurement` over two such artifacts.

use super::*;
use review_check::CheckEnding;
use review_core::task::measurement::{
    MeasurementCacheV1, MeasurementFailureReasonV1, MeasurementFailureV1, MeasurementRunV1,
    parse_report, summarize,
};

/// The warm kind a measure's `CARGO_TARGET_DIR` comes from; its observation names what the
/// repetition actually got.
const CARGO_TARGET: &str = "cargo_target";

/// The message a changed or added source entry fails a measurement with: the one a mutated
/// check produces.
const SOURCE_MUTATED: &str = "Check mutated its input Snapshot";

fn now_ms() -> Result<u64, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64)
}

/// What one repetition came to.
enum Repetition {
    Completed(MeasurementRunV1),
    Failed(MeasurementRunV1, MeasurementFailureReasonV1, String),
}

impl CodeTaskDomain {
    /// The Warm Check Cache layer one measure's repetitions receive: the policy's `[warm]`
    /// table, less `cargo_target` for a `warm = false` measure, which builds into a fresh
    /// directory of its own runtime directory instead. `None` when nothing is left to bind.
    fn measure_warm_policy(&self, definition: &MeasureDefinitionV1) -> Option<CodeWarmPolicy> {
        let mut warm = self.policy.warm.clone()?;
        if !definition.warm {
            warm.build_cache
                .retain(|kind| *kind != super::super::warm_check::WarmBuildCacheKind::CargoTarget);
        }
        (!warm.build_cache.is_empty() || !warm.caches.is_empty()).then_some(warm)
    }

    /// The resolved command a measure runs: its program and resolved arguments.
    fn command_record(definition: &MeasureDefinitionV1) -> Result<serde_json::Value, String> {
        let args = definition
            .command
            .resolve()
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "schema": "af.measure-command/1",
            "program": definition.command.program,
            "args": args
        }))
    }

    /// Record the resolved command and return its content identity.
    fn command_id(&self, cas: &Cas, definition: &MeasureDefinitionV1) -> Result<String, String> {
        cas.put_json(&Self::command_record(definition)?)
            .map_err(|e| e.to_string())
    }

    pub(super) fn measure(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        measures: &BTreeSet<String>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let source = input.inputs.get("source").ok_or("Measure needs source")?;
        let (snapshot_id, snapshot, manifest) = source_snapshot(cas, source)?;
        let repository = review_source_git::task::read_origin(cas, &snapshot.origin_id)?;
        let mut outputs = BTreeMap::new();
        for name in measures {
            let definition = self
                .policy
                .measures
                .get(name)
                .ok_or("Named measure is not captured")?;
            let warm = self.measure_warm_policy(definition);
            let mut session = match &warm {
                Some(warm) => Some(WarmSession::new(
                    &self.warm,
                    warm,
                    repository.repository_id(),
                    ToolchainDeclaration::from_manifest(cas, &manifest)?,
                )),
                None => None,
            };
            let rustup = self.policy.warm.as_ref().map(|_| RustupHome::of_kernel());
            let command_id = self.command_id(cas, definition)?;
            let check = CheckDefinition {
                name: name.clone(),
                command: definition.command.clone(),
                required: true,
                remote: None,
            };
            let mut runs = Vec::new();
            let mut failure = None;
            let mut toolchain_id = None;
            for repetition in 1..=definition.repetitions {
                super::super::control::check(cancellation)?;
                let outcome = self.repeat(
                    cas,
                    &manifest,
                    attempt,
                    definition,
                    &check,
                    session.as_mut(),
                    rustup.as_ref(),
                    &mut toolchain_id,
                    cancellation,
                )?;
                match outcome {
                    Repetition::Completed(run) => runs.push(run),
                    Repetition::Failed(run, reason, detail) => {
                        runs.push(run);
                        failure = Some(MeasurementFailureV1 {
                            repetition,
                            reason,
                            detail,
                        });
                        // A failed repetition ends the measurement: no later one runs.
                        break;
                    }
                }
            }
            let summary = match failure {
                None => summarize(&definition.declared(), &runs)?,
                Some(_) => BTreeMap::new(),
            };
            let measurement = MeasurementV1 {
                plan_id: input.plan_id.clone(),
                policy_id: self.policy_id.clone(),
                snapshot_id: snapshot_id.clone(),
                measure: name.clone(),
                command_id: command_id.clone(),
                toolchain_id,
                warm: definition.warm,
                repetitions: definition.repetitions,
                wall_ms: definition.wall_ms,
                metrics: definition.declared(),
                outcome: if failure.is_some() {
                    ReceiptOutcomeV1::Failed
                } else {
                    ReceiptOutcomeV1::Passed
                },
                failure,
                runs,
                summary,
            };
            measurement.validate()?;
            let refs = measurement
                .runs
                .iter()
                .flat_map(|run| run.stdout_id.iter().chain(run.stderr_id.iter()).cloned())
                .chain(source.artifact_ids.iter().cloned())
                .chain([command_id, input.plan_id.clone(), self.policy_id.clone()])
                .collect();
            let id = cas
                .put_artifact(
                    MEASUREMENT_V1,
                    invocation_producer(cas, input, Some(attempt))?,
                    refs,
                    Some(snapshot_id.clone()),
                    serde_json::to_value(measurement).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            outputs.insert(
                name.clone(),
                ArtifactInputV1 {
                    artifact_ids: vec![id],
                    artifact_type: MEASUREMENT_V1.into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: Some(snapshot_id.clone()),
                },
            );
        }
        Ok(outputs)
    }

    /// Run one repetition and classify it. The source is re-verified after the command, and a
    /// changed or added entry outranks every other outcome.
    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &self,
        cas: &Cas,
        manifest: &review_source_git::Manifest,
        attempt: &PreparedTaskAttempt,
        definition: &MeasureDefinitionV1,
        check: &CheckDefinition,
        session: Option<&mut WarmSession<'_>>,
        rustup: Option<&RustupHome>,
        toolchain_id: &mut Option<String>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Repetition, String> {
        let deadline = attempt.reservation().deadline_unix_ms;
        let started = now_ms()?;
        let never_started = |detail: &str| {
            Repetition::Failed(
                MeasurementRunV1 {
                    started_unix_ms: started,
                    elapsed_ms: 0,
                    exit_code: None,
                    stdout_id: None,
                    stderr_id: None,
                    cache: None,
                    metrics: BTreeMap::new(),
                },
                MeasurementFailureReasonV1::Deadline,
                detail.into(),
            )
        };
        if deadline.saturating_sub(started) == 0 {
            return Ok(never_started(
                "the measure Attempt's deadline left no time to start this repetition",
            ));
        }
        let sandbox =
            Sandbox::materialize(manifest, cas, Mode::ReadOnly).map_err(|e| e.to_string())?;
        review_sandbox::admit(self.policy.isolation(), &sandbox).map_err(|e| e.to_string())?;
        // HOME, TMPDIR, XDG_CACHE_HOME and a cold CARGO_TARGET_DIR live here, and all of it is
        // discarded with the repetition: the command reports the bytes it cares about itself.
        let runtime = tempfile::tempdir().map_err(|e| e.to_string())?;
        for directory in ["tmp", "cache"] {
            std::fs::create_dir_all(runtime.path().join(directory)).map_err(|e| e.to_string())?;
        }
        let remaining = deadline.saturating_sub(now_ms()?);
        let runner = CheckRunner::new(cas, sandbox.root())
            .with_cancellation(cancellation)
            .with_timeout(Duration::from_millis(definition.wall_ms.min(remaining)))
            .with_env("HOME", runtime.path().display().to_string())
            .with_env("TMPDIR", runtime.path().join("tmp").display().to_string())
            .with_env(
                "XDG_CACHE_HOME",
                runtime.path().join("cache").display().to_string(),
            );
        let runner = rustup
            .iter()
            .flat_map(|rustup| rustup.environment())
            .fold(runner, |runner, (key, value)| runner.with_env(key, value));
        let (session, prepared) = match session {
            Some(session) => {
                let prepared = session.prepare(
                    cas,
                    runner.local_environment(),
                    sandbox.root(),
                    runtime.path(),
                    cancellation,
                    Duration::from_millis(remaining),
                )?;
                (Some(&*session), Some(prepared))
            }
            None => (None, None),
        };
        if let Some(refusal) = prepared.as_ref().and_then(|p| p.refusal.clone()) {
            // A declared Cache Snapshot that cannot be materialized is infrastructure, not a
            // measured result: the Attempt fails, exactly as the check it mirrors does not run.
            return Err(refusal);
        }
        // What the warm layer actually gave this repetition's cargo target: the kind's
        // observation says whether the warm directory was bound, its bytes and, when the
        // repetition ran cold instead, why — a busy key, a discarded directory, an unresolved
        // toolchain. The policy's `warm` is what was asked; this is what was had.
        let cache = prepared.as_ref().and_then(|prepared| {
            prepared
                .observations
                .iter()
                .find(|observation| {
                    observation.kind == CARGO_TARGET
                        || observation
                            .kind
                            .strip_prefix(CARGO_TARGET)
                            .is_some_and(|rest| rest.starts_with(':'))
                })
                .map(|observation| {
                    // A directory the kernel discarded and recreated before binding held
                    // nothing for this repetition: cold, with the discard as its reason.
                    let warm = observation.eligible && observation.discarded.is_none();
                    MeasurementCacheV1 {
                        warm,
                        bytes: if warm { observation.bytes_available } else { 0 },
                        reason: if observation.eligible {
                            // The diagnostic names a host path; the record keeps only what
                            // was found, bounded, never where.
                            observation.discarded.as_ref().map(|why| {
                                let what: String = why
                                    .split(" at ")
                                    .next()
                                    .unwrap_or(why)
                                    .chars()
                                    .take(120)
                                    .collect();
                                format!("discarded: {what}")
                            })
                        } else {
                            Some(
                                observation
                                    .kind
                                    .split_once(':')
                                    .map_or("cold", |(_, reason)| reason)
                                    .to_string(),
                            )
                        },
                    }
                })
        });
        if toolchain_id.is_none() {
            *toolchain_id = prepared.as_ref().and_then(|prepared| {
                prepared
                    .observations
                    .iter()
                    .find_map(|observation| observation.toolchain_id.clone())
            });
        }
        // Preparation spent Attempt time: the repetition runs against what is left now.
        let remaining = deadline.saturating_sub(now_ms()?).min(remaining);
        if remaining == 0 {
            if let (Some(session), Some(prepared)) = (session, prepared) {
                session.finish(prepared.key_lock, prepared.directories, None)?;
            }
            return Ok(never_started(
                "the measure Attempt's deadline ran out while its warm layer was prepared",
            ));
        }
        let timeout_ms = definition.wall_ms.min(remaining);
        let runner = runner.with_timeout(Duration::from_millis(timeout_ms));
        let runner = match &prepared {
            Some(prepared) => prepared
                .environment
                .iter()
                .fold(runner, |runner, (key, value)| {
                    runner.with_env(key.clone(), value.clone())
                }),
            None => runner.with_env(
                "CARGO_TARGET_DIR",
                runtime.path().join("target").display().to_string(),
            ),
        };
        let (execution, exceeded) =
            match (session, prepared.as_ref().and_then(|p| p.key_lock.as_ref())) {
                (Some(session), Some(key)) => session.run_monitored(
                    runner,
                    check,
                    key,
                    &prepared.as_ref().expect("prepared").directories,
                    cancellation,
                ),
                _ => (runner.run_observed(check), None),
            };
        if let (Some(session), Some(prepared)) = (session, prepared) {
            session.finish(prepared.key_lock, prepared.directories, exceeded.clone())?;
        }
        super::super::control::check(cancellation)?;
        let ending = execution.ending;
        let result = execution.result;
        let run = MeasurementRunV1 {
            started_unix_ms: execution.started_unix_ms,
            elapsed_ms: execution.elapsed_ms,
            exit_code: observed_exit_code(ending, result.exit_code),
            stdout_id: result.stdout.clone(),
            stderr_id: result.stderr.clone(),
            cache,
            metrics: BTreeMap::new(),
        };
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if !sealed.unchanged() {
            return Ok(Repetition::Failed(
                run,
                MeasurementFailureReasonV1::SourceMutated,
                SOURCE_MUTATED.into(),
            ));
        }
        // Above `max_bytes` only, the directories were evicted and the repetition stands
        // (ADR-0135); the hard bound and suspicion end it.
        let ended = match exceeded {
            Some(super::super::warm_check::Excess::Suspect(_)) => Some(WARM_CACHE_SUSPECT),
            Some(super::super::warm_check::Excess::Bound(_)) => Some(WARM_CACHE_BOUND_EXCEEDED),
            Some(super::super::warm_check::Excess::Evict(_)) | None => None,
        };
        if let Some(reason) = ended {
            return Ok(Repetition::Failed(
                run,
                MeasurementFailureReasonV1::Exit,
                format!("the kernel ended the command: {reason}"),
            ));
        }
        if let Some((reason, detail)) = timeout_failure(ending, timeout_ms, definition.wall_ms) {
            return Ok(Repetition::Failed(run, reason, detail));
        }
        if result.status != CheckStatus::Passed {
            let detail = match (result.exit_code, &result.reason) {
                _ if ending == CheckEnding::Signaled => "the command was ended by a signal".into(),
                (_, Some(reason)) => reason.clone(),
                (Some(code), None) => format!("the command exited {code}"),
                (None, None) => "the command did not exit".into(),
            };
            return Ok(Repetition::Failed(
                run,
                MeasurementFailureReasonV1::Exit,
                detail,
            ));
        }
        let stdout = cas
            .get(
                result
                    .stdout
                    .as_deref()
                    .ok_or("A completed command kept no stdout")?,
            )
            .map_err(|e| e.to_string())?;
        match parse_report(&stdout, &definition.declared()) {
            Ok(metrics) => Ok(Repetition::Completed(MeasurementRunV1 { metrics, ..run })),
            Err(refusal) => Ok(Repetition::Failed(run, refusal.reason, refusal.detail)),
        }
    }

    /// One recorded Measurement, checked against this Task's plan, policy and measure. With
    /// `port`, it must also be the measure that port names.
    pub(super) fn measurement(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
        port: Option<&str>,
    ) -> Result<MeasurementV1, String> {
        let artifact = envelope(cas, id)?;
        if artifact.artifact_type != MEASUREMENT_V1 {
            return Err("Measurement input has another type".into());
        }
        let measurement: MeasurementV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        measurement.validate()?;
        let definition = self
            .policy
            .measures
            .get(&measurement.measure)
            .ok_or("Measurement names a measure the policy lacks")?;
        if measurement.plan_id != input.plan_id
            || measurement.policy_id != self.policy_id
            || artifact.subject_snapshot_id.as_ref() != Some(&measurement.snapshot_id)
            || port.is_some_and(|port| port != measurement.measure)
            || measurement.command_id
                != review_store::canonical::content_id(&Self::command_record(definition)?)
                    .map_err(|e| e.to_string())?
            || measurement.warm != definition.warm
            || measurement.repetitions != definition.repetitions
            || measurement.wall_ms != definition.wall_ms
            || measurement.metrics != definition.declared()
        {
            return Err("Measurement differs from this Task's captured measure".into());
        }
        cas.verify(&measurement.command_id)
            .map_err(|e| e.to_string())?;
        for run in &measurement.runs {
            for id in run.stdout_id.iter().chain(run.stderr_id.iter()) {
                cas.verify(id).map_err(|e| e.to_string())?;
            }
        }
        Ok(measurement)
    }

    /// The comparison of the invocation's exact baseline and candidate under `objective`; the
    /// same two Measurements always give the same artifact.
    pub(super) fn compare(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        objective: &str,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let definition = self
            .policy
            .objectives
            .get(objective)
            .ok_or("Comparison names an objective the policy lacks")?;
        let side = |port: &str| -> Result<(String, MeasurementV1, ArtifactInputV1), String> {
            let value = input
                .inputs
                .get(port)
                .ok_or_else(|| format!("Comparison needs {port}"))?;
            value.validate()?;
            let [id] = value.artifact_ids.as_slice() else {
                return Err(format!("Comparison {port} is not one Measurement"));
            };
            let measurement = self.measurement(cas, input, id, None)?;
            if value.artifact_type != MEASUREMENT_V1
                || value.snapshot_id.as_ref() != Some(&measurement.snapshot_id)
            {
                return Err(format!("Comparison {port} changed its Snapshot"));
            }
            Ok((id.clone(), measurement, value.clone()))
        };
        let (baseline_id, baseline, _) = side("baseline")?;
        let (candidate_id, candidate, candidate_port) = side("candidate")?;
        let comparison = compare_measurements(
            &input.plan_id,
            &self.policy_id,
            (&baseline_id, &baseline),
            (&candidate_id, &candidate),
            ComparisonObjective {
                name: objective,
                objective: definition,
            },
        )?;
        let id = cas
            .put_artifact(
                MEASUREMENT_COMPARISON_V1,
                invocation_producer(cas, input, None)?,
                vec![
                    baseline_id,
                    candidate_id,
                    input.plan_id.clone(),
                    self.policy_id.clone(),
                ],
                candidate_port.snapshot_id.clone(),
                serde_json::to_value(comparison).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(BTreeMap::from([(
            "result".into(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: MEASUREMENT_COMPARISON_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: candidate_port.snapshot_id,
            },
        )]))
    }
}

/// The failure a repetition records when the kernel ended its command at a time bound: the
/// Attempt deadline when it was the tighter bound, else the repetition's own `wall_ms`. The
/// supervisor reports the bound typed, so a command that printed before it was ended — its
/// output is kept — is still a timeout, never an exit.
fn timeout_failure(
    ending: CheckEnding,
    timeout_ms: u64,
    wall_ms: u64,
) -> Option<(MeasurementFailureReasonV1, String)> {
    match ending {
        CheckEnding::TimedOut if timeout_ms < wall_ms => Some((
            MeasurementFailureReasonV1::Deadline,
            format!("the measure Attempt's deadline cut the repetition after {timeout_ms} ms"),
        )),
        CheckEnding::TimedOut => Some((
            MeasurementFailureReasonV1::Timeout,
            format!("the repetition exceeded its wall_ms of {wall_ms} ms"),
        )),
        CheckEnding::Exited
        | CheckEnding::Signaled
        | CheckEnding::Cancelled
        | CheckEnding::NotStarted => None,
    }
}

/// The exit code a run records: the command's own, and only when it exited. A command a signal
/// or the kernel ended produced none; the runner's `-1` for a signal is its sentinel, not an
/// observation.
fn observed_exit_code(ending: CheckEnding, code: Option<i32>) -> Option<i32> {
    match ending {
        CheckEnding::Exited => code,
        CheckEnding::Signaled
        | CheckEnding::TimedOut
        | CheckEnding::Cancelled
        | CheckEnding::NotStarted => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_bound_is_named_by_whichever_was_tighter() {
        let (reason, detail) = timeout_failure(CheckEnding::TimedOut, 20_000, 20_000).unwrap();
        assert_eq!(reason, MeasurementFailureReasonV1::Timeout);
        assert_eq!(detail, "the repetition exceeded its wall_ms of 20000 ms");
        let (reason, detail) = timeout_failure(CheckEnding::TimedOut, 1_500, 20_000).unwrap();
        assert_eq!(reason, MeasurementFailureReasonV1::Deadline);
        assert_eq!(
            detail,
            "the measure Attempt's deadline cut the repetition after 1500 ms"
        );
        for ending in [
            CheckEnding::Exited,
            CheckEnding::Signaled,
            CheckEnding::Cancelled,
            CheckEnding::NotStarted,
        ] {
            assert!(timeout_failure(ending, 1_500, 20_000).is_none());
        }
    }

    #[test]
    fn only_a_command_that_exited_has_an_exit_code() {
        assert_eq!(observed_exit_code(CheckEnding::Exited, Some(3)), Some(3));
        assert_eq!(observed_exit_code(CheckEnding::Signaled, Some(-1)), None);
        assert_eq!(observed_exit_code(CheckEnding::TimedOut, Some(-1)), None);
    }
}
