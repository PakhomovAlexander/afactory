//! Plan-time validation of every Model binding's model and effort level (ADR-0148).
//!
//! The rules are `review_runner_claude::models`, the one validation authority; this module
//! applies them where a plan is made. The first pass runs the static rules over every Model
//! Worker before any Provider is probed (ADR-0147). At plan time (`af task plan`, `af review
//! plan`) a second pass then reads each Provider's catalog token-free, once per Provider, and
//! prints one line per Model Worker:
//!
//! ```text
//! model WORKER MODEL EFFORT  ok | unverified against the catalog | refused: HINT
//! ```
//!
//! A catalog the harness cannot give is skipped and the binding reported `unverified against the
//! catalog`; a refusal stops the plan. Runs (`af task run`, `af review run`) apply the static
//! rules only and read no catalog, so a run starts no process the plan did not.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use review_runner_claude::hint;
use review_runner_claude::models::{self as rules, Kind, ProviderRef, Verdict};

use crate::providers::models::{CatalogReport, catalog_for};

static PLAN_TIME: AtomicBool = AtomicBool::new(false);
static PRINTED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static CATALOGS: Mutex<BTreeMap<String, Option<CatalogReport>>> = Mutex::new(BTreeMap::new());

/// This process plans: `af task plan` and `af review plan` read catalogs and print model lines.
pub(crate) fn enable_plan_time() {
    PLAN_TIME.store(true, Ordering::Release);
}

fn plan_time() -> bool {
    PLAN_TIME.load(Ordering::Acquire)
}

/// One Model binding to check: the Worker or node it belongs to, its Provider label and kind,
/// and its model and effort level.
pub(crate) struct Binding<'a> {
    /// How a refusal names the binding's owner, such as `Worker team/review`.
    pub(crate) owner: String,
    /// The Worker or node the plan line names.
    pub(crate) label: &'a str,
    pub(crate) provider: Option<&'a str>,
    pub(crate) kind: &'a str,
    pub(crate) model: &'a str,
    pub(crate) effort: &'a str,
}

impl Binding<'_> {
    fn kind(&self) -> Result<Kind, String> {
        Kind::parse(self.kind).ok_or_else(|| format!("{}: unsupported Provider kind", self.owner))
    }

    fn provider(&self, kind: Kind) -> ProviderRef<'_> {
        ProviderRef {
            id: self.provider,
            kind,
        }
    }

    fn refusal(&self, hint: &str) -> String {
        match self.provider {
            Some(provider) => format!("{} on Provider {provider}: {hint}", self.owner),
            None => format!("{}: {hint}", self.owner),
        }
    }

    /// The plan line for this binding and its verdict.
    pub(crate) fn line(&self, verdict: &str) -> String {
        format!(
            "model {} {} {}  {verdict}",
            shown_label(self.label),
            hint::shown(self.model),
            hint::shown(self.effort)
        )
    }
}

/// The first pass: the static rules, before any Provider is probed. At plan time a refusal is
/// also printed as its plan line.
pub(crate) fn static_rules(binding: &Binding<'_>) -> Result<(), String> {
    let kind = binding.kind()?;
    rules::check_static(kind, binding.model, Some(binding.effort)).map_err(|refusal| {
        let hint = refusal.hint(binding.provider(kind), binding.model, Some(binding.effort));
        if plan_time() {
            print_once(&binding.line(&format!("refused: {hint}")));
        }
        binding.refusal(&hint)
    })
}

/// The second pass at plan time: each Provider's catalog, read once, then each binding's verdict
/// as a plan line, on stderr. Outside plan time it does nothing.
pub(crate) fn catalog_rules(bindings: &[Binding<'_>]) -> Result<(), String> {
    if !plan_time() {
        return Ok(());
    }
    for (binding, verdict) in verdicts(bindings)? {
        print_once(&binding.line(&verdict_text(binding, &verdict)?));
        if let Verdict::Refused(refusal) = verdict {
            let kind = binding.kind()?;
            let hint = refusal.hint(binding.provider(kind), binding.model, Some(binding.effort));
            return Err(binding.refusal(&hint));
        }
    }
    Ok(())
}

/// Every binding's verdict at plan time, with each Provider's catalog read once; a binding
/// without a Provider is judged by the static rules alone.
pub(crate) fn verdicts<'b, 'a>(
    bindings: &'b [Binding<'a>],
) -> Result<Vec<(&'b Binding<'a>, Verdict)>, String> {
    let mut verdicts = Vec::new();
    for binding in bindings {
        if let Some(provider) = binding.provider {
            crate::providers::availability::check_id(provider)?;
        }
        let kind = binding.kind()?;
        let catalog = binding.provider.and_then(catalog_of);
        let verdict = rules::check(
            kind,
            catalog.as_ref().map(|report| &report.catalog),
            binding.model,
            Some(binding.effort),
        );
        verdicts.push((binding, verdict));
    }
    Ok(verdicts)
}

/// A verdict as a plan line prints it; a refusal as its full hint.
pub(crate) fn verdict_text(binding: &Binding<'_>, verdict: &Verdict) -> Result<String, String> {
    let kind = binding.kind()?;
    Ok(match verdict {
        Verdict::Refused(refusal) => format!(
            "refused: {}",
            refusal.hint(binding.provider(kind), binding.model, Some(binding.effort))
        ),
        other => other.label(kind, binding.model, Some(binding.effort)),
    })
}

/// A Worker or node name as a plan line prints it: a package name (`team/review`) or a node ID
/// verbatim, anything else as grammar G prints it.
fn shown_label(label: &str) -> String {
    let named = !label.is_empty()
        && label.len() <= 128
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._:/".contains(&b));
    if named {
        label.to_owned()
    } else {
        hint::shown(label)
    }
}

fn catalog_of(provider: &str) -> Option<CatalogReport> {
    let mut catalogs = CATALOGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    catalogs
        .entry(provider.to_owned())
        .or_insert_with(|| catalog_for(provider))
        .clone()
}

fn print_once(line: &str) {
    let mut printed = PRINTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if printed.insert(line.to_owned()) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding<'a>(model: &'a str, effort: &'a str) -> Binding<'a> {
        Binding {
            owner: "Worker team/review".into(),
            label: "team/review",
            provider: None,
            kind: "claude",
            model,
            effort,
        }
    }

    #[test]
    fn the_first_pass_refuses_with_the_owner_and_the_hint() {
        let refusal = static_rules(&binding("fable", "xhigh")).unwrap_err();
        assert!(
            refusal.starts_with("Worker team/review: af refused the binding of model fable"),
            "{refusal}"
        );
        assert!(refusal.contains("claude-fable-5-1"), "{refusal}");
        assert!(static_rules(&binding("claude-fable-5-1", "xhigh")).is_ok());
    }

    #[test]
    fn a_binding_without_a_catalog_is_unverified_and_its_line_names_it() {
        let checked = binding("claude-opus-4-8", "high");
        let verdicts = verdicts(std::slice::from_ref(&checked)).unwrap();
        assert_eq!(verdicts[0].1, Verdict::Unverified);
        assert_eq!(
            checked.line(&verdict_text(&checked, &verdicts[0].1).unwrap()),
            "model team/review claude-opus-4-8 high  unverified against the catalog"
        );
    }
}
