//! Providers: the inventory `af provider status --usage` prints, from
//! `providers::discover_with_cancel`, and each Provider's model catalog `af provider models`
//! prints (ADR-0148). Discovery runs one bounded authentication check per context, then reads
//! each probed Provider's catalog token-free, and hands the inventory over; the bounded usage
//! probe (`--usage`: a Provider process or an app-server round trip per login, no tokens) then
//! runs on its own and hands the same inventory over with every quota window, so the limits are
//! always on screen, a moment after the rows. `R` and a command's return run the probe again.
//! All of it runs on a thread the event loop polls and cancels, so a key is never waiting on a
//! Provider. Each window is a bar of twenty cells in the brand's colour for how much is used,
//! green under half, blue from half, pink from four fifths, drawn so that two bars one under
//! the other keep a hairline between them. The model catalogs are not on screen until asked
//! for: one muted row at the end says `<m> to show models`, and `m` shows every listed
//! Provider's `models` section, one row per exact ID of that Provider's own catalog, until `m`
//! hides them again. The
//! catalogs read are shared with the Workers pane under each Provider's id, which gives a
//! runner's verdict once per Provider of its kind and follows the catalogs as they arrive: each
//! hand-over advances the shared generation the Workers pane polls.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};

use review_runner_claude::models::{Catalog, Kind};

use super::{Effect, Pane, Row, SPINNER};
use crate::providers::models::{self as catalogs, CatalogOutcome, ProviderCatalog};
use crate::providers::{
    self, ProviderInventory, ProviderLimit, ProviderStatus, UsageProbe, UsageState,
};
use crate::tui::keymap::Key;
use crate::tui::paint::{Paint, Pixel, Span, Tone};
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

/// Cells of a limit bar.
const BAR: usize = 20;
/// Row text for a login whose quota windows are still being read.
const READING_LIMITS: &str = "limit  reading the quota windows";
/// Row text for a login that exposes no quota window to read.
const NO_LIMITS: &str = "limit  no quota window is reported for this login";
/// The pane's last row while the model catalogs are off screen, and while they show.
const MODELS_SHOW: &str = "<m> to show models";
const MODELS_HIDE: &str = "<m> to hide models";

/// What `d` says where no Provider is selected.
pub(crate) const REMOVE_NEEDS_A_PROVIDER: &str =
    "d removes a Provider: select one in the bar, or open it";

/// The pane's first row on a machine whose registry names no Provider, in the browser's ASCII
/// (`--` for a dash).
pub(crate) const NONE_REGISTERED: &str = "no Provider registered yet -- :provider detect scans this machine (read-only); :provider detect --apply registers what it finds";

/// One probed Provider's catalog, under its Provider id and with its kind.
#[derive(Clone)]
pub(crate) struct SharedCatalog {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    pub(crate) catalog: Catalog,
}

/// The catalogs the Providers pane read, one per probed Provider whose catalog was listed, in
/// inventory order: what the Workers pane judges a runner against, once per Provider of the
/// runner's kind. A Provider of the same kind never hides another's catalog. Every `set`
/// advances the generation, which the Workers pane polls to rebuild its rows when the catalogs
/// it judged against change.
#[derive(Clone, Default)]
pub(crate) struct SharedCatalogs(Arc<Mutex<Shared>>);

#[derive(Default)]
struct Shared {
    generation: u64,
    catalogs: Vec<SharedCatalog>,
}

impl SharedCatalogs {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// How many times the catalogs were set: a reader that recorded another generation judged
    /// against catalogs that are no longer the ones shared.
    pub(crate) fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// The catalogs of every Provider of `kind`, in inventory order.
    pub(crate) fn of_kind(&self, kind: Kind) -> Vec<SharedCatalog> {
        self.lock()
            .catalogs
            .iter()
            .filter(|catalog| catalog.kind == kind)
            .cloned()
            .collect()
    }

    /// Replace the catalogs with those `read` listed and advance the generation.
    pub(crate) fn set(&self, read: &[ProviderCatalog]) {
        let mut shared = self.lock();
        shared.generation += 1;
        shared.catalogs.clear();
        for catalog in read {
            if let Some(report) = catalog.outcome.report() {
                shared.catalogs.push(SharedCatalog {
                    id: catalog.id.clone(),
                    kind: catalog.kind,
                    catalog: report.catalog.clone(),
                });
            }
        }
    }
}

/// What one discovery handed over: the inventory and the catalogs of the Providers it probed.
struct Discovered {
    inventory: ProviderInventory,
    catalogs: Vec<ProviderCatalog>,
}

struct Job {
    receiver: Receiver<Discovered>,
    cancel: Arc<AtomicBool>,
    usage: UsageProbe,
}

