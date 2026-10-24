//! `af provider models`: each Provider's model catalog, read token-free from its harness's own
//! local protocol (ADR-0148).
//!
//! Claude Code answers the `list_models` and `get_binary_version` control requests of a
//! `claude -p --input-format stream-json --output-format stream-json --verbose` session before
//! any turn; af writes both requests, reads both answers and closes the session, so no turn
//! starts. Codex answers `model/list` on a bare `codex app-server` after `initialize` and
//! `initialized` (ADR-0146), and caches the catalog in `$CODEX_HOME/models_cache.json`, whose
//! `fetched_at` is the catalog's freshness when af can read it. Both probes run under the
//! Provider's auth directory, in the private probe directory and the cleared environment `af
//! provider status` uses, bounded by its timeout and cancellable; neither reads a credential.
//!
//! A catalog says which spellings af recognises for a Provider: what its harness offers (Claude
//! Code's picker leaves out exact IDs its API still accepts), never every ID the API accepts,
//! and never whether this account can run one, which only the charged `af provider doctor`
//! proves. The rules af applies to a binding live in `review_runner_claude::models`; this module
//! only reads catalogs and prints them.

use super::installation::{VersionCheck, version_check};
use super::*;
use review_runner_claude::hint;
use review_runner_claude::models::{self as rules, Catalog, CatalogModel, Kind, Resolution};

/// The source a Claude catalog is read from.
pub(crate) const CLAUDE_SOURCE: &str = "claude list_models";
/// The source a Codex catalog is read from.
pub(crate) const CODEX_SOURCE: &str = "codex app-server model/list";

/// What a listed catalog proves, printed under every one.
pub(crate) const PROOF: &str = "af recognises these spellings for this Provider: they are what \
its harness offers, not every ID its API accepts; whether this account can run one is proven \
only by the charged `af provider doctor`";

/// The arguments of the Claude catalog session: the stream-json session that accepts control
/// requests before any turn.
const CLAUDE_CATALOG_ARGS: [&str; 6] = [
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
];

const MODELS_REQUEST: &str = "af-list-models";
const VERSION_REQUEST: &str = "af-binary-version";

/// The most models a catalog keeps; the rest are dropped and the catalog says so.
const MAX_CATALOG_MODELS: usize = 64;
/// The largest `models_cache.json` af reads for its `fetched_at`.
const MAX_MODELS_CACHE_BYTES: u64 = 8 * 1024 * 1024;
/// How long a closed catalog session may take to exit before af stops it.
const SESSION_EXIT_GRACE: Duration = Duration::from_secs(1);

/// One Provider's catalog, as its harness answered.
#[derive(Clone, Debug)]
pub(crate) struct CatalogReport {
    pub(crate) source: &'static str,
    /// The harness's own version, when it reported one in grammar G.
    pub(crate) version: Option<String>,
    /// `fetched_at` of the Codex catalog cache, when it was readable; otherwise the catalog is
    /// live.
    pub(crate) fetched_at: Option<String>,
    pub(crate) catalog: Catalog,
    /// Entries the harness listed that af left out: an ID outside grammar G, or past the bound.
    pub(crate) skipped: usize,
}

impl CatalogReport {
    pub(crate) fn freshness(&self) -> String {
        match &self.fetched_at {
            Some(at) => format!("models_cache.json fetched_at {at}"),
            None => "live".to_owned(),
        }
    }

    /// The header line of decision 1: source, harness version, freshness.
    pub(crate) fn header(&self) -> String {
        format!(
            "source {}  version {}  freshness {}",
            self.source,
            self.version.as_deref().unwrap_or("not reported"),
            self.freshness()
        )
    }
}

/// What reading one Provider's catalog concluded.
#[derive(Clone, Debug)]
pub(crate) enum CatalogOutcome {
    Listed(CatalogReport),
    /// The Provider CLI cannot start or does not accept af's invocation (exit 4).
    CliRefused(String),
    /// The harness did not answer; nothing is invented and the static rules stand (exit 7).
    Unavailable(String),
}

impl CatalogOutcome {
    pub(crate) fn report(&self) -> Option<&CatalogReport> {
        match self {
            CatalogOutcome::Listed(report) => Some(report),
            _ => None,
        }
    }
}

/// One Provider and its catalog.
#[derive(Clone, Debug)]
pub(crate) struct ProviderCatalog {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    pub(crate) outcome: CatalogOutcome,
}

fn rule_kind(kind: ProviderKind) -> Kind {
    match kind {
        ProviderKind::Claude => Kind::Claude,
        ProviderKind::Codex => Kind::Codex,
    }
}

/// The catalog of one registered Provider, without the capability check: what plan time reads,
/// once per Provider, and treats as unverified when the harness cannot answer.
pub(crate) fn catalog_for(provider_id: &str) -> Option<CatalogReport> {
    let spec = configured_spec(provider_id).ok()?;
    let cancelled = AtomicBool::new(false);
    match probe_spec(&spec, &cancelled, false) {
        CatalogOutcome::Listed(report) => Some(report),
        _ => None,
    }
}

