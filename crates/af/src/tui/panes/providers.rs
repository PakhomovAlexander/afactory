//! Providers: the inventory `af provider status` prints, from `providers::discover_with_cancel`,
//! and each Provider's model catalog `af provider models` prints (ADR-0148). Discovery runs one
//! bounded authentication check per context, then reads each probed Provider's catalog
//! token-free, and `R` adds the bounded usage probe; all of it runs on a thread the event loop
//! polls and cancels, so a key is never waiting on a Provider. A Provider's `models` section is
//! folded to its header line in the list and unfolds to one row per exact ID when the Provider
//! is opened or `m` unfolds every section, and shows that Provider's own catalog only. The
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
use crate::providers::{self, ProviderInventory, ProviderStatus, UsageProbe};
use crate::tui::keymap::Key;
use crate::tui::paint::{Paint, Span, Tone};
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

/// Columns of a limit bar.
const BAR: usize = 10;

/// What `d` says where no Provider is selected.
pub(crate) const REMOVE_NEEDS_A_PROVIDER: &str =
    "d removes a Provider: select one in the bar, or open it";

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
    /// What the shown inventory was probed with.
    usage: UsageProbe,
    job: Option<Job>,
    /// Discover on load. A pane over an inventory already taken never runs a Provider CLI
    /// unless `R` asks.
    discover: bool,
    selected: Option<String>,
    rows: Vec<Row>,
    spinner: usize,
    /// Each Provider's catalog, as the last complete discovery read it.
    catalogs: Vec<ProviderCatalog>,
    /// `m` unfolds every Provider's `models` section in the list.
    models_open: bool,
    shared: SharedCatalogs,
}

impl ProvidersPane {
    /// A pane that discovers the machine's providers when it first loads.
    pub(crate) fn discovering() -> ProvidersPane {
        ProvidersPane {
            inventory: None,
            usage: UsageProbe::Skip,
            job: None,
            discover: true,
            selected: None,
            rows: Vec::new(),
            spinner: 0,
            catalogs: Vec::new(),
            models_open: false,
            shared: SharedCatalogs::default(),
        }
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
    pub(crate) fn with_inventory(inventory: ProviderInventory, usage: UsageProbe) -> ProvidersPane {
        let mut pane = ProvidersPane::discovering();
        pane.inventory = Some(inventory);
        pane.usage = usage;
        pane.discover = false;
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
            Some(provider) if providers::is_ambient_candidate(provider) => Err(format!(
                "{id} is discovered, not registered: there is no registry entry to remove"
            )),
            Some(_) => Ok(format!("provider remove {}", shell_words::quote(id))),
            None => Err(format!("{id} is not a listed Provider")),
        }
    }