pub(crate) struct ProvidersPane {
    inventory: Option<ProviderInventory>,
    job: Option<Job>,
    /// Discover on load, and probe the quota windows once discovery answered. A pane over an
    /// inventory already taken (a test's) never runs a Provider CLI.
    discover: bool,
    /// A test's pane: `start` opens a job no thread feeds, and the test hands its answer over.
    #[cfg(test)]
    fed: Option<mpsc::Sender<Discovered>>,
    /// Whether the registry named no Provider when the pane loaded, read without a probe, for
    /// the time before discovery hands an inventory over.
    unregistered: bool,
    selected: Option<String>,
    rows: Vec<Row>,
    spinner: usize,
    /// Each Provider's catalog, as the last complete discovery read it.
    catalogs: Vec<ProviderCatalog>,
    /// `m` shows every Provider's `models` section; off until asked.
    models_open: bool,
    shared: SharedCatalogs,
    /// The variables pointing a Provider CLI at its directory (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME`) the browser's environment sets: an ambient row says it came through one.
    selectors: BTreeSet<&'static str>,
}

impl ProvidersPane {
    /// A pane that discovers the machine's providers when it first loads.
    pub(crate) fn discovering() -> ProvidersPane {
        ProvidersPane {
            inventory: None,
            job: None,
            discover: true,
            #[cfg(test)]
            fed: None,
            unregistered: false,
            selected: None,
            rows: Vec::new(),
            spinner: 0,
            catalogs: Vec::new(),
            models_open: false,
            shared: SharedCatalogs::default(),
            selectors: ["claude", "codex"]
                .into_iter()
                .map(|kind| providers::ambient_selector(kind).0)
                .filter(|name| std::env::var_os(name).is_some())
                .collect(),
        }
    }

    /// The pane as if the browser's environment set exactly `selectors`.
    #[cfg(test)]
    pub(crate) fn with_selectors(mut self, selectors: &[&'static str]) -> ProvidersPane {
        self.selectors = selectors.iter().copied().collect();
        self.rebuild();
        self
    }

    /// Share the catalogs this pane reads with the Workers pane.
    pub(crate) fn share(&mut self, shared: SharedCatalogs) {
        shared.set(&self.catalogs);
        self.shared = shared;
    }

    #[cfg(test)]
    pub(crate) fn with_catalogs(mut self, catalogs: Vec<ProviderCatalog>) -> ProvidersPane {
        self.shared.set(&catalogs);
        self.catalogs = catalogs;
        self.rebuild();
        self
    }

    #[cfg(test)]
    pub(crate) fn with_inventory(inventory: ProviderInventory) -> ProvidersPane {
        let mut pane = ProvidersPane::discovering();
        pane.inventory = Some(inventory);
        pane.discover = false;
        // A render never depends on the environment of the process painting it.
        pane.selectors.clear();
        pane.rebuild();
        pane
    }

    /// `d` on a Provider this pane lists: the `:` line that removes it from the registry, as
    /// `af provider remove` does. An ambient candidate has no registry entry behind it, so
    /// there is no line to offer and the pane says why.
    pub(crate) fn remove_line(&self, id: &str) -> Result<String, String> {
        let inventory = self.inventory.as_ref();
        let listed = inventory.and_then(|inventory| {
            let mut providers = inventory.providers.iter();
            providers.find(|provider| provider.id == id)
        });
        match listed {
            // Nothing to remove: the line says how it is registered instead, in the status
            // line's width; its rows also say how it got there and how to make it go.
            Some(provider) if providers::is_ambient_candidate(provider) => {
                let kind = &provider.kind;
                let add = providers::available_provider_id(kind, &self.ids());
                Err(format!(
                    "{id}: no registry entry to remove; af provider add {add} --kind {kind} \
                     adds it"
                ))
            }
            Some(_) => Ok(format!("provider remove {}", shell_words::quote(id))),
            None => Err(format!("{id} is not a listed Provider")),
        }
    }

    /// Discover again on a thread, with or without the usage probe; the shown inventory stays
    /// until the thread answers.
    fn start(&mut self, usage: UsageProbe) {
        self.cancel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        #[cfg(test)]
        if self.fed.is_some() {
            // No Provider CLI runs in a test: the job waits for what the test hands over.
            self.fed = Some(sender);
            self.job = Some(Job {
                receiver,
                cancel,
                usage,
            });
            self.rebuild();
            return;
        }
        std::thread::spawn(move || {
            let inventory = providers::discover_with_cancel(&flag, usage);
            // A CLI that cannot start, or was not probed, has no catalog to read.
            let ids: Vec<String> = inventory
                .providers
                .iter()
                .filter(|provider| {
                    !matches!(
                        provider.status.as_str(),
                        "installation failed"
                            | "incompatible"
                            | "not probed"
                            | "unavailable"
                            | "disabled"
                    )
                })
                .map(|provider| provider.id.clone())
                .collect();
            let catalogs = catalogs::catalogs_with_cancel(&ids, &flag);
            let _ = sender.send(Discovered {
                inventory,
                catalogs,
            });
        });
        self.job = Some(Job {
            receiver,
            cancel,
            usage,
        });
        self.rebuild();
    }

    /// Whether this machine has no registered Provider: no inventory entry but an ambient
    /// candidate, or, before discovery answers, a registry that names none.
    pub(crate) fn none_registered(&self) -> bool {
        match &self.inventory {
            Some(inventory) => inventory
                .providers
                .iter()
                .all(providers::is_ambient_candidate),
            None => self.unregistered,
        }
    }

    /// Every Provider id the inventory lists.
    fn ids(&self) -> BTreeSet<String> {
        let providers = self
            .inventory
            .iter()
            .flat_map(|inventory| &inventory.providers);
        providers.map(|provider| provider.id.clone()).collect()
    }

    fn rebuild(&mut self) {
        let title = match &self.selected {
            Some(id) => format!("PROVIDERS  {id}"),
            None => "PROVIDERS  (af provider status)".to_owned(),
        };
        let mut rows = vec![Row::painted(title, Paint::Title)];
        if self.none_registered() {
            rows.push(Row::plain(NONE_REGISTERED));
        }
        rows.push(Row::blank());
        match &self.inventory {
            Some(inventory) => {
                let selected = self.selected.as_deref();
                let models = Models {
                    catalogs: &self.catalogs,
                    open: self.models_open,
                    selectors: &self.selectors,
                };
                let reading = self.job.is_some();
                inventory_rows(inventory, selected, reading, &models, &mut rows);
            }
            None => rows.push(Row::plain("Discovering providers.")),
        }
        if let Some(job) = &self.job {
            let spinner = SPINNER[self.spinner % SPINNER.len()];
            let what = match job.usage {
                UsageProbe::Skip => {
                    "checking authentication and reading model catalogs, bounded per context"
                }
                UsageProbe::Probe => {
                    "reading quota windows and model catalogs, bounded per context"
                }
            };
            rows.push(Row::blank());
            rows.push(Row::painted(format!("{spinner} {what}"), Paint::Muted));
        }
        self.rows = rows;
    }
}

impl Pane for ProvidersPane {
    fn load(&mut self, _scope: &Scope) -> Result<(), String> {
        if self.discover && self.inventory.is_none() && self.job.is_none() {
            self.unregistered = providers::read_registry_entries().registered.is_empty();
            self.start(UsageProbe::Skip);
        }
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let Some(inventory) = &self.inventory else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for provider in &inventory.providers {
            items.push(Item {
                id: provider.id.clone(),
                label: provider.id.clone(),
                muted: providers::is_ambient_candidate(provider),
                tone: None,
                children: None,
            });
        }
        items
    }

