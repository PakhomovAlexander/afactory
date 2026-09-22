//! Token-free declared history capture for the M1 report-only Optimization Task.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use review_core::task::optimization::*;
use review_core::task::usage::DecimalU64;
use review_store::{Cas, EventStore, content_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::task_execution;

mod optimization_adapters;
use optimization_adapters::{AdapterState, AdapterVersion, parse_adapter_record};

const DEFAULT_RAW_LIMIT: u64 = 256 * 1024 * 1024;
const DEFAULT_RECORD_LIMIT: u64 = 1024 * 1024;
const DEFAULT_NORMALIZED_LIMIT: u64 = 16 * 1024 * 1024;

pub(crate) struct Options {
    pub since: Option<String>,
    pub all_history: bool,
    pub strategy: String,
    pub history_config: PathBuf,
    pub execute: bool,
    pub experiment: bool,
    pub candidate: Option<PathBuf>,
    pub repo: PathBuf,
    pub state: Option<PathBuf>,
    pub json: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceConfig {
    schema: String,
    project_id: String,
    #[serde(default = "default_sessions")]
    max_sessions: u32,
    #[serde(default = "default_raw")]
    max_raw_bytes: u64,
    #[serde(default = "default_record")]
    max_record_bytes: u64,
    #[serde(default = "default_normalized")]
    max_normalized_bytes: u64,
    sources: Vec<DeclaredSource>,
}
fn default_sessions() -> u32 {
    200
}
fn default_raw() -> u64 {
    DEFAULT_RAW_LIMIT
}
fn default_record() -> u64 {
    DEFAULT_RECORD_LIMIT
}
fn default_normalized() -> u64 {
    DEFAULT_NORMALIZED_LIMIT
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredSource {
    /// `external` is the explicit normalized fixture/import contract. The other adapters parse
    /// provider-native, allowlisted records and retain no transcript body or private reasoning.
    adapter: String,
    path: PathBuf,
    source_id: String,
    execution_id: String,
    /// Explicit operator attestation for historical AF exports without project metadata.
    #[serde(default)]
    attest_project: bool,
    /// Original project location for a declared session imported from an analysis checkout.
    #[serde(default)]
    project_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizedRecord {
    observed_unix_ms: DecimalU64,
    attribution: OptimizationAttributionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<OptimizationTokenObservationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spans: Vec<OptimizationSpanV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    caches: Vec<OptimizationCacheObservationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<OptimizationOutcomeObservationV1>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    missing_fields: BTreeSet<String>,
}

fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())
        .and_then(|duration| u64::try_from(duration.as_millis()).map_err(|error| error.to_string()))
}

fn parse_since(value: Option<&str>, cutoff: u64) -> Result<Option<u64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let last = value
        .char_indices()
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0);
    let (digits, unit) = value.split_at(last);
    let count: u64 = digits
        .parse()
        .map_err(|_| "--since requires a positive duration such as 30d or 12h")?;
    if count == 0 {
        return Err("--since must be positive".into());
    }
    let multiplier: u64 = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err("--since supports m, h, or d units".into()),
    };
    Ok(Some(
        cutoff.saturating_sub(
            count
                .checked_mul(multiplier)
                .ok_or("--since duration overflow")?,
        ),
    ))
}

fn read_config(repo: &Path, requested: &Path) -> Result<(SourceConfig, PathBuf), String> {
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        repo.join(requested)
    };
    let bytes =
        std::fs::read(&path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    if bytes.len() > 1024 * 1024 {
        return Err("Optimization source configuration exceeds 1 MiB".into());
    }
    let config: SourceConfig = if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))?
    } else {
        toml::from_str(std::str::from_utf8(&bytes).map_err(|error| error.to_string())?)
            .map_err(|error| format!("{}: {error}", path.display()))?
    };
    let mut source_ids = BTreeSet::new();
    let sources_valid = config.sources.iter().all(|source| {
        matches!(
            source.adapter.as_str(),
            "af" | "codex" | "claude" | "external"
        ) && !source.path.as_os_str().is_empty()
            && source.path.as_os_str().len() <= 4096
            && !source.source_id.is_empty()
            && source.source_id.len() <= 512
            && !source.source_id.chars().any(char::is_control)
            && !source.execution_id.is_empty()
            && source.execution_id.len() <= 256
            && !source.execution_id.chars().any(char::is_control)
            && source
                .project_root
                .as_ref()
                .is_none_or(|path| path.is_absolute() && path.as_os_str().len() <= 4096)
            && (!source.attest_project || source.adapter == "af")
            && source_ids.insert(source.source_id.clone())
    });
    if config.schema != "af.optimization-sources/1"
        || !review_core::is_digest(&config.project_id)
        || config.sources.is_empty()
        || config.sources.len() > config.max_sessions as usize
        || !(1..=200).contains(&config.max_sessions)
        || !(1..=DEFAULT_RAW_LIMIT).contains(&config.max_raw_bytes)
        || !(1..=DEFAULT_RECORD_LIMIT).contains(&config.max_record_bytes)
        || !(1..=DEFAULT_NORMALIZED_LIMIT).contains(&config.max_normalized_bytes)
        || !sources_valid
    {
        return Err("Optimization source configuration violates M1 capture bounds".into());
    }
    Ok((config, path))
}