/// The catalogs of the listed Provider IDs, in chunks as the status discovery probes, and stopped
/// by `cancelled`: what the TUI's discovery job reads after the inventory, whose discovery has
/// just run the capability check on each of them.
pub(crate) fn catalogs_with_cancel(ids: &[String], cancelled: &AtomicBool) -> Vec<ProviderCatalog> {
    let (specs, _, _) = load_specs();
    let specs: Vec<ProviderSpec> = specs
        .into_iter()
        .filter(|spec| ids.contains(&spec.id))
        .collect();
    probe_all(&specs, cancelled, false)
}

fn probe_all(
    specs: &[ProviderSpec],
    cancelled: &AtomicBool,
    check_cli: bool,
) -> Vec<ProviderCatalog> {
    let mut catalogs = Vec::with_capacity(specs.len());
    for chunk in specs.chunks(MAX_CONCURRENT_PROBES) {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|spec| (spec, scope.spawn(|| probe_spec(spec, cancelled, check_cli))))
                .collect();
            catalogs.extend(handles.into_iter().map(|(spec, handle)| ProviderCatalog {
                id: spec.id.clone(),
                kind: rule_kind(spec.kind),
                outcome: handle.join().unwrap_or_else(|_| {
                    CatalogOutcome::Unavailable("the catalog probe panicked".into())
                }),
            }));
        });
    }
    catalogs
}

/// Read one Provider's catalog: the CLI is located, its invocation checked when `check_cli`
/// asks, then its harness asked.
fn probe_spec(spec: &ProviderSpec, cancelled: &AtomicBool, check_cli: bool) -> CatalogOutcome {
    if let Err(error) = availability::check(spec) {
        return CatalogOutcome::Unavailable(error);
    }
    let program = match locate_cli(spec) {
        Ok(program) => program,
        Err(failure) => return CatalogOutcome::CliRefused(failure.message()),
    };
    let probe_path = sanitized_path();
    if check_cli {
        match check_invocation(&program, spec, &probe_path, cancelled, None) {
            Ok(()) => {}
            Err(CliRefusal::Unchecked(reason)) => return CatalogOutcome::Unavailable(reason),
            Err(refusal) => return CatalogOutcome::CliRefused(refusal.message()),
        }
    }
    let listed = match spec.kind {
        ProviderKind::Claude => claude_catalog(&program, spec, &probe_path, cancelled),
        ProviderKind::Codex => codex_catalog(&program, spec, &probe_path, cancelled),
    };
    match listed {
        Ok(report) => CatalogOutcome::Listed(report),
        Err(reason) => CatalogOutcome::Unavailable(reason),
    }
}

/// The Codex catalog: `model/list` on a bare app-server, the CLI's version, and the cache's
/// `fetched_at`.
fn codex_catalog(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) -> Result<CatalogReport, String> {
    let mut observed = AppServerProbe::default();
    let request = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"model/list","params":{}});
    let response = probe_codex_request_observed(
        program,
        spec,
        probe_path,
        cancelled,
        Some(&request),
        None,
        &mut observed,
    )?;
    if response.get("error").is_some() {
        return Err("codex app-server answered `model/list` with an error".into());
    }
    let (catalog, skipped) = parse_codex_model_list(&response["result"])?;
    let version = match version_check(program, spec, probe_path, cancelled, None) {
        VersionCheck::Reported(Some(line)) => version_word(&line),
        _ => None,
    };
    Ok(CatalogReport {
        source: CODEX_SOURCE,
        version,
        fetched_at: codex_home_of(spec).and_then(|home| cache_fetched_at(&home)),
        catalog,
        skipped,
    })
}

/// The Codex home a probe of `spec` runs against: the one it is handed as `CODEX_HOME`,
/// otherwise the CLI's default below `HOME`.
fn codex_home_of(spec: &ProviderSpec) -> Option<PathBuf> {
    if spec.explicit_selector {
        return spec.auth_dir.clone();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".codex"))
}

/// `fetched_at` of `models_cache.json` in `home`, when it is a bounded regular file whose value
/// af may print.
fn cache_fetched_at(home: &Path) -> Option<String> {
    let path = home.join("models_cache.json");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_MODELS_CACHE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_MODELS_CACHE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let at = value.get("fetched_at")?.as_str()?;
    (!at.is_empty()
        && at.len() <= 64
        && at
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-:.+".contains(&b)))
    .then(|| at.to_owned())
}

/// The version word of a CLI's `--version` line (`codex-cli 0.162.1` is `0.162.1`).
fn version_word(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|word| hint::printable(word))
        .map(str::to_owned)
}