    fn open(&mut self, item: Option<&str>) {
        self.selected = item.map(str::to_owned);
        self.rebuild();
    }

    fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// `d` fills the `:` line with `provider remove ID` for the opened Provider; nothing is
    /// removed until the user runs that line. `m` shows or hides the `models` sections.
    fn key(&mut self, key: Key, _row: usize) -> Result<Option<Effect>, String> {
        if key == Key::Char('m') {
            self.models_open = !self.models_open;
            self.rebuild();
            return Ok(None);
        }
        if key != Key::Char('d') {
            return Ok(None);
        }
        match &self.selected {
            Some(id) => self.remove_line(id).map(Effect::Prefill).map(Some),
            None => Err(REMOVE_NEEDS_A_PROVIDER.to_owned()),
        }
    }

    fn legend(&self) -> &'static str {
        match self.selected {
            Some(_) => "m models  d remove  y yank id  gf registry  Tab bar  :cmd  q quit",
            None => "j/k move  m models  y yank id  gf registry  Tab bar  :cmd  q quit",
        }
    }

    /// `R` discovers again with the usage probe `af provider status --usage` runs, and reads
    /// every catalog again; the shown inventory stays until the probe answers.
    fn refresh(&mut self, _scope: &Scope) -> Result<(), String> {
        self.start(UsageProbe::Probe);
        Ok(())
    }

    /// A command may have set up or removed a Provider, or spent a window: discover again with
    /// the probe, as `R` does. A fixed inventory stays as it is.
    fn reread(&mut self, _scope: &Scope) -> Result<(), String> {
        if self.discover {
            self.start(UsageProbe::Probe);
        }
        Ok(())
    }

    fn poll(&mut self) -> bool {
        let Some(job) = self.job.as_ref() else {
            return false;
        };
        let usage = job.usage;
        match job.receiver.try_recv() {
            Ok(Discovered {
                inventory,
                catalogs,
            }) => {
                self.shared.set(&catalogs);
                self.catalogs = catalogs;
                // A Provider removed since the last discovery is no longer there to show: the
                // folder shows what is registered now.
                let listed = |id: &String| inventory.providers.iter().any(|p| p.id == *id);
                if !self.selected.as_ref().is_some_and(listed) {
                    self.selected = None;
                }
                self.inventory = Some(inventory);
                self.job = None;
                // The rows are on screen: now the quota windows, which take a Provider process
                // per login and a couple of seconds, behind their own spinner.
                if usage == UsageProbe::Skip && self.discover {
                    self.start(UsageProbe::Probe);
                }
            }
            Err(TryRecvError::Empty) => self.spinner = self.spinner.wrapping_add(1),
            Err(TryRecvError::Disconnected) => self.job = None,
        }
        self.rebuild();
        true
    }

    fn busy(&self) -> Option<char> {
        let spinner = SPINNER[self.spinner % SPINNER.len()];
        self.job.as_ref().map(|_| spinner)
    }