fn latest_capture(
    cas: &Cas,
    store: &EventStore,
    project_id: &str,
) -> Result<Option<(String, OptimizationHistoryV1)>, String> {
    let ids = store
        .map_tasks(cas, |task| {
            task.revision.inputs.get("history").and_then(|port| {
                (port.artifact_type == OPTIMIZATION_HISTORY_V1 && port.artifact_ids.len() == 1)
                    .then(|| port.artifact_ids[0].clone())
            })
        })
        .map_err(|error| error.to_string())?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    let mut candidates = BTreeMap::new();
    for id in ids {
        let artifact = cas
            .get_artifact(&id)
            .map_err(|error| format!("Retained history is unavailable: {error}"))?;
        if artifact.artifact_type != OPTIMIZATION_HISTORY_V1 {
            return Err("Retained history has a wrong artifact type".into());
        }
        let history: OptimizationHistoryV1 =
            serde_json::from_value(artifact.payload).map_err(|error| error.to_string())?;
        history.validate()?;
        if history.project_id == project_id {
            candidates.insert(id, history);
        }
    }
    let parents = candidates
        .values()
        .filter_map(|history| history.previous_capture_id.clone())
        .collect::<BTreeSet<_>>();
    candidates.retain(|id, _| !parents.contains(id));
    if candidates.len() > 1 {
        return Err(
            "Project history has multiple heads; explicit reconciliation is required".into(),
        );
    }
    Ok(candidates.into_iter().next())
}

fn source_path(config_path: &Path, declared: &Path) -> Result<PathBuf, String> {
    let path = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(declared)
    };
    let path = std::fs::canonicalize(&path)
        .map_err(|error| format!("opening declared source {}: {error}", path.display()))?;
    if !path.is_file() {
        return Err(format!(
            "declared history source {} is not a regular file",
            path.display()
        ));
    }
    Ok(path)
}

fn digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        review_core::hex::encode(&Sha256::digest(bytes))
    )
}

/// The legacy field `prefix_digest` hashes the exact receipted byte range. Recheck all
/// retained ranges before continuing a source; an empty last capture must not hide rotation.
fn verify_retained_ranges(
    file: &mut std::fs::File,
    source: &DeclaredSource,
    receipts: &[OptimizationSourceReceiptV1],
    remaining_raw: &mut u64,
) -> Result<(), String> {
    let mut checked = BTreeSet::new();
    let mut buffer = [0u8; 64 * 1024];
    for receipt in receipts
        .iter()
        .filter(|receipt| receipt.source_id == source.source_id)
    {
        if receipt.adapter != source.adapter || receipt.execution_id != source.execution_id {
            return Err("Declared source identity changed; use a new source identity".into());
        }
        let start = receipt.byte_start.get();
        let end = receipt.byte_end.get();
        if !checked.insert((start, end, receipt.prefix_digest.clone())) {
            continue;
        }
        let length = end
            .checked_sub(start)
            .ok_or("Retained source range is reversed")?;
        if length > *remaining_raw {
            return Err("Retained source verification exceeds the captured raw-read limit".into());
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|error| error.to_string())?;
        let mut left = length;
        let mut hash = Sha256::new();
        while left > 0 {
            let wanted = usize::try_from(left.min(buffer.len() as u64))
                .map_err(|error| error.to_string())?;
            let count = file
                .read(&mut buffer[..wanted])
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Err("Retained source range was truncated or rotated".into());
            }
            left -= count as u64;
            *remaining_raw -= count as u64;
            hash.update(&buffer[..count]);
        }
        if format!("sha256:{}", review_core::hex::encode(&hash.finalize())) != receipt.prefix_digest
        {
            return Err("Retained source bytes changed; source rotation or rewriting requires explicit new provenance".into());
        }
    }
    Ok(())
}