/// The catalog of a `model/list` result: one row per listed ID in grammar G, with its display
/// name, effort levels and default.
pub(crate) fn parse_codex_model_list(
    result: &serde_json::Value,
) -> Result<(Catalog, usize), String> {
    let data = result
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("codex app-server answered `model/list` without a model list")?;
    let mut catalog = Catalog::default();
    let mut skipped = 0;
    for entry in data {
        let Some(id) = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| hint::printable(id))
        else {
            skipped += 1;
            continue;
        };
        if catalog.model(id).is_some() {
            continue;
        }
        if catalog.models.len() == MAX_CATALOG_MODELS {
            skipped += 1;
            continue;
        }
        let efforts = levels(
            entry
                .get("supportedReasoningEfforts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|level| level.get("reasoningEffort")),
        );
        let display = entry
            .get("displayName")
            .and_then(serde_json::Value::as_str)
            .map(|name| safe_display(name, id))
            .unwrap_or_else(|| id.to_owned());
        catalog.models.push(CatalogModel {
            id: id.to_owned(),
            display_name: display,
            other_names: Vec::new(),
            aliases: Vec::new(),
            default_effort: entry
                .get("defaultReasoningEffort")
                .and_then(serde_json::Value::as_str)
                .filter(|level| efforts.iter().any(|listed| listed == level))
                .map(str::to_owned),
            efforts,
            hidden: entry.get("hidden").and_then(serde_json::Value::as_bool) == Some(true),
        });
    }
    Ok((catalog, skipped))
}

/// The catalog of a `list_models` answer: one row per exact ID (`resolvedModel`), whose aliases
/// are the entries whose `value` resolves to it, named by its description's own family and
/// version (`Fable 5.1 · …` is `Fable 5.1`), with its effort levels; the aliases Claude Code
/// documents are added where the picker leaves them out.
pub(crate) fn parse_claude_list_models(
    response: &serde_json::Value,
) -> Result<(Catalog, usize), String> {
    let entries = response
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or("claude answered `list_models` without a model list")?;
    let mut catalog = Catalog::default();
    let mut skipped = 0;
    for entry in entries {
        let text = |key: &str| entry.get(key).and_then(serde_json::Value::as_str);
        let Some(value) = text("value").filter(|value| hint::printable(value)) else {
            skipped += 1;
            continue;
        };
        let Some(id) = text("resolvedModel")
            .or(Some(value))
            .filter(|id| hint::printable(id))
        else {
            skipped += 1;
            continue;
        };
        let efforts = if entry
            .get("supportsEffort")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            Vec::new()
        } else {
            levels(
                entry
                    .get("supportedEffortLevels")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten(),
            )
        };
        let shown_name = text("displayName").map(|name| safe_display(name, id));
        let family = text("description")
            .and_then(|description| description.split(" · ").next())
            .map(str::trim)
            .filter(|name| name.contains(|c: char| c.is_ascii_digit()) && name.len() <= 48)
            .map(|name| safe_display(name, id));
        let index = match catalog.models.iter().position(|model| model.id == id) {
            Some(index) => index,
            None if catalog.models.len() == MAX_CATALOG_MODELS => {
                skipped += 1;
                continue;
            }
            None => {
                catalog.models.push(CatalogModel {
                    id: id.to_owned(),
                    display_name: String::new(),
                    other_names: Vec::new(),
                    aliases: Vec::new(),
                    efforts: Vec::new(),
                    default_effort: None,
                    hidden: false,
                });
                catalog.models.len() - 1
            }
        };
        let model = &mut catalog.models[index];
        if value != id && !model.aliases.iter().any(|alias| alias == value) {
            model.aliases.push(value.to_owned());
        }
        // The exact entry's own words win; an alias entry fills what is still missing.
        let exact = value == id;
        if let Some(family) = family
            && (exact || model.display_name.is_empty())
        {
            if !model.display_name.is_empty() && model.display_name != family {
                model
                    .other_names
                    .push(std::mem::take(&mut model.display_name));
            }
            model.display_name = family;
        }
        if let Some(name) = shown_name
            && name != model.display_name
            && !model.other_names.contains(&name)
        {
            model.other_names.push(name);
        }
        if (exact || model.efforts.is_empty()) && !efforts.is_empty() {
            model.efforts = efforts;
        }
    }
    for model in &mut catalog.models {
        if model.display_name.is_empty() {
            model.display_name = match model.other_names.first() {
                Some(_) => model.other_names.remove(0),
                None => model.id.clone(),
            };
        }
    }
    Ok((catalog.with_documented_aliases(Kind::Claude), skipped))
}

/// Effort levels in grammar G, in the harness's order, each once.
fn levels<'a>(values: impl Iterator<Item = &'a serde_json::Value>) -> Vec<String> {
    let mut levels: Vec<String> = Vec::new();
    for level in values.filter_map(serde_json::Value::as_str) {
        if hint::printable(level) && !levels.iter().any(|listed| listed == level) {
            levels.push(level.to_owned());
        }
    }
    levels
}

