//! Providers: the inventory `af provider status` prints, from `providers::discover_with_cancel`.
//! Discovery runs one bounded authentication check per context and `R` adds the bounded usage
//! probe; both run on a thread the event loop polls, so a key is never waiting on a Provider.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::{Effect, Pane, Row, SPINNER};
use crate::providers::{self, ProviderInventory, ProviderStatus, UsageProbe};
use crate::tui::keymap::Key;
use crate::tui::paint::Paint;
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

/// Columns of a limit bar.
const BAR: usize = 10;

/// What `d` says where no Provider is selected.
pub(crate) const REMOVE_NEEDS_A_PROVIDER: &str =
    "d removes a Provider: select one in the bar, or open it";

struct Job {
    receiver: Receiver<ProviderInventory>,
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
        }
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
            let _ = sender.send(providers::discover_with_cancel(&flag, usage));
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
                inventory_rows(inventory, selected, self.usage, &mut rows);
            }
            None => rows.push(Row::plain("Discovering providers.")),
        }
        if let Some(job) = &self.job {
            let spinner = SPINNER[self.spinner % SPINNER.len()];
            let what = match job.usage {
                UsageProbe::Skip => "checking authentication, bounded per context",
                UsageProbe::Probe => "probing subscription and quota windows, bounded",
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
    /// removed until the user runs that line.
    fn key(&mut self, key: Key, _row: usize) -> Result<Option<Effect>, String> {
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
            None => "j/k move  R probe usage  y yank id  gf registry  Tab bar  :cmd  q quit",
        }
    }

    /// `R` runs the bounded usage probe `af provider status --usage` runs.
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
            Ok(inventory) => {
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

/// The `af provider status` table for the selected provider, or for all of them, then each
/// provider's limit bars and note, and the setup line for an ambient candidate.
fn inventory_rows(
    inventory: &ProviderInventory,
    selected: Option<&str>,
    usage: UsageProbe,
    rows: &mut Vec<Row>,
) {
    if let Some(warning) = &inventory.warning {
        rows.push(Row::painted(format!("warning: {warning}"), Paint::Error));
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
    rows.push(Row::painted(providers::status_table_row(provider), paint));
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

/// `used_percent` as a bar of `#` used and `.` left.
fn bar(used_percent: u8) -> String {
    let used = (usize::from(used_percent.min(100)) * BAR + 50) / 100;
    format!("[{}{}]", "#".repeat(used), ".".repeat(BAR - used))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        sender.send(inventory).unwrap();
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
}