    /// Stop the probe and forget it: whatever it returns is never shown, so the last complete
    /// inventory and its windows stay on screen and `R` reads again.
    fn cancel(&mut self) -> bool {
        match self.job.take() {
            Some(job) => {
                job.cancel.store(true, Ordering::Release);
                self.rebuild();
                true
            }
            None => false,
        }
    }

    fn file(&self, _row: usize) -> Option<PathBuf> {
        let registry = self.inventory.as_ref()?.registry.clone()?;
        registry.is_file().then_some(registry)
    }

    fn yank(&self, _row: usize) -> Option<String> {
        self.selected.clone()
    }
}

/// The catalogs a rebuild may show, and whether `m` asked for them.
struct Models<'a> {
    catalogs: &'a [ProviderCatalog],
    open: bool,
    /// The selector variables the environment sets, which an ambient row names.
    selectors: &'a BTreeSet<&'static str>,
}

/// The `af provider status` table for the selected provider, or for all of them with a blank
/// row between two providers, then each provider's limit bars and note, the setup line for an
/// ambient candidate, and, once `m` asked for them, each provider's models; the last row says
/// what `m` does while a catalog was read.
/// `reading` while a discovery or probe runs: a login whose windows it has not read yet says so.
fn inventory_rows(
    inventory: &ProviderInventory,
    selected: Option<&str>,
    reading: bool,
    models: &Models<'_>,
    rows: &mut Vec<Row>,
) {
    if let Some(warning) = &inventory.warning {
        rows.push(Row::error("warning", warning));
    }
    if inventory.providers.is_empty() {
        let none = "No supported provider CLI is installed and no provider registry entries exist; \
                    af provider detect finds and registers the logins on this machine";
        rows.push(Row::plain(none));
        return;
    }
    let mut ids = BTreeSet::new();
    for provider in &inventory.providers {
        ids.insert(provider.id.clone());
    }
    rows.push(Row::painted(providers::status_table_header(), Paint::Title));
    let mut any_catalog = false;
    let mut listed = 0;
    for provider in &inventory.providers {
        if selected.is_none_or(|id| id == provider.id) {
            if listed > 0 {
                rows.push(Row::blank());
            }
            listed += 1;
            provider_rows(
                provider,
                selected.is_some(),
                reading,
                &ids,
                models.selectors,
                rows,
            );
            let catalog = models.catalogs.iter().find(|c| c.id == provider.id);
            if let Some(catalog) = catalog {
                any_catalog = true;
                if models.open {
                    // The models stand apart from the login's windows and note.
                    rows.push(Row::blank());
                    model_rows(catalog, rows);
                }
            }
        }
    }
    if any_catalog {
        rows.push(Row::blank());
        let hint = if models.open {
            MODELS_HIDE
        } else {
            MODELS_SHOW
        };
        rows.push(Row::painted(hint, Paint::Muted));
    }
}

fn provider_rows(
    provider: &ProviderStatus,
    alone: bool,
    reading: bool,
    ids: &BTreeSet<String>,
    selectors: &BTreeSet<&'static str>,
    rows: &mut Vec<Row>,
) {
    let ambient = providers::is_ambient_candidate(provider);
    let paint = if ambient { Paint::Muted } else { Paint::Plain };
    rows.push(status_row(provider, paint));
    if alone {
        rows.push(Row::blank());
    }
    limit_rows(provider, reading, paint, rows);
    if !provider.detail.is_empty() {
        rows.push(Row::painted(format!("note   {}", provider.detail), paint));
    }
    if ambient {
        // How it got here and how to make it go, then how to log it in.
        let (selector, _) = providers::ambient_selector(&provider.kind);
        let set = selectors.contains(selector);
        for line in providers::ambient_explanation(&provider.kind, set, ids) {
            rows.push(Row::painted(line, Paint::Muted));
        }
        let hint = providers::setup_hint(&provider.kind, ids);
        rows.push(Row::painted(hint, Paint::Muted));
    }
}

/// A Provider's `models` section: the header line of `af provider models`, then one row per
/// exact ID (ID, display name, effort levels, aliases); a catalog that could not be read says
/// why.
fn model_rows(catalog: &ProviderCatalog, rows: &mut Vec<Row>) {
    let report = match &catalog.outcome {
        CatalogOutcome::Listed(report) => report,
        CatalogOutcome::CliRefused(reason) | CatalogOutcome::Unavailable(reason) => {
            rows.push(Row::painted(
                format!("models unavailable: {reason}; the static rules apply"),
                Paint::Muted,
            ));
            return;
        }
    };
    let count = report.catalog.models.len();
    rows.push(Row::plain(format!(
        "models {}  {count} exact IDs",
        report.header()
    )));
    for model in &report.catalog.models {
        let aliases = if model.aliases.is_empty() {
            String::new()
        } else {
            format!("  aliases {}", model.aliases.join(", "))
        };
        rows.push(Row::plain(format!(
            "  model  {:<24} {:<14} {}{aliases}",
            model.id,
            model.display_name,
            catalogs::effort_levels(model)
        )));
    }
    rows.push(Row::painted(
        format!("  {}.", catalogs::PROOF),
        Paint::Muted,
    ));
}