    fn start(&mut self, usage: UsageProbe) {
        self.cancel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let inventory = providers::discover_with_cancel(&flag, usage);
            // A CLI that cannot start, or was not probed, has no catalog to read.
            let ids: Vec<String> = inventory
                .providers
                .iter()
                .filter(|provider| {
                    !matches!(
                        provider.status.as_str(),
                        "installation failed" | "incompatible" | "not probed" | "unavailable"
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

    fn rebuild(&mut self) {
        let title = match &self.selected {
            Some(id) => format!("PROVIDERS  {id}"),
            None => "PROVIDERS  (af provider status)".to_owned(),
        };
        let mut rows = vec![Row::painted(title, Paint::Title), Row::blank()];
        match &self.inventory {
            Some(inventory) => {
                let selected = self.selected.as_deref();
                let models = Models {
                    catalogs: &self.catalogs,
                    open: self.models_open,
                };
                inventory_rows(inventory, selected, self.usage, &models, &mut rows);
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
                    "probing subscription and quota windows and model catalogs, bounded"
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
    /// removed until the user runs that line. `m` folds or unfolds the `models` sections.
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
            Some(_) => "d remove  R probe usage  y yank id  gf registry  Tab bar  :cmd  q quit",
            None => {
                "j/k move  m models  R probe usage  y yank id  gf registry  Tab bar  :cmd  q quit"
            }
        }
    }

    /// `R` runs the bounded usage probe `af provider status --usage` runs, and reads every
    /// catalog again.
    fn refresh(&mut self, _scope: &Scope) -> Result<(), String> {
        self.start(UsageProbe::Probe);
        Ok(())
    }

    /// A command may have set up or removed a Provider: discover again, never probing, since a
    /// probe may be charged and only `R` asks for one. A fixed inventory stays as it is.
    fn reread(&mut self, _scope: &Scope) -> Result<(), String> {
        if self.discover {
            self.start(UsageProbe::Skip);
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
                self.usage = usage;
                self.job = None;
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
    /// inventory and its usage stay on screen and `R` can probe again.
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

/// The catalogs a rebuild shows, and whether `m` unfolded them.
struct Models<'a> {
    catalogs: &'a [ProviderCatalog],
    open: bool,
}

/// The `af provider status` table for the selected provider, or for all of them, then each
/// provider's limit bars, note and models, and the setup line for an ambient candidate.
fn inventory_rows(
    inventory: &ProviderInventory,
    selected: Option<&str>,
    usage: UsageProbe,
    models: &Models<'_>,
    rows: &mut Vec<Row>,
) {
    if let Some(warning) = &inventory.warning {
        rows.push(Row::error("warning", warning));
    }
    if inventory.providers.is_empty() {
        let none = "No supported provider CLI is installed and no provider registry entries exist";
        rows.push(Row::plain(none));
        return;
    }
    let mut ids = BTreeSet::new();
    for provider in &inventory.providers {
        ids.insert(provider.id.clone());
    }
    rows.push(Row::painted(providers::status_table_header(), Paint::Title));
    for provider in &inventory.providers {
        if selected.is_none_or(|id| id == provider.id) {
            provider_rows(provider, selected.is_some(), &ids, rows);
            let catalog = models.catalogs.iter().find(|c| c.id == provider.id);
            if let Some(catalog) = catalog {
                model_rows(catalog, selected.is_some() || models.open, rows);
            }
        }
    }
    if usage == UsageProbe::Skip {
        rows.push(Row::blank());
        let hint = "Subscription and quota windows are not probed by default; R probes them.";
        rows.push(Row::plain(hint));
    }
}

fn provider_rows(
    provider: &ProviderStatus,
    alone: bool,
    ids: &BTreeSet<String>,
    rows: &mut Vec<Row>,
) {
    let ambient = providers::is_ambient_candidate(provider);
    let paint = if ambient { Paint::Muted } else { Paint::Plain };
    rows.push(status_row(provider, paint));
    if alone {
        rows.push(Row::blank());
    }
    for limit in &provider.limits {
        let used = bar(limit.used_percent);
        let text = providers::format_limit(limit);
        rows.push(Row::painted(format!("limit  {used}  {text}"), paint));
    }
    if !provider.detail.is_empty() {
        rows.push(Row::painted(format!("note   {}", provider.detail), paint));
    }
    if ambient {
        let hint = providers::setup_hint(&provider.kind, ids);
        rows.push(Row::painted(hint, Paint::Muted));
    }
}

/// A Provider's `models` section: the header line of `af provider models`, then, unfolded, one
/// row per exact ID (ID, display name, effort levels, aliases); a catalog that could not be read
/// says why.
fn model_rows(catalog: &ProviderCatalog, unfolded: bool, rows: &mut Vec<Row>) {
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
    let fold = if unfolded { "" } else { "  (m unfolds)" };
    rows.push(Row::plain(format!(
        "models {}  {count} exact IDs{fold}",
        report.header()
    )));
    if !unfolded {
        return;
    }
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

/// `used_percent` as a bar of `#` used and `.` left.
fn bar(used_percent: u8) -> String {
    let used = (usize::from(used_percent.min(100)) * BAR + 50) / 100;
    format!("[{}{}]", "#".repeat(used), ".".repeat(BAR - used))
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

    #[test]
    fn limit_bars_round_to_the_nearest_column() {
        assert_eq!(bar(0), "[..........]");
        assert_eq!(bar(41), "[####......]");
        assert_eq!(bar(68), "[#######...]");
        assert_eq!(bar(100), "[##########]");
        assert_eq!(bar(255), "[##########]");
    }

    fn inventory(ids: &[&str]) -> ProviderInventory {
        let provider = |id: &&str| ProviderStatus {
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
        let mut pane = ProvidersPane::with_inventory(both, UsageProbe::Skip);
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
    fn each_provider_shows_its_models_folded_in_the_list_and_unfolded_when_opened() {
        let mut catalogs = fixture_catalogs();
        catalogs.push(ProviderCatalog {
            id: "codex-work".into(),
            kind: Kind::Codex,
            outcome: CatalogOutcome::Unavailable("codex app-server exited 1".into()),
        });
        let mut pane =
            ProvidersPane::with_inventory(with_kinds(), UsageProbe::Skip).with_catalogs(catalogs);
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(
            rows.contains(
                &"models source claude list_models  version 2.1.300  freshness live  4 exact IDs  (m unfolds)"
                    .to_owned()
            ),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.starts_with(
                "models source codex app-server model/list  version 0.162.1  freshness \
                 models_cache.json fetched_at 2026-10-15T09:58:12Z  5 exact IDs"
            )),
            "{rows:#?}"
        );
        assert!(
            rows.contains(
                &"models unavailable: codex app-server exited 1; the static rules apply".to_owned()
            ),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.starts_with("  model ")),
            "folded"
        );
        // Opened, a Provider's section unfolds to one row per exact ID and what it proves.
        pane.open(Some("claude-work"));
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        let fable = rows
            .iter()
            .find(|row| row.starts_with("  model  claude-fable-5-1"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        for part in ["Fable 5.1", "low medium high xhigh max", "aliases fable"] {
            assert!(fable.contains(part), "{part}: {fable}");
        }
        assert!(rows.iter().any(|row| row.contains("af provider doctor")));
        // `m` unfolds every section in the list, and folds them again.
        pane.open(None);
        assert_eq!(pane.key(Key::Char('m'), 0), Ok(None));
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(
            rows.iter()
                .any(|row| row.starts_with("  model  gpt-6-luna") && !row.contains("ultra")),
            "{rows:#?}"
        );
        assert_eq!(pane.key(Key::Char('m'), 0), Ok(None));
        assert!(
            !pane
                .rows()
                .iter()
                .any(|row| row.text().starts_with("  model "))
        );
    }

    #[test]
    fn a_discovery_hands_its_catalogs_to_the_workers_pane() {
        let shared = SharedCatalogs::default();
        let mut pane = ProvidersPane::with_inventory(with_kinds(), UsageProbe::Skip);
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
        let mut pane = ProvidersPane::with_inventory(inventory, UsageProbe::Skip)
            .with_catalogs(two_claude_catalogs());
        pane.share(shared.clone());
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