/// The Claude catalog: `list_models` and `get_binary_version` on a stream-json session that is
/// closed once both are answered, before any turn.
fn claude_catalog(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) -> Result<CatalogReport, String> {
    // Removed, with what the CLI kept for it, once the session has exited (ADR-0144).
    availability::check(spec)?;
    let directory = ProbeDirectory::new(spec)?;
    let mut command = Command::new(program);
    command.args(CLAUDE_CATALOG_ARGS);
    configure_probe_environment(&mut command, spec, probe_path, directory.path());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = review_process::spawn(&mut command)
        .map_err(|error| format!("cannot start the Claude catalog session: {error}"))?;
    let mut watch = review_process::ExitWatch::new(child.id());
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("catalog stdout was piped");
    let mut stderr = child.stderr.take().expect("catalog stderr was piped");
    if let Err(error) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        stop_probe(&mut child);
        return Err(error);
    }
    let requests = format!(
        "{}\n{}\n",
        serde_json::json!({"type":"control_request","request_id":MODELS_REQUEST,"request":{"subtype":"list_models"}}),
        serde_json::json!({"type":"control_request","request_id":VERSION_REQUEST,"request":{"subtype":"get_binary_version"}}),
    );
    // A CLI that already exited closes stdin; its exit is reported below, in its own words.
    let _ = stdin.as_mut().map(|pipe| {
        pipe.write_all(requests.as_bytes())
            .and_then(|()| pipe.flush())
    });
    let timeout = probe_timeout(PROBE_TIMEOUT);
    let deadline = Instant::now() + timeout;
    let mut captured = Vec::new();
    let mut said = Vec::new();
    let (mut exceeded, mut said_exceeded) = (false, false);
    let mut reaped = false;
    let answered = loop {
        let drained = loop {
            match drain_available(&mut stdout, &mut captured, &mut exceeded) {
                Ok(true) if !exceeded => {}
                Ok(_) => break Ok(()),
                Err(error) => break Err(error),
            }
        };
        if let Err(error) = drained {
            break Err(error);
        }
        let _ = drain_available(&mut stderr, &mut said, &mut said_exceeded);
        if exceeded {
            break Err(format!(
                "the Claude catalog session's output exceeds {MAX_PROBE_OUTPUT} bytes"
            ));
        }
        let models = control_response(&captured, MODELS_REQUEST);
        let version = control_response(&captured, VERSION_REQUEST);
        match (&models, &version) {
            (Some(Err(error)), _) => break Err(error.clone()),
            (Some(Ok(_)), Some(_)) => break Ok((models, version)),
            _ => {}
        }
        match review_process::try_reap_killing_group(&mut child, &mut watch) {
            Ok(Some(status)) => {
                reaped = true;
                while let Ok(true) = drain_available(&mut stdout, &mut captured, &mut exceeded) {}
                let _ = drain_available(&mut stderr, &mut said, &mut said_exceeded);
                let models = control_response(&captured, MODELS_REQUEST);
                if matches!(models, Some(Ok(_))) {
                    break Ok((models, control_response(&captured, VERSION_REQUEST)));
                }
                if let Some(Err(error)) = models {
                    break Err(error);
                }
                let (line, _) = review_runner::cli::error_and_fix(&said, &captured);
                let line = line.map(|line| format!(": {line}")).unwrap_or_default();
                break Err(format!(
                    "claude {} before answering `list_models`{line}",
                    review_runner::cli::exit_description(&status)
                ));
            }
            Ok(None) => {}
            Err(error) => break Err(format!("the Claude catalog session failed: {error}")),
        }
        if cancelled.load(Ordering::Acquire) {
            break Err("the catalog probe was cancelled".into());
        }
        if Instant::now() >= deadline {
            // A catalog without a version is still the catalog.
            if matches!(models, Some(Ok(_))) {
                break Ok((models, version));
            }
            break Err(format!(
                "claude did not answer `list_models` within {} seconds",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    // Closing stdin ends the session without a turn; one that does not exit is stopped.
    drop(stdin.take());
    if !reaped {
        let grace = Instant::now() + SESSION_EXIT_GRACE;
        loop {
            match review_process::try_reap_killing_group(&mut child, &mut watch) {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < grace => thread::sleep(Duration::from_millis(10)),
                Ok(None) | Err(_) => {
                    stop_probe(&mut child);
                    break;
                }
            }
        }
    }
    let (models, version) = answered?;
    let models = models
        .and_then(Result::ok)
        .ok_or("claude did not answer `list_models`")?;
    let (catalog, skipped) = parse_claude_list_models(&models)?;
    let version = version
        .and_then(Result::ok)
        .and_then(|answer| {
            answer
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .filter(|version| hint::printable(version));
    Ok(CatalogReport {
        source: CLAUDE_SOURCE,
        version,
        fetched_at: None,
        catalog,
        skipped,
    })
}

/// The answer to the control request `id` among the session's complete stdout lines: its
/// `response` on success, or af's words for an error answer.
fn control_response(captured: &[u8], id: &str) -> Option<Result<serde_json::Value, String>> {
    complete_lines(captured).find_map(|(_, line)| {
        let message: serde_json::Value = serde_json::from_slice(line).ok()?;
        if message.get("type").and_then(serde_json::Value::as_str) != Some("control_response") {
            return None;
        }
        let response = message.get("response")?;
        if response
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            != Some(id)
        {
            return None;
        }
        let subtype = if id == MODELS_REQUEST {
            "list_models"
        } else {
            "get_binary_version"
        };
        Some(
            match response.get("subtype").and_then(serde_json::Value::as_str) {
                Some("success") => Ok(response
                    .get("response")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)),
                _ => Err(format!(
                    "claude answered `{subtype}` with an error: {}",
                    response
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(review_runner::cli::bounded_cli_line)
                        .unwrap_or_else(|| "no message".into())
                )),
            },
        )
    })
}

// ------------------------------------------------------------------------------------------
// The command

/// A `--resolve SPELLING[/EFFORT]` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolveRequest {
    pub(crate) spelling: String,
    pub(crate) effort: Option<String>,
}

/// Read `SPELLING[/EFFORT]`: the spelling holds letters, digits, spaces, `.`, `-`, `_` or `:`;
/// the effort level is in grammar G.
pub(crate) fn parse_resolve(value: &str) -> Result<ResolveRequest, String> {
    let (spelling, effort) = match value.rsplit_once('/') {
        Some((spelling, effort)) => (spelling.trim(), Some(effort.trim())),
        None => (value.trim(), None),
    };
    if spelling.is_empty()
        || spelling.len() > 128
        || !spelling
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ' || "-._:".contains(c))
    {
        return Err(
            "--resolve takes SPELLING[/EFFORT]: a spelling of at most 128 ASCII letters, digits, \
             spaces, '.', '-', '_' or ':', such as `Fable 5.1/xhigh`"
                .into(),
        );
    }
    if let Some(effort) = effort
        && !hint::printable(effort)
    {
        return Err(format!(
            "--resolve effort level ({} bytes) is not a spelling af accepts: {}",
            effort.len(),
            hint::PRINTABLE_RULE
        ));
    }
    Ok(ResolveRequest {
        spelling: spelling.to_owned(),
        effort: effort.map(str::to_owned),
    })
}

/// `af provider models [ID …] [--json] [--resolve SPELLING[/EFFORT]]`. Returns the documented
/// exit code: 0, 4 when a Provider CLI cannot start or is incompatible, 7 when a catalog is
/// unavailable.
pub fn print_models(ids: &[String], json: bool, resolve: Option<&str>) -> Result<i32, String> {
    let resolve = resolve.map(parse_resolve).transpose()?;
    let (specs, _, warning) = load_specs();
    let specs = selected_specs(specs, ids)?;
    let cancelled = AtomicBool::new(false);
    let catalogs = probe_all(&specs, &cancelled, true);
    let code = exit_code(&catalogs);
    if let Some(warning) = warning.as_deref() {
        eprintln!("warning: {warning}");
    }
    if json {
        println!("{}", document(&catalogs, resolve.as_ref(), code));
    } else {
        for line in table(&catalogs, resolve.as_ref()) {
            println!("{line}");
        }
    }
    for catalog in &catalogs {
        match &catalog.outcome {
            CatalogOutcome::Listed(_) => {}
            CatalogOutcome::CliRefused(reason) | CatalogOutcome::Unavailable(reason) => {
                eprintln!("af provider models: {}: {reason}", catalog.id);
            }
        }
    }
    if code == EXIT_USAGE_UNAVAILABLE {
        eprintln!(
            "af provider models: a catalog is unavailable; af's static rules still apply and nothing was invented"
        );
    }
    Ok(code)
}

/// The Providers the command reads: the named ones, else every registered one, else the CLI
/// default contexts af discovered.
fn selected_specs(specs: Vec<ProviderSpec>, ids: &[String]) -> Result<Vec<ProviderSpec>, String> {
    if !ids.is_empty() {
        let mut selected = Vec::new();
        for id in ids {
            let spec = specs.iter().find(|spec| &spec.id == id).ok_or_else(|| {
                format!(
                    "Provider {} not found in this machine's Provider registry or its \
                         discovered contexts; af provider status lists the Providers af knows",
                    hint::shown(id)
                )
            })?;
            if !selected
                .iter()
                .any(|known: &ProviderSpec| known.id == spec.id)
            {
                selected.push(spec.clone());
            }
        }
        return Ok(selected);
    }
    let registered: Vec<ProviderSpec> = specs
        .iter()
        .filter(|spec| spec.registry_declared)
        .cloned()
        .collect();
    Ok(if registered.is_empty() {
        specs
    } else {
        registered
    })
}

pub(crate) fn exit_code(catalogs: &[ProviderCatalog]) -> i32 {
    if catalogs
        .iter()
        .any(|catalog| matches!(catalog.outcome, CatalogOutcome::CliRefused(_)))
    {
        EXIT_PROVIDER_CLI_MISSING
    } else if catalogs
        .iter()
        .any(|catalog| matches!(catalog.outcome, CatalogOutcome::Unavailable(_)))
    {
        EXIT_USAGE_UNAVAILABLE
    } else {
        EXIT_OK
    }
}

/// The table header of the catalog rows.
pub(crate) fn rows_header() -> String {
    format!(
        "  {:<28} {:<22} {:<20} EFFORT LEVELS (* default)",
        "EXACT ID", "DISPLAY NAME", "ALIASES"
    )
}

/// One catalog row: exact ID, display name, aliases, effort levels with the default marked.
pub(crate) fn model_row(model: &CatalogModel) -> String {
    let aliases = if model.aliases.is_empty() {
        "-".to_owned()
    } else {
        model.aliases.join(", ")
    };
    let name = if model.hidden {
        format!("{} (hidden)", model.display_name)
    } else {
        model.display_name.clone()
    };
    format!(
        "  {:<28} {:<22} {:<20} {}",
        model.id,
        name,
        aliases,
        effort_levels(model)
    )
}

/// A model's effort levels, the default marked `*`.
pub(crate) fn effort_levels(model: &CatalogModel) -> String {
    if model.efforts.is_empty() {
        return "none listed".into();
    }
    model
        .efforts
        .iter()
        .map(|level| {
            if model.default_effort.as_deref() == Some(level) {
                format!("{level}*")
            } else {
                level.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The static rules, printed when a catalog is unavailable: what af still applies, and nothing
/// invented.
pub(crate) fn static_rules_lines(kind: Kind) -> Vec<String> {
    let mut lines = vec![
        format!("  static rules  {}", hint::PRINTABLE_RULE),
        format!(
            "  static rules  {}: an exact ID holds a '-' and a digit and does not end in -latest",
            rules::VERSIONED_RULE
        ),
        format!(
            "  static rules  {} effort levels: {}",
            kind.name(),
            kind.efforts().join(", ")
        ),
    ];
    let aliases = kind.documented_aliases();
    if !aliases.is_empty() {
        lines.push(format!(
            "  static rules  documented aliases, refused with their exact IDs: {}",
            aliases
                .iter()
                .map(|(alias, id)| format!("{alias} → {id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines
}

/// The human output: per Provider its header, rows and proof sentence, or why it has no
/// catalog and the static rules; then each resolution.
pub(crate) fn table(catalogs: &[ProviderCatalog], resolve: Option<&ResolveRequest>) -> Vec<String> {
    let mut lines = Vec::new();
    if catalogs.is_empty() {
        lines.push(
            "No supported provider CLI is installed and no provider registry entries exist; af \
             provider detect finds and registers the logins on this machine"
                .to_owned(),
        );
        return lines;
    }
    for (index, catalog) in catalogs.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        let name = format!("{} ({})", catalog.id, catalog.kind.name());
        match &catalog.outcome {
            CatalogOutcome::Listed(report) => {
                lines.push(format!("{name}  {}", report.header()));
                lines.push(rows_header());
                lines.extend(report.catalog.models.iter().map(model_row));
                if report.skipped > 0 {
                    lines.push(format!(
                        "  ({} listed entries left out: an ID outside af's printable spelling, or past {MAX_CATALOG_MODELS} models)",
                        report.skipped
                    ));
                }
                lines.push(format!("  {PROOF}."));
            }
            CatalogOutcome::CliRefused(_) => {
                lines.push(format!(
                    "{name}  CLI cannot serve af; see stderr. Nothing is listed."
                ));
                lines.extend(static_rules_lines(catalog.kind));
            }
            CatalogOutcome::Unavailable(_) => {
                lines.push(format!(
                    "{name}  catalog unavailable; see stderr. Nothing is invented."
                ));
                lines.extend(static_rules_lines(catalog.kind));
            }
        }
        if let Some(request) = resolve {
            lines.extend(resolution_lines(catalog, request));
        }
    }
    lines
}

/// The copyable configuration for an exact ID and effort level, in af's own schemas. Each field
/// holds the lines of one TOML document that parses on its own and pastes unchanged into the file
/// it is named for.
pub(crate) struct Configuration {
    /// The review package's `[runner]` table for `reviewer.toml`.
    pub(crate) reviewer: Vec<String>,
    /// The Task package's `[runner]` table for `worker.toml`.
    pub(crate) worker: Vec<String>,
    /// The Task catalog's (or `--bindings` file's) `[providers]` table, with the review's
    /// `--provider` as a comment line.
    pub(crate) binding: Vec<String>,
}

pub(crate) fn configuration(
    provider: &str,
    kind: Kind,
    model: &str,
    effort: &str,
) -> Configuration {
    // A TOML basic string, as the repository's own manifests spell them.
    let quoted = |value: &str| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    let args: Vec<String> = match kind {
        Kind::Claude => vec![
            "--model".into(),
            model.into(),
            "--effort".into(),
            effort.into(),
        ],
        Kind::Codex => vec![
            "--model".into(),
            model.into(),
            "-c".into(),
            format!("model_reasoning_effort={}", serde_json::Value::from(effort)),
        ],
    };
    let args = args
        .iter()
        .map(|value| format!("{{ value = {} }}", quoted(value)))
        .collect::<Vec<_>>()
        .join(", ");
    Configuration {
        reviewer: vec![
            "[runner]".into(),
            format!("program = {}", quoted(kind.name())),
            format!("args = [{args}]"),
        ],
        worker: vec![
            "[runner]".into(),
            "kind = \"model\"".into(),
            format!("provider_kind = {}", quoted(kind.name())),
            format!("model = {}", quoted(model)),
            format!("effort = {}", quoted(effort)),
        ],
        binding: vec![
            "[providers]".into(),
            format!("\"PACKAGE\" = {}", quoted(provider)),
            format!("# af review run … --provider NODE={provider}"),
        ],
    }
}

/// What `--resolve` concluded on one Provider.
pub(crate) struct Resolved {
    pub(crate) resolution: Option<Resolution>,
    pub(crate) effort: EffortCheck,
    /// The exact ID's effort levels, when the catalog lists it.
    pub(crate) levels: Vec<String>,
    pub(crate) configuration: Option<Configuration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EffortCheck {
    NotRequested,
    Listed,
    NotListed,
    /// No listed model to check it against; checked against the kind's vocabulary only.
    Vocabulary {
        accepted: bool,
    },
}

impl EffortCheck {
    fn name(&self) -> &'static str {
        match self {
            EffortCheck::NotRequested => "not_requested",
            EffortCheck::Listed => "listed",
            EffortCheck::NotListed => "not_listed",
            EffortCheck::Vocabulary { accepted: true } => "in_vocabulary",
            EffortCheck::Vocabulary { accepted: false } => "not_in_vocabulary",
        }
    }
}

pub(crate) fn resolved(catalog: &ProviderCatalog, request: &ResolveRequest) -> Resolved {
    let Some(report) = catalog.outcome.report() else {
        let effort = match &request.effort {
            Some(effort) => EffortCheck::Vocabulary {
                accepted: catalog.kind.efforts().contains(&effort.as_str()),
            },
            None => EffortCheck::NotRequested,
        };
        return Resolved {
            resolution: None,
            effort,
            levels: Vec::new(),
            configuration: None,
        };
    };
    let resolution = rules::resolve(catalog.kind, &report.catalog, &request.spelling);
    let listed = resolution.id().and_then(|id| report.catalog.model(id));
    let effort = match (&request.effort, listed) {
        (None, _) => EffortCheck::NotRequested,
        (Some(effort), Some(model)) if model.efforts.iter().any(|level| level == effort) => {
            EffortCheck::Listed
        }
        (Some(_), Some(_)) => EffortCheck::NotListed,
        (Some(effort), None) => EffortCheck::Vocabulary {
            accepted: catalog.kind.efforts().contains(&effort.as_str()),
        },
    };
    // af never substitutes: a configuration is printed only for a listed pair.
    let configuration = listed.and_then(|model| {
        let effort = match (&request.effort, &effort) {
            (Some(effort), EffortCheck::Listed) => Some(effort.clone()),
            (None, _) => model.default_effort.clone(),
            _ => None,
        }?;
        Some(configuration(&catalog.id, catalog.kind, &model.id, &effort))
    });
    Resolved {
        levels: listed
            .map(|model| model.efforts.clone())
            .unwrap_or_default(),
        resolution: Some(resolution),
        effort,
        configuration,
    }
}

fn resolution_lines(catalog: &ProviderCatalog, request: &ResolveRequest) -> Vec<String> {
    let spelling = &request.spelling;
    let asked = match &request.effort {
        Some(effort) => format!("{spelling}/{effort}"),
        None => spelling.clone(),
    };
    let mut lines = vec![format!(
        "  resolve  `{asked}` on {} ({})",
        catalog.id,
        catalog.kind.name()
    )];
    let resolved = resolved(catalog, request);
    let model = match &resolved.resolution {
        None => format!(
            "no catalog to resolve against; the static rules say: {}",
            match rules::check_static(catalog.kind, spelling, None) {
                Ok(()) => "passes af's spelling rule, unverified against the catalog".to_owned(),
                Err(refusal) => refusal.fact(catalog.kind, spelling, None),
            }
        ),
        Some(Resolution::Exact { id }) => {
            format!("`{id}` is an exact ID in this harness's catalog: confirmed")
        }
        Some(Resolution::Alias { id }) => format!(
            "`{spelling}` is an alias of the exact ID {id}; af binds exact IDs, so bind {id}"
        ),
        Some(Resolution::DisplayName { id }) => {
            format!("`{spelling}` is the display name of the exact ID {id}")
        }
        Some(Resolution::FamilyVersion { id }) => {
            format!("`{spelling}` names the exact ID {id}")
        }
        Some(Resolution::Misspelled { nearest }) => format!(
            "`{spelling}` is not in this harness's catalog; nearest catalog IDs: {}",
            nearest.join(", ")
        ),
        Some(Resolution::NotListed { rule: None }) => format!(
            "`{spelling}` is not in this harness's catalog; passes af's spelling rule; usability only by af provider doctor"
        ),
        Some(Resolution::NotListed {
            rule: Some(refusal),
        }) => format!(
            "`{spelling}` is not in this harness's catalog; fails af's spelling rule: {}; usability only by af provider doctor",
            refusal.fact(catalog.kind, spelling, None)
        ),
    };
    lines.push(format!("    model          {model}"));
    let id = resolved
        .resolution
        .as_ref()
        .and_then(Resolution::id)
        .unwrap_or("");
    if let Some(effort) = &request.effort {
        let text = match &resolved.effort {
            EffortCheck::Listed => format!(
                "{effort}: confirmed, {id} lists {}",
                resolved.levels.join(", ")
            ),
            EffortCheck::NotListed if resolved.levels.is_empty() => {
                format!("{effort}: effort not listed: {id} lists no effort levels")
            }
            EffortCheck::NotListed => format!(
                "{effort}: effort not listed for {id}, which lists {}",
                resolved.levels.join(", ")
            ),
            EffortCheck::Vocabulary { accepted: true } => format!(
                "{effort}: one of the {} effort levels; no listed model to check it against",
                catalog.kind.name()
            ),
            EffortCheck::Vocabulary { accepted: false } => format!(
                "{effort}: not one of the {} effort levels: {}",
                catalog.kind.name(),
                catalog.kind.efforts().join(", ")
            ),
            EffortCheck::NotRequested => unreachable!("an effort level was requested"),
        };
        lines.push(format!("    effort         {text}"));
    }
    match &resolved.configuration {
        Some(configuration) => {
            for (label, block) in [
                ("reviewer.toml", &configuration.reviewer),
                ("worker.toml", &configuration.worker),
                ("bindings", &configuration.binding),
            ] {
                for (index, line) in block.iter().enumerate() {
                    let label = if index == 0 { label } else { "" };
                    lines.push(format!("    {label:<14} {line}"));
                }
            }
        }
        None if resolved
            .resolution
            .as_ref()
            .and_then(Resolution::id)
            .is_some()
            && request.effort.is_none() =>
        {
            lines.push(
                "    configuration  add /EFFORT, one of the levels above, to print it".to_owned(),
            );
        }
        None => {}
    }
    lines
}

/// The versioned `af/provider-models@1` document.
pub(crate) fn document(
    catalogs: &[ProviderCatalog],
    resolve: Option<&ResolveRequest>,
    exit_code: i32,
) -> serde_json::Value {
    let providers: Vec<serde_json::Value> = catalogs
        .iter()
        .map(|catalog| {
            let (state, report) = match &catalog.outcome {
                CatalogOutcome::Listed(report) => ("listed", Some(report)),
                CatalogOutcome::CliRefused(_) => ("cli_refused", None),
                CatalogOutcome::Unavailable(_) => ("unavailable", None),
            };
            let models: Vec<serde_json::Value> = report
                .map(|report| report.catalog.models.iter().map(model_document).collect())
                .unwrap_or_default();
            serde_json::json!({
                "id": catalog.id,
                "kind": catalog.kind.name(),
                "state": state,
                "source": match catalog.kind {
                    Kind::Claude => CLAUDE_SOURCE,
                    Kind::Codex => CODEX_SOURCE,
                },
                "harness_version": report.and_then(|report| report.version.clone()),
                "freshness": {
                    "source": if report.is_some_and(|report| report.fetched_at.is_some()) {
                        "models_cache"
                    } else {
                        "live"
                    },
                    "fetched_at": report.and_then(|report| report.fetched_at.clone()),
                },
                "models": models,
                "skipped_entries": report.map_or(0, |report| report.skipped),
                "static_rules": {
                    "effort_levels": catalog.kind.efforts(),
                    "documented_aliases": catalog
                        .kind
                        .documented_aliases()
                        .iter()
                        .map(|(alias, id)| serde_json::json!({"alias": alias, "id": id}))
                        .collect::<Vec<_>>(),
                },
                "resolution": resolve.map(|request| resolution_document(catalog, request)),
            })
        })
        .collect();
    serde_json::json!({
        "schema": "af/provider-models@1",
        "exit_code": exit_code,
        "token_free": true,
        "proves": PROOF,
        "providers": providers,
    })
}

fn model_document(model: &CatalogModel) -> serde_json::Value {
    serde_json::json!({
        "id": model.id,
        "display_name": model.display_name,
        "aliases": model.aliases,
        "effort_levels": model.efforts,
        "default_effort": model.default_effort,
        "hidden": model.hidden,
    })
}

fn resolution_document(catalog: &ProviderCatalog, request: &ResolveRequest) -> serde_json::Value {
    let resolved = resolved(catalog, request);
    let (outcome, nearest, spelling_rule) = match &resolved.resolution {
        None => ("unavailable", Vec::new(), None),
        Some(Resolution::Exact { .. }) => ("exact", Vec::new(), None),
        Some(Resolution::Alias { .. }) => ("alias", Vec::new(), None),
        Some(Resolution::DisplayName { .. }) => ("display_name", Vec::new(), None),
        Some(Resolution::FamilyVersion { .. }) => ("family_version", Vec::new(), None),
        Some(Resolution::Misspelled { nearest }) => ("misspelled", nearest.clone(), None),
        Some(Resolution::NotListed { rule }) => (
            "not_listed",
            Vec::new(),
            Some(if rule.is_some() { "fails" } else { "passes" }),
        ),
    };
    serde_json::json!({
        "spelling": request.spelling,
        "effort": request.effort,
        "outcome": outcome,
        "id": resolved.resolution.as_ref().and_then(Resolution::id),
        "nearest": nearest,
        "spelling_rule": spelling_rule,
        "effort_check": resolved.effort.name(),
        "effort_levels": resolved.levels,
        "configuration": resolved.configuration.as_ref().map(|configuration| serde_json::json!({
            "reviewer_runner": configuration.reviewer.join("\n"),
            "worker_runner": configuration.worker.join("\n"),
            "binding": configuration.binding.join("\n"),
        })),
    })
}

#[cfg(test)]
mod tests;