/// The provider's `af provider status` row, its STATUS word as a chip: green for
/// `authenticated`, pink for a provider that cannot be used, none for one that is not known.
fn status_row(provider: &ProviderStatus, paint: Paint) -> Row {
    let row = providers::status_table_row(provider);
    let tone = match providers::usability_name(&provider.status) {
        "usable_or_untested" => Tone::Ok,
        "unusable" => Tone::Fail,
        _ => return Row::painted(row, paint),
    };
    if paint != Paint::Plain {
        return Row::painted(row, paint);
    }
    // STATUS follows the ID and KIND cells; the chip takes the space either side of the word.
    let word = format!(" {} ", provider.status);
    let from = provider.id.len() + 1 + provider.kind.len();
    let Some(at) = row.get(from..).and_then(|rest| rest.find(&word)) else {
        return Row::painted(row, paint);
    };
    let at = from + at;
    Row {
        spans: vec![
            Span::new(&row[..at], paint),
            Span::chip(&provider.status, tone),
            Span::new(&row[at + word.len()..], paint),
        ],
    }
}

/// One row per quota window, each a bar; a login whose windows are still being read, or that
/// reports none, says so in one muted row. A login that is not authenticated has no window, and
/// one whose probe did not answer carries the cause in its `note`.
fn limit_rows(provider: &ProviderStatus, reading: bool, paint: Paint, rows: &mut Vec<Row>) {
    if provider.limits.is_empty() {
        let authenticated = provider.status == "authenticated";
        match provider.usage {
            UsageState::NotRequested if reading && authenticated => {
                rows.push(Row::painted(READING_LIMITS, Paint::Muted));
            }
            UsageState::Unsupported => rows.push(Row::painted(NO_LIMITS, Paint::Muted)),
            _ => {}
        }
        return;
    }
    let width = provider.limits.iter().map(|l| l.name.len()).max();
    for limit in &provider.limits {
        rows.push(limit_row(&provider.kind, limit, width.unwrap_or(0), paint));
    }
}

/// A window as `limit  NAME  [####................]   20% used  resets in 6d 20h`: the bar's
/// used cells are drawn in the band's colour (green under half, blue from half, pink from four
/// fifths), the rest as a grey track, each cell a block that keeps a hairline from the row
/// above ([`Paint::Bar`]); in the terminal's own colours the same, and without colour `#` and
/// `.`. The text is what the CLI prints, the words put in bar order.
fn limit_row(kind: &str, limit: &ProviderLimit, width: usize, paint: Paint) -> Row {
    let used = usize::from(limit.used_percent.min(100));
    let filled = (used * BAR + 50) / 100;
    let mut spans = vec![Span::new(
        format!("limit  {:<width$}  [", limit.name),
        paint,
    )];
    if filled > 0 {
        spans.push(Span::new(
            "#".repeat(filled),
            Paint::Bar(band(limit.used_percent)),
        ));
    }
    if filled < BAR {
        spans.push(Span::new(".".repeat(BAR - filled), Paint::Bar(Pixel::Grey)));
    }
    spans.push(Span::new(
        format!(
            "]  {:>3}% used  {}",
            limit.used_percent,
            providers::format_reset_of(kind, limit)
        ),
        paint,
    ));
    Row { spans }
}