fn retained_receipts(
    state: &Path,
    previous: Option<&(String, OptimizationHistoryV1)>,
) -> Result<Vec<OptimizationSourceReceiptV1>, String> {
    let Some((id, history)) = previous else {
        return Ok(vec![]);
    };
    let cas = Cas::open_existing(state.join("cas")).map_err(|error| error.to_string())?;
    let mut result = history.receipts.clone();
    let mut next = history.previous_capture_id.clone();
    let mut seen = BTreeSet::from([id.clone()]);
    while let Some(id) = next {
        if seen.len() >= 1024 || !seen.insert(id.clone()) {
            return Err("Retained capture chain is cyclic or exceeds the bounded traversal".into());
        }
        let envelope = cas.get_artifact(&id).map_err(|error| error.to_string())?;
        if envelope.artifact_type != OPTIMIZATION_HISTORY_V1 {
            return Err("Retained capture chain has a wrong artifact type".into());
        }
        let prior: OptimizationHistoryV1 =
            serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
        prior.validate()?;
        if prior.project_id != history.project_id {
            return Err("Retained capture chain crosses project identities".into());
        }
        result.extend(prior.receipts);
        next = prior.previous_capture_id;
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn capture_source(
    config: &SourceConfig,
    config_path: &Path,
    project_root: &Path,
    source: &DeclaredSource,
    start: u64,
    since: Option<u64>,
    cutoff: u64,
    remaining_raw: &mut u64,
    remaining_normalized: &mut u64,
    prior_receipts: &[OptimizationSourceReceiptV1],
) -> Result<
    (
        OptimizationSourceReceiptV1,
        Vec<OptimizationObservationV1>,
        BTreeSet<String>,
    ),
    String,
> {
    if !matches!(
        source.adapter.as_str(),
        "af" | "codex" | "claude" | "external"
    ) {
        return Err(format!(
            "unsupported declared history adapter {}",
            source.adapter
        ));
    }
    let path = source_path(config_path, &source.path)?;
    let before = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    let end = before.len();
    if start > end {
        return Err(format!(
            "declared history source {} was truncated",
            source.source_id
        ));
    }
    let mut file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
    verify_retained_ranges(&mut file, source, prior_receipts, remaining_raw)?;
    let available = end - start;
    let selected = available.min(*remaining_raw);
    let mut complete = selected == available;
    let mut filtered = false;
    file.seek(SeekFrom::Start(start))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(file.take(selected));
    let mut hasher = Sha256::new();
    let mut records = Vec::new();
    let mut adapter_state = AdapterState::new(
        &source.adapter,
        &config.project_id,
        &source.execution_id,
        source.project_root.as_deref().unwrap_or(project_root),
        cutoff,
        start > 0,
    );
    adapter_state.attest_project = source.attest_project;
    let mut line = Vec::new();
    let mut consumed = 0u64;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        // A bounded read must never parse or receipt a truncated JSON record.
        if !line.ends_with(b"\n") && selected < available {
            complete = false;
            break;
        }
        if read as u64 > config.max_record_bytes {
            return Err(format!(
                "{} contains a record over the captured limit",
                source.source_id
            ));
        }
        hasher.update(&line);
        let parsed = parse_adapter_record(&mut adapter_state, &line)
            .map_err(|error| format!("{} {} record: {error}", source.source_id, source.adapter))?;
        for record in parsed {
            if record.attribution.project_id != config.project_id {
                return Err(format!(
                    "{} adapter emitted a foreign project observation",
                    source.source_id
                ));
            }
            record.attribution.validate()?;
            if since.is_none_or(|minimum| record.observed_unix_ms.get() >= minimum)
                && record.observed_unix_ms.get() <= cutoff
            {
                let encoded = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
                if encoded.len() as u64 > *remaining_normalized {
                    return Err("Normalized history exceeds the captured limit; continue with a narrower window".into());
                }
                *remaining_normalized -= encoded.len() as u64;
                records.push((start + consumed, record));
            } else {
                filtered = true;
            }
        }
        consumed = consumed
            .checked_add(read as u64)
            .ok_or("History byte offset overflow")?;
    }
    let after = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(format!(
            "declared history source {} changed during stable prefix capture",
            source.source_id
        ));
    }
    *remaining_raw -= selected;
    complete &= !filtered && !adapter_state.has_coverage_gap();
    let prefix_digest = format!("sha256:{}", review_core::hex::encode(&hasher.finalize()));
    let adapter_version = match adapter_state.version() {
        AdapterVersion::Normalized => "normalized-v1",
        AdapterVersion::Native => "native-v1",
    };
    let receipt_body = serde_json::json!({"adapter":source.adapter,"adapter_version":adapter_version,"project_id":config.project_id,"source_id":source.source_id,"execution_id":source.execution_id,"byte_start":start.to_string(),"byte_end":(start+consumed).to_string(),"prefix_digest":prefix_digest,"cutoff_unix_ms":cutoff.to_string(),"redaction_version":"allowlist-v1","completeness":if complete{"complete"}else{"partial"}});
    let receipt_id = content_id(&receipt_body).map_err(|error| error.to_string())?;
    let receipt = OptimizationSourceReceiptV1 {
        receipt_id: receipt_id.clone(),
        adapter: source.adapter.clone(),
        adapter_version: adapter_version.into(),
        project_id: config.project_id.clone(),
        source_id: source.source_id.clone(),
        execution_id: source.execution_id.clone(),
        byte_start: start.into(),
        byte_end: (start + consumed).into(),
        prefix_digest,
        cutoff_unix_ms: cutoff.into(),
        redaction_version: "allowlist-v1".into(),
        completeness: if complete {
            SourceCompletenessV1::Complete
        } else {
            SourceCompletenessV1::Partial
        },
    };
    let observations = records
        .into_iter()
        .map(|(byte_start, record)| {
            let observation_id =
                observation_identity(source, adapter_version, byte_start, &record)?;
            Ok(OptimizationObservationV1 {
                observation_id,
                source_receipt_id: receipt_id.clone(),
                attribution: record.attribution,
                tokens: record.tokens,
                spans: record.spans,
                caches: record.caches,
                outcome: record.outcome,
                missing_fields: record.missing_fields,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((receipt, observations, adapter_state.take_gaps()))
}

/// Native execution/counter/span identities are independent of which exported file carried
/// them. Receipt IDs still retain every source/range. Normalized imports have no native identity
/// guarantee, so retain their original source/range identity instead of comparing prose.
fn observation_identity(
    source: &DeclaredSource,
    version: &str,
    byte_start: u64,
    record: &NormalizedRecord,
) -> Result<String, String> {
    let mut body = serde_json::to_value(record).map_err(|error| error.to_string())?;
    let identity = if version == "native-v1" {
        body.as_object_mut()
            .ok_or("Native observation is not an object")?
            .remove("observed_unix_ms");
        serde_json::json!({"adapter":source.adapter,"native_record":body})
    } else {
        serde_json::json!({"adapter":source.adapter,"source_id":source.source_id,"byte_start":byte_start.to_string(),"record":body})
    };
    content_id(&identity).map_err(|error| error.to_string())
}

fn capture(options: &Options, repo: &Path, state: &Path) -> Result<OptimizationHistoryV1, String> {
    let (config, config_path) = read_config(repo, &options.history_config)?;
    OptimizationPolicyV1 {
        schema: "af.optimization-policy/1".into(),
        project_id: config.project_id.clone(),
        strategy: if options.strategy == "heavy" {
            OptimizationStrategyV1::Heavy
        } else {
            OptimizationStrategyV1::Light
        },
        max_sessions: config.max_sessions,
        max_raw_bytes: config.max_raw_bytes.into(),
        max_record_bytes: config.max_record_bytes.into(),
        max_normalized_bytes: config.max_normalized_bytes.into(),
    }
    .validate()?;
    let cutoff = now_ms()?;
    let since = parse_since(options.since.as_deref(), cutoff)?;
    let previous = if state.join("events.sqlite").is_file() {
        let cas = Cas::open_existing(state.join("cas")).map_err(|error| error.to_string())?;
        let store = EventStore::open_read_only(state.join("events.sqlite"))
            .map_err(|error| error.to_string())?;
        latest_capture(&cas, &store, &config.project_id)?
    } else {
        None
    };
    let prior_receipts = retained_receipts(state, previous.as_ref())?;
    let cursors: BTreeMap<_, _> = previous
        .as_ref()
        .map(|(_, history)| {
            history
                .receipts
                .iter()
                .map(|receipt| (receipt.source_id.clone(), receipt.byte_end.get()))
                .collect()
        })
        .unwrap_or_default();
    let mut remaining_raw = config.max_raw_bytes;
    let mut remaining_normalized = config.max_normalized_bytes;
    let mut receipts = Vec::new();
    let mut observations = Vec::new();
    let mut gaps = BTreeSet::new();
    for source in &config.sources {
        let start = if options.all_history || options.since.is_some() {
            0
        } else {
            cursors.get(&source.source_id).copied().unwrap_or(0)
        };
        match capture_source(
            &config,
            &config_path,
            repo,
            source,
            start,
            since,
            cutoff,
            &mut remaining_raw,
            &mut remaining_normalized,
            &prior_receipts,
        ) {
            Ok((receipt, mut source_observations, source_gaps)) => {
                if receipt.completeness != SourceCompletenessV1::Complete {
                    gaps.insert(format!("partial_{}", source.adapter));
                }
                receipts.push(receipt);
                observations.append(&mut source_observations);
                gaps.extend(source_gaps);
            }
            Err(error) => {
                let reason = if error.contains("Retained source bytes changed") {
                    "source_changed_"
                } else if error.contains("Retained source range was truncated") {
                    "source_truncated_"
                } else if error.contains("Retained source verification exceeds") {
                    "source_verification_limit_"
                } else if error.contains("Declared source identity changed") {
                    "source_identity_changed_"
                } else {
                    ""
                };
                gaps.insert(format!(
                    "unavailable_{}_{}{}",
                    source.adapter,
                    reason,
                    digest(source.source_id.as_bytes())
                        .trim_start_matches("sha256:")
                        .chars()
                        .take(12)
                        .collect::<String>()
                ));
            }
        }
    }
    // Keep every source receipt, but each exact native observation appears only once per
    // capture. The history contract rejects duplicate IDs rather than silently counting them.
    let mut observation_ids = BTreeSet::new();
    observations.retain(|observation| observation_ids.insert(observation.observation_id.clone()));
    let exposed_case_families = observations
        .iter()
        .map(|observation| observation.attribution.case_family.clone())
        .collect();
    let history = OptimizationHistoryV1 {
        schema: "af.optimization-history/1".into(),
        project_id: config.project_id,
        previous_capture_id: previous.map(|(id, _)| id),
        cutoff_unix_ms: cutoff.into(),
        receipts,
        observations,
        gaps,
        exposed_case_families,
    };
    history.validate()?;
    Ok(history)
}

fn optimization_request_digest(
    capture_digest: &str,
    experiment: bool,
    light: bool,
    candidate: &Option<review_pipeline::task::optimization_configuration::CandidateProposal>,
) -> Result<String, String> {
    content_id(&serde_json::json!([
        capture_digest,
        experiment,
        light,
        candidate
    ]))
    .map_err(|error| error.to_string())
}

pub(crate) fn run(mut options: Options) -> Result<i32, String> {
    let (repo, state) = task_execution::state_path(&options.repo, options.state.as_deref())?;
    let candidate_kind_installed = std::fs::read_to_string(repo.join(".af/task-catalog.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|catalog| {
            catalog
                .get("kinds")?
                .get("optimize")?
                .as_str()
                .map(str::to_owned)
        })
        .is_some_and(|name| name == "builtin/optimization-candidate-kind");
    let candidate = options
        .candidate
        .as_ref()
        .map(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                repo.join(path)
            };
            let mut data = Vec::new();
            std::fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() > 4 * 1024 * 1024 {
                return Err("Candidate proposal exceeds 4 MiB".into());
            }
            serde_json::from_slice::<
                review_pipeline::task::optimization_configuration::CandidateProposal,
            >(&data)
            .map_err(|e| e.to_string())
        })
        .transpose()?;
    if candidate.is_some() {
        options.experiment = true;
    }
    let light = candidate.is_none()
        && !options.experiment
        && options.strategy == "light"
        && candidate_kind_installed
        && repo
            .join(review_pipeline::task::optimization_configuration::POLICY_PATH)
            .is_file();
    let history = capture(&options, &repo, &state)?;
    let capture_digest =
        content_id(&serde_json::to_value(&history).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let request_digest =
        optimization_request_digest(&capture_digest, options.experiment, light, &candidate)?;
    let task_id = format!("optimize-{}", &request_digest[7..31]);
    let (goal, mode, facts) = if light {
        (
            "Diagnose one routine cost from bounded development evidence, propose one installed light recipe, and verify it through the protected experiment slot.",
            "candidate",
            serde_json::json!({"experimental":true,"finalize":true,"light":true}),
        )
    } else if options.experiment {
        (
            "Run the installed bounded baseline/candidate experiment and emit an evidence-derived comparison; refuse delivery until independent finalization exists.",
            "candidate",
            serde_json::json!({"experimental":true}),
        )
    } else {
        (
            "Explain project token/time economics from captured history; do not edit configuration.",
            "analyze",
            serde_json::json!({"report_only":true}),
        )
    };
    let max_attempts = if light { 84 } else { 82 };
    let mut file = serde_json::json!({
        "schema":"af.task-file/1", "task_id":task_id, "kind":"optimize",
        "goal":goal,
        "requirements":{"mode":mode,"capture_payload_digest":capture_digest},
        "strategy":options.strategy, "facts":facts,
        "limits":{"tokens":10000000,"max_attempts":max_attempts,"wall_ms":28800000,
            "verification":{"tokens":9000000,"attempts":81,"wall_ms":25200000}}
    });
    if let Some(candidate) = candidate {
        file["goal"] = serde_json::json!(
            "Verify one concrete configuration improvement with protected checks and independent evaluation."
        );
        file["requirements"]["candidate"] =
            serde_json::to_value(candidate).map_err(|e| e.to_string())?;
        file["pipeline"] =
            serde_json::json!({"name":"builtin/optimization-controlled","fallback":"refuse"});
        file["facts"]["finalize"] = serde_json::json!(true);
    } else if light {
        file["pipeline"] =
            serde_json::json!({"name":"builtin/optimization-light","fallback":"refuse"});
    }
    let mut temporary = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut temporary, &file).map_err(|error| error.to_string())?;
    temporary.flush().map_err(|error| error.to_string())?;
    task_execution::start(task_execution::StartOptions {
        file: temporary.path().to_path_buf(),
        bindings: None,
        source_bindings: None,
        repo,
        state: Some(state),
        authority: "HEAD".into(),
        uncommitted: false,
        json: options.json,
        plan_only: !options.execute,
        timeout_secs: None,
        optimization_history: Some(history),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_report_requests_for_one_capture_have_distinct_task_ids() {
        let capture = format!("sha256:{}", "a".repeat(64));
        assert_ne!(
            optimization_request_digest(&capture, false, false, &None).unwrap(),
            optimization_request_digest(&capture, false, true, &None).unwrap()
        );
    }

    #[test]
    fn native_exports_deduplicate_observations_across_source_names_and_snapshot_times() {
        let source = |name: &str| {
            serde_json::from_value::<DeclaredSource>(serde_json::json!({"adapter":"af","path":"receipt.json","source_id":name,"execution_id":"task"})).unwrap()
        };
        let record = |time: &str| {
            serde_json::from_value::<NormalizedRecord>(serde_json::json!({"observed_unix_ms":time,"attribution":{"project_id":format!("sha256:{}","1".repeat(64)),"case_family":"af-task","execution_id":"task","task_id":"task"},"outcome":{"outcome":"incomplete","retries":0,"repairs":0,"later_defects":0}})).unwrap()
        };
        let a = observation_identity(&source("first"), "native-v1", 0, &record("10")).unwrap();
        let b = observation_identity(&source("copy"), "native-v1", 100, &record("20")).unwrap();
        assert_eq!(a, b);
        assert_ne!(
            observation_identity(&source("first"), "normalized-v1", 0, &record("10")).unwrap(),
            observation_identity(&source("copy"), "normalized-v1", 100, &record("20")).unwrap()
        );
    }

    #[test]
    fn duplicate_af_exports_keep_both_receipts_but_count_one_incomplete_execution() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = serde_json::json!({"schema":"af/task-inspection@11","task_id":"task","chargeable_tokens":"12","history":[{"transition":{"now_unix_ms":1,"change":{"kind":"opened"}}}],"execution_records":[{"record":{"kind":"settled","attempt_id":"a","charged_tokens":"12"}}]});
        std::fs::write(dir.path().join("receipt.json"), receipt.to_string() + "\n").unwrap();
        let project = format!("sha256:{}", "1".repeat(64));
        let config = serde_json::json!({"schema":"af.optimization-sources/1","project_id":project,"sources":[{"adapter":"af","path":"receipt.json","source_id":"first-export","execution_id":"task","attest_project":true},{"adapter":"af","path":"receipt.json","source_id":"duplicate-export","execution_id":"task","attest_project":true}]});
        std::fs::write(dir.path().join("sources.json"), config.to_string()).unwrap();
        let state = dir.path().join("state");
        let options = Options {
            since: None,
            all_history: false,
            strategy: "light".into(),
            history_config: PathBuf::from("sources.json"),
            execute: false,
            experiment: false,
            candidate: None,
            repo: dir.path().to_path_buf(),
            state: Some(state.clone()),
            json: true,
        };
        let history = capture(&options, dir.path(), &state).unwrap();
        assert_eq!(history.receipts.len(), 2);
        assert_eq!(history.observations.len(), 2);
        let economics = review_store::optimization::project_economics(&[(
            format!("sha256:{}", "2".repeat(64)),
            history,
        )])
        .unwrap();
        assert_eq!(economics.af_usage.chargeable_tokens.get(), 12);
        assert_eq!(economics.failed_or_incomplete, 1);
        assert_eq!(economics.repeated_failures, 0);
        assert_eq!(economics.rows[0].occurrences, 1);
    }

    #[test]
    fn retained_range_check_detects_rewrite_even_after_an_empty_capture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, b"old\n").unwrap();
        let source:DeclaredSource=serde_json::from_value(serde_json::json!({"adapter":"external","path":"history.jsonl","source_id":"source","execution_id":"run"})).unwrap();
        let receipt = OptimizationSourceReceiptV1 {
            receipt_id: format!("sha256:{}", "1".repeat(64)),
            adapter: "external".into(),
            adapter_version: "normalized-v1".into(),
            project_id: format!("sha256:{}", "2".repeat(64)),
            source_id: "source".into(),
            execution_id: "run".into(),
            byte_start: 0.into(),
            byte_end: 4.into(),
            prefix_digest: digest(b"old\n"),
            cutoff_unix_ms: 1.into(),
            redaction_version: "allowlist-v1".into(),
            completeness: SourceCompletenessV1::Complete,
        };
        let mut empty = receipt.clone();
        empty.byte_start = 4.into();
        empty.prefix_digest = digest(b"");
        let receipts = vec![empty, receipt];
        let mut file = std::fs::File::open(&path).unwrap();
        let mut budget = 8;
        verify_retained_ranges(&mut file, &source, &receipts, &mut budget).unwrap();
        assert_eq!(budget, 4);
        std::fs::write(&path, b"new\n").unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        assert!(
            verify_retained_ranges(&mut file, &source, &receipts, &mut 8)
                .unwrap_err()
                .contains("changed")
        );
        assert!(
            verify_retained_ranges(&mut file, &source, &receipts, &mut 3)
                .unwrap_err()
                .contains("limit")
        );
    }

    #[test]
    fn duration_parser_is_bounded_and_uses_captured_cutoff() {
        assert_eq!(
            parse_since(Some("2h"), 10_000_000).unwrap(),
            Some(2_800_000)
        );
        assert!(parse_since(Some("0d"), 1).is_err());
        assert!(parse_since(Some("1y"), 1).is_err());
    }

    #[test]
    fn normalized_records_reject_unknown_transcript_and_reasoning_fields() {
        let value = serde_json::json!({"observed_unix_ms":"1","attribution":{"project_id":format!("sha256:{}","1".repeat(64)),"case_family":"x","execution_id":"x"},"private_reasoning":"do not retain"});
        assert!(serde_json::from_value::<NormalizedRecord>(value).is_err());
    }
    fn capture_fixture(
        raw_limit: Option<u64>,
        since: Option<u64>,
        cutoff: u64,
    ) -> (
        OptimizationSourceReceiptV1,
        Vec<OptimizationObservationV1>,
        usize,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let project = format!("sha256:{}", "1".repeat(64));
        let record = |time: &str| {
            serde_json::json!({"observed_unix_ms":time,
            "attribution":{"project_id":project,"case_family":"fixture","execution_id":"run"},
            "tokens":{"cumulative_key":"attempt","usage":{"chargeable_tokens":"10"},"status":"exact","outer_session":false}}).to_string()+"\n"
        };
        let first = record("1");
        let log = first.clone() + &record("10");
        std::fs::write(dir.path().join("history.jsonl"), &log).unwrap();
        let config: SourceConfig = serde_json::from_value(serde_json::json!({
            "schema":"af.optimization-sources/1","project_id":project,
            "sources":[{"adapter":"external","path":"history.jsonl","source_id":"fixture","execution_id":"session"}]
        })).unwrap();
        let mut raw = raw_limit.unwrap_or(log.len() as u64);
        let mut normalized = 100_000;
        let (receipt, observations, _) = capture_source(
            &config,
            &dir.path().join("sources.json"),
            dir.path(),
            &config.sources[0],
            0,
            since,
            cutoff,
            &mut raw,
            &mut normalized,
            &[],
        )
        .unwrap();
        (receipt, observations, first.len())
    }

    #[test]
    fn byte_limit_returns_a_complete_record_prefix() {
        let (_, _, first) = capture_fixture(None, None, 100);
        let (receipt, observations, _) = capture_fixture(Some(first as u64 + 3), None, 100);
        assert_eq!(receipt.completeness, SourceCompletenessV1::Partial);
        assert_eq!(receipt.byte_end.get(), first as u64);
        assert_eq!(observations.len(), 1);
        let (empty, observations, _) = capture_fixture(Some(3), None, 100);
        assert_eq!(empty.byte_end.get(), 0);
        assert_eq!(empty.completeness, SourceCompletenessV1::Partial);
        assert!(observations.is_empty());
    }

    #[test]
    fn time_filtered_receipts_are_visibly_partial() {
        for (since, cutoff) in [(Some(5), 100), (None, 5)] {
            let (receipt, observations, _) = capture_fixture(None, since, cutoff);
            assert_eq!(receipt.completeness, SourceCompletenessV1::Partial);
            assert_eq!(observations.len(), 1);
        }
    }

    #[test]
    fn duration_parser_rejects_unicode_without_panicking() {
        for value in ["30é", "é", "", "30💥"] {
            assert!(parse_since(Some(value), 100).is_err());
        }
    }

    #[test]
    fn native_fixtures_redact_and_deduplicate_provider_counters() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("fixtures/self-optimizer/native");
        let project = format!("sha256:{}", "1".repeat(64));
        let config: SourceConfig = serde_json::from_value(serde_json::json!({
            "schema":"af.optimization-sources/1","project_id":project,
            "sources":[
                {"adapter":"codex","path":"codex-session.jsonl","source_id":"codex-session","execution_id":"codex-source"},
                {"adapter":"claude","path":"claude-session.jsonl","source_id":"claude-session","execution_id":"claude-source"}
            ]
        }))
        .unwrap();
        let config_path = root.join("sources.json");
        let mut raw = 1_000_000;
        let mut normalized = 1_000_000;
        let mut receipts = Vec::new();
        let mut observations = Vec::new();
        let mut gaps = BTreeSet::new();
        for source in &config.sources {
            let (receipt, mut records, source_gaps) = capture_source(
                &config,
                &config_path,
                &root,
                source,
                0,
                None,
                1_000,
                &mut raw,
                &mut normalized,
                &[],
            )
            .unwrap();
            assert_eq!(receipt.adapter_version, "native-v1");
            receipts.push(receipt);
            observations.append(&mut records);
            gaps.extend(source_gaps);
        }
        let encoded = serde_json::to_string(&observations).unwrap();
        assert!(!encoded.contains("chain of thought"));
        assert!(!encoded.contains("private reasoning"));
        assert!(!encoded.contains("visible response"));
        assert!(gaps.contains("excluded_foreign_project"));

        let capture_id = format!("sha256:{}", "8".repeat(64));
        let history = OptimizationHistoryV1 {
            schema: "af.optimization-history/1".into(),
            project_id: project,
            previous_capture_id: None,
            cutoff_unix_ms: 1_000.into(),
            receipts,
            observations,
            gaps,
            exposed_case_families: BTreeSet::from([
                "codex-session".into(),
                "claude-session".into(),
            ]),
        };
        history.validate().unwrap();
        let economics =
            review_store::optimization::project_economics(&[(capture_id, history)]).unwrap();
        // Codex's two cumulative snapshots become 100, not 85 + 100; the duplicate Claude
        // message remains 15, not 30. Outer sessions stay separate from AF charges.
        assert_eq!(economics.af_usage.chargeable_tokens.get(), 0);
        assert_eq!(economics.outer_session_usage.chargeable_tokens.get(), 115);
        assert_eq!(economics.cache_economics["provider_prompt"].hits, 1);
        assert_eq!(
            economics
                .rows
                .iter()
                .find(|row| row.execution_id == "codex-source")
                .unwrap()
                .context_tokens
                .unwrap()
                .get(),
            120
        );
        // Codex 120 plus the deduplicated Claude message's 10.
        assert_eq!(economics.context_tokens.unwrap().get(), 130);
        assert!(economics.missing_fields.contains("reasoning_tokens"));
    }

    #[test]
    fn native_cursor_append_and_empty_replay_do_not_recount_cumulative_usage() {
        let dir = tempfile::tempdir().unwrap();
        let project = format!("sha256:{}", "1".repeat(64));
        let path = dir.path().join("codex.jsonl");
        let line = |input: u64, cached: u64, output: u64, at: u64| {
            serde_json::json!({"timestamp_unix_ms":at.to_string(),"type":"event_msg",
                "payload":{"type":"token_count","info":{"total_token_usage":{
                    "input_tokens":input,"cached_input_tokens":cached,"output_tokens":output}}}})
            .to_string()
                + "\n"
        };
        let initial = serde_json::json!({"timestamp_unix_ms":"1","type":"session_meta",
            "payload":{"id":"session","project_id":project}})
        .to_string()
            + "\n"
            + &line(100, 20, 5, 2);
        std::fs::write(&path, initial).unwrap();
        let config: SourceConfig = serde_json::from_value(serde_json::json!({
            "schema":"af.optimization-sources/1","project_id":project,
            "sources":[{"adapter":"codex","path":"codex.jsonl","source_id":"codex","execution_id":"declared-execution"}]
        }))
        .unwrap();
        let mut raw = 1_000_000;
        let mut normalized = 1_000_000;
        let (first_receipt, first, _) = capture_source(
            &config,
            &dir.path().join("sources.json"),
            dir.path(),
            &config.sources[0],
            0,
            None,
            100,
            &mut raw,
            &mut normalized,
            &[],
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(line(120, 30, 10, 3).as_bytes())
            .unwrap();
        let mut raw = 1_000_000;
        let mut normalized = 1_000_000;
        let (second_receipt, second, _) = capture_source(
            &config,
            &dir.path().join("sources.json"),
            dir.path(),
            &config.sources[0],
            first_receipt.byte_end.get(),
            None,
            100,
            &mut raw,
            &mut normalized,
            std::slice::from_ref(&first_receipt),
        )
        .unwrap();
        let mut raw = 1_000_000;
        let mut normalized = 1_000_000;
        let (third_receipt, third, _) = capture_source(
            &config,
            &dir.path().join("sources.json"),
            dir.path(),
            &config.sources[0],
            second_receipt.byte_end.get(),
            None,
            100,
            &mut raw,
            &mut normalized,
            &[first_receipt.clone(), second_receipt.clone()],
        )
        .unwrap();
        assert!(third.is_empty());
        let ids = [
            format!("sha256:{}", "7".repeat(64)),
            format!("sha256:{}", "8".repeat(64)),
            format!("sha256:{}", "9".repeat(64)),
        ];
        let history = |previous: Option<String>, receipt, observations| OptimizationHistoryV1 {
            schema: "af.optimization-history/1".into(),
            project_id: project.clone(),
            previous_capture_id: previous,
            cutoff_unix_ms: 100.into(),
            receipts: vec![receipt],
            observations,
            gaps: BTreeSet::new(),
            exposed_case_families: BTreeSet::from(["codex-session".into()]),
        };
        let captures = vec![
            (ids[0].clone(), history(None, first_receipt, first)),
            (
                ids[1].clone(),
                history(Some(ids[0].clone()), second_receipt, second),
            ),
            (
                ids[2].clone(),
                history(Some(ids[1].clone()), third_receipt, third),
            ),
        ];
        let economics = review_store::optimization::project_economics(&captures).unwrap();
        assert_eq!(economics.outer_session_usage.chargeable_tokens.get(), 100);
        assert_eq!(economics.capture_ids, ids);
    }
}