/// The colour of a bar's used cells: green while under half is used, blue from half, pink from
/// four fifths, when a window is close to its end.
fn band(used_percent: u8) -> Pixel {
    match used_percent {
        0..=49 => Pixel::Green,
        50..=79 => Pixel::Blue,
        _ => Pixel::Pink,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::providers::models::{
        CatalogReport, parse_claude_list_models, parse_codex_model_list,
    };

    /// The two recorded catalogs, as the discovery job would hand them over for `claude-work` and
    /// `codex-main`.
    pub(crate) fn fixture_catalogs() -> Vec<ProviderCatalog> {
        let claude =
            include_str!("../../../tests/fixtures/provider-models/claude-list-models.json");
        let codex = include_str!("../../../tests/fixtures/provider-models/codex-model-list.json");
        let (claude, _) = parse_claude_list_models(&serde_json::from_str(claude).unwrap()).unwrap();
        let (codex, _) = parse_codex_model_list(&serde_json::from_str(codex).unwrap()).unwrap();
        vec![
            ProviderCatalog {
                id: "claude-work".into(),
                kind: Kind::Claude,
                outcome: CatalogOutcome::Listed(CatalogReport {
                    source: catalogs::CLAUDE_SOURCE,
                    version: Some("2.1.300".into()),
                    fetched_at: None,
                    catalog: claude,
                    skipped: 0,
                }),
            },
            ProviderCatalog {
                id: "codex-main".into(),
                kind: Kind::Codex,
                outcome: CatalogOutcome::Listed(CatalogReport {
                    source: catalogs::CODEX_SOURCE,
                    version: Some("0.162.1".into()),
                    fetched_at: Some("2026-10-15T09:58:12Z".into()),
                    catalog: codex,
                    skipped: 0,
                }),
            },
        ]
    }

    fn window(name: &str, used_percent: u8, resets_at: Option<u64>) -> ProviderLimit {
        ProviderLimit {
            name: name.to_owned(),
            window_minutes: Some(10_080),
            used_percent,
            resets_at,
        }
    }

    /// The bar's text, and the paint of its used and left cells.
    fn bar_of(row: &Row) -> (String, Vec<(usize, Paint)>) {
        let text = row.text();
        let start = text.find('[').unwrap();
        let end = text.find(']').unwrap();
        let cells = row
            .spans
            .iter()
            .filter(|span| matches!(span.paint, Paint::Bar(_)))
            .map(|span| (span.text.len(), span.paint))
            .collect();
        (text[start..=end].to_owned(), cells)
    }

    #[test]
    fn limit_bars_round_to_the_nearest_cell_and_take_the_band_colour() {
        let row = |used| limit_row("claude", &window("weekly", used, None), 6, Paint::Plain);
        let (bar, cells) = bar_of(&row(0));
        assert_eq!(bar, "[....................]");
        assert_eq!(cells, [(20, Paint::Bar(Pixel::Grey))]);
        let (bar, cells) = bar_of(&row(41));
        assert_eq!(bar, "[########............]");
        assert_eq!(
            cells,
            [(8, Paint::Bar(Pixel::Green)), (12, Paint::Bar(Pixel::Grey))]
        );
        let (bar, cells) = bar_of(&row(68));
        assert_eq!(bar, "[##############......]");
        assert_eq!(cells[0], (14, Paint::Bar(Pixel::Blue)));
        let (bar, cells) = bar_of(&row(85));
        assert_eq!(bar, "[#################...]");
        assert_eq!(cells[0], (17, Paint::Bar(Pixel::Pink)));
        let (bar, cells) = bar_of(&row(100));
        assert_eq!(bar, "[####################]");
        assert_eq!(cells, [(20, Paint::Bar(Pixel::Pink))]);
        // A percentage past 100 fills the bar and is printed as reported.
        let (bar, _) = bar_of(&row(255));
        assert_eq!(bar, "[####################]");
        assert!(row(255).text().contains("]  255% used"));
    }

    #[test]
    fn a_limit_row_names_the_window_its_use_and_its_reset() {
        let far = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 6 * 24 * 60 * 60
            + 20 * 60 * 60
            + 30 * 60;
        let codex = limit_row("codex", &window("codex 1w", 5, Some(far)), 8, Paint::Plain);
        assert_eq!(
            codex.text(),
            "limit  codex 1w  [#...................]    5% used  resets in 6d 20h"
        );
        let claude = limit_row("claude", &window("weekly", 68, None), 9, Paint::Plain);
        assert_eq!(
            claude.text(),
            "limit  weekly     [##############......]   68% used  no reset time from Claude"
        );
        // An ambient Provider's words are muted; its bar keeps its colour.
        let muted = limit_row("codex", &window("codex 1w", 5, Some(far)), 8, Paint::Muted);
        assert_eq!(muted.spans[0].paint, Paint::Muted);
        assert_eq!(muted.spans[1].paint, Paint::Bar(Pixel::Green));
    }

    /// Discovery hands the rows over first; the quota windows follow from a probe the pane
    /// starts itself, and a login says it is reading them meanwhile.
    #[test]
    fn the_quota_windows_are_read_right_after_discovery() {
        let mut pane = ProvidersPane::discovering();
        pane.fed = Some(mpsc::channel().0);
        pane.unregistered = false;
        pane.start(UsageProbe::Skip);
        assert_eq!(
            pane.job.as_ref().map(|job| job.usage),
            Some(UsageProbe::Skip)
        );
        let mut found = inventory(&["codex-main"]);
        found.providers[0].usage = UsageState::NotRequested;
        pane.fed
            .as_ref()
            .unwrap()
            .send(Discovered {
                inventory: found,
                catalogs: Vec::new(),
            })
            .unwrap();
        assert!(pane.poll());
        // The rows are on screen and the probe has started.
        assert_eq!(
            pane.job.as_ref().map(|job| job.usage),
            Some(UsageProbe::Probe)
        );
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(
            rows.iter().any(|row| row.starts_with("codex-main ")),
            "{rows:#?}"
        );
        assert!(rows.contains(&READING_LIMITS.to_owned()), "{rows:#?}");
        assert!(
            rows.iter()
                .any(|row| row.contains("reading quota windows and model catalogs")),
            "{rows:#?}"
        );
        // The probe answers: the windows' bars take the placeholder's place, one under the
        // other, and no third discovery starts.
        let mut probed = inventory(&["codex-main"]);
        probed.providers[0].usage = UsageState::Available;
        probed.providers[0].limits =
            vec![window("codex 5h", 40, None), window("codex 1w", 5, None)];
        pane.fed
            .as_ref()
            .unwrap()
            .send(Discovered {
                inventory: probed,
                catalogs: Vec::new(),
            })
            .unwrap();
        assert!(pane.poll());
        assert!(pane.job.is_none());
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(!rows.contains(&READING_LIMITS.to_owned()), "{rows:#?}");
        let first = rows
            .iter()
            .position(|row| row.starts_with("limit  codex 5h  [########...."))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(
            rows[first + 1].starts_with("limit  codex 1w  [#..."),
            "{rows:#?}"
        );
        // A login that exposes no window says so instead of a bar.
        let mut none = inventory(&["codex-main"]);
        none.providers[0].usage = UsageState::Unsupported;
        pane.inventory = Some(none);
        pane.rebuild();
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(rows.contains(&NO_LIMITS.to_owned()), "{rows:#?}");
    }

    fn inventory(ids: &[&str]) -> ProviderInventory {
        let provider = |id: &&str| ProviderStatus {
            disabled_until: None,
            id: (*id).to_owned(),
            kind: "codex".to_owned(),
            auth_context: "/auth".to_owned(),
            source: "registry".to_owned(),
            status: "authenticated".to_owned(),
            auth_type: "ChatGPT".to_owned(),
            subscription: "-".to_owned(),
            limits: Vec::new(),
            usage: providers::UsageState::NotRequested,
            detail: String::new(),
        };
        ProviderInventory {
            providers: ids.iter().map(provider).collect(),
            registry: None,
            warning: None,
        }
    }

    /// A finished discovery, as the thread `start` spawns would hand it over.
    fn discovered(pane: &mut ProvidersPane, inventory: ProviderInventory) {
        let (sender, receiver) = mpsc::channel();
        pane.job = Some(Job {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            usage: UsageProbe::Skip,
        });
        sender
            .send(Discovered {
                inventory,
                catalogs: Vec::new(),
            })
            .unwrap();
        assert!(pane.poll());
    }

    #[test]
    fn a_discovery_without_the_opened_provider_shows_the_folder() {
        let both = inventory(&["codex-main", "codex-work"]);
        let mut pane = ProvidersPane::with_inventory(both);
        pane.open(Some("codex-work"));
        assert_eq!(pane.rows()[0].text(), "PROVIDERS  codex-work");
        // `af provider remove codex-work` ran: the next discovery no longer lists it.
        discovered(&mut pane, inventory(&["codex-main"]));
        assert_eq!(pane.selected, None);
        assert_eq!(pane.rows()[0].text(), "PROVIDERS  (af provider status)");
        assert!(
            pane.rows()
                .iter()
                .any(|row| row.text().starts_with("codex-main "))
        );
        // A Provider the discovery still lists stays opened.
        pane.open(Some("codex-main"));
        discovered(&mut pane, inventory(&["codex-main"]));
        assert_eq!(pane.selected.as_deref(), Some("codex-main"));
    }

    fn with_kinds() -> ProviderInventory {
        let mut inventory = inventory(&["claude-work", "codex-main", "codex-work"]);
        inventory.providers[0].kind = "claude".into();
        inventory
    }

    #[test]
    fn the_models_are_off_screen_until_m_shows_them() {
        let mut catalogs = fixture_catalogs();
        catalogs.push(ProviderCatalog {
            id: "codex-work".into(),
            kind: Kind::Codex,
            outcome: CatalogOutcome::Unavailable("codex app-server exited 1".into()),
        });
        let mut pane = ProvidersPane::with_inventory(with_kinds()).with_catalogs(catalogs);
        let texts = |pane: &ProvidersPane| pane.rows().iter().map(Row::text).collect::<Vec<_>>();
        // The list: no models row, one hint at the end.
        let rows = texts(&pane);
        assert!(
            !rows.iter().any(|row| row.starts_with("models ")),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.starts_with("  model ")),
            "{rows:#?}"
        );
        assert_eq!(rows[rows.len() - 2], "");
        assert_eq!(rows[rows.len() - 1], MODELS_SHOW);
        // An opened Provider likewise.
        pane.open(Some("claude-work"));
        let rows = texts(&pane);
        assert!(
            !rows.iter().any(|row| row.starts_with("models ")),
            "{rows:#?}"
        );
        assert_eq!(rows[rows.len() - 1], MODELS_SHOW);
        // `m` shows the opened Provider's section: the header, one row per exact ID and what it
        // proves, one blank row under the login's rows.
        assert_eq!(pane.key(Key::Char('m'), 0), Ok(None));
        let rows = texts(&pane);
        let header = rows
            .iter()
            .position(|row| {
                row == "models source claude list_models  version 2.1.300  freshness live  4 exact IDs"
            })
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert_eq!(rows[header - 1], "", "{rows:#?}");
        let fable = rows
            .iter()
            .find(|row| row.starts_with("  model  claude-fable-5-1"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        for part in ["Fable 5.1", "low medium high xhigh max", "aliases fable"] {
            assert!(fable.contains(part), "{part}: {fable}");
        }
        assert!(rows.iter().any(|row| row.contains("af provider doctor")));
        assert_eq!(rows[rows.len() - 1], MODELS_HIDE);
        // Back in the list, every Provider's section shows, a catalog that could not be read
        // as its reason.
        pane.open(None);
        let rows = texts(&pane);
        assert!(
            rows.iter().any(|row| row.starts_with(
                "models source codex app-server model/list  version 0.162.1  freshness \
                 models_cache.json fetched_at 2026-10-15T09:58:12Z  5 exact IDs"
            )),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.starts_with("  model  gpt-6-luna") && !row.contains("ultra")),
            "{rows:#?}"
        );
        assert!(
            rows.contains(
                &"models unavailable: codex app-server exited 1; the static rules apply".to_owned()
            ),
            "{rows:#?}"
        );
        // `m` again hides them; the hint says so.
        assert_eq!(pane.key(Key::Char('m'), 0), Ok(None));
        let rows = texts(&pane);
        assert!(
            !rows.iter().any(|row| row.starts_with("  model ")),
            "{rows:#?}"
        );
        assert_eq!(rows[rows.len() - 1], MODELS_SHOW);
        // Without any catalog there is nothing to show and no hint.
        let bare = ProvidersPane::with_inventory(with_kinds());
        assert!(!texts(&bare).iter().any(|row| row.contains("<m>")));
    }

    #[test]
    fn a_discovery_hands_its_catalogs_to_the_workers_pane() {
        let shared = SharedCatalogs::default();
        let mut pane = ProvidersPane::with_inventory(with_kinds());
        pane.share(shared.clone());
        assert!(shared.of_kind(Kind::Claude).is_empty());
        let (sender, receiver) = mpsc::channel();
        pane.job = Some(Job {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            usage: UsageProbe::Probe,
        });
        sender
            .send(Discovered {
                inventory: with_kinds(),
                catalogs: fixture_catalogs(),
            })
            .unwrap();
        assert!(pane.poll());
        let claude = shared.of_kind(Kind::Claude);
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].id, "claude-work");
        assert!(claude[0].catalog.model("claude-fable-5-1").is_some());
        let codex = shared.of_kind(Kind::Codex);
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].id, "codex-main");
        assert!(codex[0].catalog.model("gpt-6-sol").is_some());
    }

    /// Two Claude Providers whose catalogs differ: the first lists claude-fable-5-1 with every
    /// Claude effort level, the second with low, medium and high only.
    pub(crate) fn two_claude_catalogs() -> Vec<ProviderCatalog> {
        let claude =
            include_str!("../../../tests/fixtures/provider-models/claude-list-models.json");
        let (full, _) = parse_claude_list_models(&serde_json::from_str(claude).unwrap()).unwrap();
        let mut short = full.clone();
        for model in &mut short.models {
            model
                .efforts
                .retain(|level| ["low", "medium", "high"].contains(&level.as_str()));
        }
        short.models.retain(|model| model.id == "claude-fable-5-1");
        let listed = |id: &str, catalog| ProviderCatalog {
            id: id.into(),
            kind: Kind::Claude,
            outcome: CatalogOutcome::Listed(CatalogReport {
                source: catalogs::CLAUDE_SOURCE,
                version: Some("2.1.300".into()),
                fetched_at: None,
                catalog,
                skipped: 0,
            }),
        };
        vec![
            listed("claude-personal", full),
            listed("claude-work", short),
        ]
    }

    /// Each Provider's `models` section shows its own catalog only, and both catalogs are shared
    /// under their own ids though the two Providers are of one kind.
    #[test]
    fn two_providers_of_one_kind_each_show_their_own_models() {
        let mut inventory = inventory(&["claude-personal", "claude-work"]);
        for provider in &mut inventory.providers {
            provider.kind = "claude".into();
        }
        let shared = SharedCatalogs::default();
        let mut pane =
            ProvidersPane::with_inventory(inventory).with_catalogs(two_claude_catalogs());
        pane.share(shared.clone());
        pane.models_open = true;
        let section = |pane: &mut ProvidersPane, id: &str| {
            pane.open(Some(id));
            pane.rows()
                .iter()
                .map(Row::text)
                .filter(|row| row.starts_with("models ") || row.starts_with("  model "))
                .collect::<Vec<_>>()
        };
        let personal = section(&mut pane, "claude-personal");
        assert!(personal[0].ends_with("4 exact IDs"), "{personal:#?}");
        let fable = personal
            .iter()
            .find(|row| row.starts_with("  model  claude-fable-5-1"))
            .unwrap_or_else(|| panic!("{personal:#?}"));
        assert!(fable.contains("low medium high xhigh max"), "{fable}");
        assert!(
            personal
                .iter()
                .any(|row| row.starts_with("  model  claude-opus-5-5"))
        );
        let work = section(&mut pane, "claude-work");
        assert_eq!(work.len(), 2, "{work:#?}");
        assert!(work[0].ends_with("1 exact IDs"), "{work:#?}");
        assert!(
            work[1].starts_with("  model  claude-fable-5-1"),
            "{work:#?}"
        );
        assert!(work[1].contains("low medium high"), "{work:#?}");
        assert!(!work[1].contains("xhigh"), "{work:#?}");
        let ids: Vec<String> = shared
            .of_kind(Kind::Claude)
            .into_iter()
            .map(|catalog| catalog.id)
            .collect();
        assert_eq!(ids, ["claude-personal", "claude-work"]);
    }
}
