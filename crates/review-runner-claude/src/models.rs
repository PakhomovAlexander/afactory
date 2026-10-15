//! The one validation authority for the model and effort level a Model binding names, for every
//! Provider kind (ADR-0148). It lives beside grammar G ([`hint::printable`]), which it applies
//! first, and every refusal it makes is worded by [`hint::compose`], so plan time and runtime say
//! the same thing. `af provider models`, `TaskProviderIdentity::execution`, `af task plan`, `af
//! review plan` and `af review run`, `af onboard`, the TUI and the package argument parsers of
//! both adapters all call it; the Codex adapter depends on this crate for it.
//!
//! Two kinds of rule:
//!
//! - **Static rules**, which need no catalog ([`check_static`]): grammar G on the model and the
//!   effort level; an alias Claude Code documents (`fable`, `opus`, `sonnet`, `haiku`) is refused
//!   with the exact ID it resolves to; the versioned-ID rule `execution()` has always applied; a
//!   spelling that differs from a known exact ID only in case or separators
//!   (`claude-fable-5.1`) is refused with that ID; the effort level must be in the Provider
//!   kind's vocabulary ([`Kind::efforts`]).
//! - **Catalog rules**, when the harness's own catalog is at hand ([`check`]): an alias the
//!   catalog lists is refused with the exact ID it resolves to today, a near miss of a listed ID
//!   is refused with the nearest IDs, and the effort level must be one the model lists. An ID the
//!   catalog does not list that passes the static rules is accepted as unverified: a catalog is
//!   what the harness offers, never every ID its API accepts.
//!
//! af never substitutes a model or an effort level: every rule refuses or accepts the spelling
//! as written.

use crate::hint;

/// The Claude effort levels `claude --help` documents for `--effort`.
pub const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The Codex reasoning-effort levels af passes as `-c model_reasoning_effort="…"`.
pub const CODEX_EFFORTS: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "ultra"];

/// The aliases Claude Code's documentation resolves, and the exact ID each resolves to today.
const CLAUDE_ALIASES: [(&str, &str); 4] = [
    ("fable", "claude-fable-5-1"),
    ("opus", "claude-opus-5-5"),
    ("sonnet", "claude-sonnet-5-5"),
    ("haiku", "claude-haiku-5-5"),
];

/// The versioned-ID rule's words, unchanged since `execution()` first applied it.
pub const VERSIONED_RULE: &str = "Task Model binding requires an explicit versioned model ID";

/// What the versioned-ID rule asks of an exact ID.
const VERSIONED_DETAIL: &str = "an exact ID holds a '-' and a digit, is at most 256 bytes, \
holds no whitespace and does not end in -latest";

/// The most catalog IDs a refusal or a resolution names as nearest.
const MAX_NEAREST: usize = 5;

/// A Provider kind, and the vocabulary af accepts for it without a catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Claude,
    Codex,
}

impl Kind {
    pub fn parse(value: &str) -> Option<Kind> {
        match value {
            "claude" => Some(Kind::Claude),
            "codex" => Some(Kind::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
        }
    }

    /// The effort levels af accepts for this kind before any catalog is read.
    pub fn efforts(self) -> &'static [&'static str] {
        match self {
            Kind::Claude => &CLAUDE_EFFORTS,
            Kind::Codex => &CODEX_EFFORTS,
        }
    }

    /// The aliases this kind's harness documents, each with the exact ID it resolves to today.
    pub fn documented_aliases(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Kind::Claude => &CLAUDE_ALIASES,
            Kind::Codex => &[],
        }
    }

    fn harness(self) -> &'static str {
        match self {
            Kind::Claude => "Claude Code",
            Kind::Codex => "Codex",
        }
    }
}

/// The Provider a binding is checked for: its registry ID when af knows one (a package parser
/// knows only the kind), and its kind.
#[derive(Clone, Copy, Debug)]
pub struct ProviderRef<'a> {
    pub id: Option<&'a str>,
    pub kind: Kind,
}

/// One exact model ID a harness's catalog lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    /// The harness's own name for the model, such as `Fable 5.1` or `GPT-6.1-Sol`.
    pub display_name: String,
    /// Every other name the harness shows for it, matched case-insensitively by `--resolve`.
    pub other_names: Vec<String>,
    /// The aliases that resolve to this exact ID.
    pub aliases: Vec<String>,
    /// The effort levels the model lists, in the harness's order.
    pub efforts: Vec<String>,
    /// The effort level the harness marks as the default, when it reports one.
    pub default_effort: Option<String>,
    /// The harness lists the model but does not offer it in its picker.
    pub hidden: bool,
}

/// What a harness's catalog lists: what af recognises for one Provider, never every ID its API
/// accepts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    /// The model whose exact ID is `id`.
    pub fn model(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// The model the alias `spelling` resolves to.
    pub fn alias(&self, spelling: &str) -> Option<&CatalogModel> {
        self.models
            .iter()
            .find(|model| model.aliases.iter().any(|alias| alias == spelling))
    }

    /// Add each alias the harness documents whose exact ID the catalog lists and that the
    /// catalog itself does not already list: Claude Code's picker offers `claude-fable-5-1` but
    /// not `fable`, which its `--model` still accepts.
    pub fn with_documented_aliases(mut self, kind: Kind) -> Catalog {
        for (alias, exact) in kind.documented_aliases() {
            if self.alias(alias).is_some() || self.model(alias).is_some() {
                continue;
            }
            if let Some(model) = self.models.iter_mut().find(|model| model.id == *exact) {
                model.aliases.push((*alias).to_owned());
            }
        }
        self
    }
}

/// The rule or catalog fact a refused binding failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The value of `option` (`--model` or `--effort`) is outside grammar G.
    NotPrintable { option: &'static str },
    /// The model is an alias; af binds the exact ID it resolves to, never the alias.
    Alias { exact: String, documented: bool },
    /// The model differs from a known exact ID only in case or separators, or is a near miss of
    /// one the catalog lists.
    Misspelled,
    /// The model is not an explicit versioned ID.
    Unversioned,
    /// The effort level is not one this Provider kind accepts.
    EffortVocabulary,
    /// The catalog lists `model` with other effort levels (none at all when `levels` is empty).
    EffortNotListed { model: String, levels: Vec<String> },
}

/// A refused binding: the rule it failed and the nearest spellings this Provider accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rule: Rule,
    pub nearest: Vec<String>,
}

impl Refusal {
    fn new(rule: Rule, nearest: Vec<String>) -> Refusal {
        Refusal { rule, nearest }
    }

    /// The rule or catalog fact, in af's words. A value outside G is named only by its length.
    pub fn fact(&self, kind: Kind, model: &str, effort: Option<&str>) -> String {
        let effort = effort.unwrap_or("");
        match &self.rule {
            Rule::NotPrintable { option } => {
                let value = if *option == "--effort" { effort } else { model };
                format!(
                    "its {option} value ({}) is not a model spelling af accepts: {}",
                    byte_length(value.len()),
                    hint::PRINTABLE_RULE
                )
            }
            Rule::Alias { exact, documented } => format!(
                "`{}` is an alias that {} resolves to the exact ID {} today, and af binds exact \
                 IDs, never an alias",
                hint::shown(model),
                if *documented {
                    format!("{}'s documentation says", kind.harness())
                } else {
                    "the catalog says".to_owned()
                },
                hint::shown(exact)
            ),
            Rule::Misspelled => format!(
                "`{}` is not an exact ID this Provider lists",
                hint::shown(model)
            ),
            Rule::Unversioned => format!("{VERSIONED_RULE}: {VERSIONED_DETAIL}"),
            Rule::EffortVocabulary => format!(
                "the effort level `{}` is not one {} accepts: {}",
                hint::shown(effort),
                kind.harness(),
                kind.efforts().join(", ")
            ),
            Rule::EffortNotListed { model, levels } if levels.is_empty() => format!(
                "the catalog lists {} with no effort levels, so it takes no effort level `{}`",
                hint::shown(model),
                hint::shown(effort)
            ),
            Rule::EffortNotListed { model, levels } => format!(
                "the catalog lists the effort levels {} for {}, and `{}` is not one of them",
                shown_list(levels),
                hint::shown(model),
                hint::shown(effort)
            ),
        }
    }

    /// The hint for this refusal, through the one hint function (ADR-0147, ADR-0148).
    pub fn hint(&self, provider: ProviderRef<'_>, model: &str, effort: Option<&str>) -> String {
        let fact = self.fact(provider.kind, model, effort);
        hint::compose(
            hint::BoundModel {
                model,
                effort,
                provider: Some(provider),
            },
            &hint::ModelOutcome::NotAccepted {
                fact: &fact,
                nearest: &self.nearest,
            },
        )
    }

    /// The short verdict the TUI's Workers pane shows, in the printable ASCII the browser draws.
    pub fn label(&self, kind: Kind, model: &str, effort: Option<&str>) -> String {
        match &self.rule {
            Rule::Alias { exact, .. } => format!("alias -> {}", hint::shown(exact)),
            Rule::EffortVocabulary => format!("effort not listed: {}", kind.efforts().join(", ")),
            Rule::EffortNotListed { levels, .. } if levels.is_empty() => {
                "effort not listed: none".to_owned()
            }
            Rule::EffortNotListed { levels, .. } => {
                format!("effort not listed: {}", shown_list(levels))
            }
            _ => format!("refused: {}", self.fact(kind, model, effort)),
        }
    }
}

/// What the validation authority concluded about one binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The catalog lists the exact ID and, when one is bound, the effort level.
    Ok,
    /// The binding passes the static rules, and no catalog was at hand or the catalog does not
    /// list the ID: a custom or deployment ID is accepted, and only `af provider doctor` proves
    /// that the account can run it.
    Unverified,
    Refused(Refusal),
}

impl Verdict {
    /// The verdict as `af task plan`, `af review plan` and the TUI print it; a refusal as its
    /// short label.
    pub fn label(&self, kind: Kind, model: &str, effort: Option<&str>) -> String {
        match self {
            Verdict::Ok => "ok".to_owned(),
            Verdict::Unverified => "unverified against the catalog".to_owned(),
            Verdict::Refused(refusal) => refusal.label(kind, model, effort),
        }
    }
}

/// The static rules, in their one order: grammar G on the model and the effort level, so a value
/// outside G is refused with G's words whatever else it fails; a documented alias; a spelling
/// that is a case or separator variant of a known exact ID; the versioned-ID rule; the kind's
/// effort vocabulary. `effort` is `None` only for a package that binds no effort level.
pub fn check_static(kind: Kind, model: &str, effort: Option<&str>) -> Result<(), Refusal> {
    if !hint::printable(model) {
        return Err(Refusal::new(
            Rule::NotPrintable { option: "--model" },
            vec![],
        ));
    }
    if let Some(effort) = effort
        && !hint::printable(effort)
    {
        return Err(Refusal::new(
            Rule::NotPrintable { option: "--effort" },
            vec![],
        ));
    }
    if let Some((_, exact)) = kind
        .documented_aliases()
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(model))
    {
        return Err(Refusal::new(
            Rule::Alias {
                exact: (*exact).to_owned(),
                documented: true,
            },
            vec![(*exact).to_owned()],
        ));
    }
    let variants: Vec<String> = known_ids(kind)
        .filter(|id| *id != model && normalized(id) == normalized(model))
        .map(str::to_owned)
        .collect();
    if !variants.is_empty() {
        return Err(Refusal::new(Rule::Misspelled, variants));
    }
    if !versioned(model) {
        let nearest = nearest_of(known_ids(kind), model);
        return Err(Refusal::new(Rule::Unversioned, nearest));
    }
    match effort {
        Some(effort) => check_effort(kind, effort),
        None => Ok(()),
    }
}

/// The effort-level rules alone, for a package that binds an effort level and leaves the model to
/// the harness: grammar G, then the kind's effort vocabulary. Its refusal's hint is composed with
/// an empty model, which the hint names as the harness's default model.
pub fn check_effort(kind: Kind, effort: &str) -> Result<(), Refusal> {
    if !hint::printable(effort) {
        return Err(Refusal::new(
            Rule::NotPrintable { option: "--effort" },
            vec![],
        ));
    }
    if !kind.efforts().contains(&effort) {
        let nearest = nearest_of(kind.efforts().iter().copied(), effort);
        return Err(Refusal::new(Rule::EffortVocabulary, nearest));
    }
    Ok(())
}

/// The static rules, then the catalog's when one is at hand. Without a catalog a binding that
/// passes the static rules is [`Verdict::Unverified`].
pub fn check(kind: Kind, catalog: Option<&Catalog>, model: &str, effort: Option<&str>) -> Verdict {
    // A catalog alias is named with the exact ID the catalog resolves it to today, ahead of the
    // documented one and of the versioned-ID rule an alias also fails.
    if let Some(catalog) = catalog
        && hint::printable(model)
        && effort.is_none_or(hint::printable)
        && let Some(target) = catalog.alias(model)
        && catalog.model(model).is_none()
    {
        return Verdict::Refused(Refusal::new(
            Rule::Alias {
                exact: target.id.clone(),
                documented: kind
                    .documented_aliases()
                    .iter()
                    .any(|(alias, _)| *alias == model),
            },
            vec![target.id.clone()],
        ));
    }
    if let Err(refusal) = check_static(kind, model, effort) {
        return Verdict::Refused(refusal);
    }
    let Some(catalog) = catalog else {
        return Verdict::Unverified;
    };
    let Some(listed) = catalog.model(model) else {
        let misses = misspelled(catalog, model);
        return if misses.is_empty() {
            Verdict::Unverified
        } else {
            Verdict::Refused(Refusal::new(Rule::Misspelled, misses))
        };
    };
    match effort {
        Some(effort) if !listed.efforts.iter().any(|level| level == effort) => {
            let nearest = nearest_of(listed.efforts.iter().map(String::as_str), effort);
            Verdict::Refused(Refusal::new(
                Rule::EffortNotListed {
                    model: listed.id.clone(),
                    levels: listed.efforts.clone(),
                },
                nearest,
            ))
        }
        _ => Verdict::Ok,
    }
}

/// The nearest exact IDs for a spelling af did not recognise, without a catalog: the exact IDs
/// the kind's documented aliases resolve to that differ from it only in case or separators. The
/// runtime hint for a model Claude Code rejected names them.
pub fn static_nearest(kind: Kind, model: &str) -> Vec<String> {
    known_ids(kind)
        .filter(|id| *id != model && normalized(id) == normalized(model))
        .map(str::to_owned)
        .collect()
}

/// How `af provider models --resolve` read a human spelling of a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// An exact ID the catalog lists: confirmed.
    Exact { id: String },
    /// An alias, and the exact ID it resolves to.
    Alias { id: String },
    /// A display name, matched case-insensitively, and the exact ID it names.
    DisplayName { id: String },
    /// A family-and-version form such as `Fable 5.1` or `GPT-6 Sol`, and the exact ID it names.
    FamilyVersion { id: String },
    /// Not an ID the catalog lists, and a near miss of these listed IDs.
    Misspelled { nearest: Vec<String> },
    /// An exact ID the catalog does not list, and whether it passes af's static rules.
    NotListed { rule: Option<Refusal> },
}

impl Resolution {
    /// The exact ID the spelling resolved to, when it resolved to one the catalog lists.
    pub fn id(&self) -> Option<&str> {
        match self {
            Resolution::Exact { id }
            | Resolution::Alias { id }
            | Resolution::DisplayName { id }
            | Resolution::FamilyVersion { id } => Some(id),
            Resolution::Misspelled { .. } | Resolution::NotListed { .. } => None,
        }
    }
}

/// Read a human spelling of a model against a catalog: an exact ID, an alias, a display name
/// (case-insensitively), or a family-and-version form; otherwise a misspelling of the nearest
/// listed IDs, or an exact ID the catalog does not list.
pub fn resolve(kind: Kind, catalog: &Catalog, spelling: &str) -> Resolution {
    let spelling = spelling.trim();
    if let Some(model) = catalog.model(spelling) {
        return Resolution::Exact {
            id: model.id.clone(),
        };
    }
    if let Some(model) = catalog.alias(spelling) {
        return Resolution::Alias {
            id: model.id.clone(),
        };
    }
    if let Some((_, exact)) = kind
        .documented_aliases()
        .iter()
        .find(|(alias, _)| *alias == spelling)
        && catalog.model(exact).is_some()
    {
        return Resolution::Alias {
            id: (*exact).to_owned(),
        };
    }
    if let Some(model) = catalog.models.iter().find(|model| {
        std::iter::once(&model.display_name)
            .chain(&model.other_names)
            .any(|name| name.eq_ignore_ascii_case(spelling))
    }) {
        return Resolution::DisplayName {
            id: model.id.clone(),
        };
    }
    // A human form carries a space; one without is read as an attempt at an exact ID, so a
    // separator variant of one is a misspelling, not a resolution.
    if spelling.contains(char::is_whitespace) {
        let wanted = normalized(spelling);
        if let Some(model) = catalog.models.iter().find(|model| {
            let id = normalized(&model.id);
            id == wanted
                || id
                    .split_once('-')
                    .is_some_and(|(_, rest)| rest == wanted && vendor_prefixed(&model.id))
        }) {
            return Resolution::FamilyVersion {
                id: model.id.clone(),
            };
        }
    }
    let misses = misspelled(catalog, spelling);
    if !misses.is_empty() {
        return Resolution::Misspelled { nearest: misses };
    }
    Resolution::NotListed {
        rule: check_static(kind, spelling, None).err(),
    }
}

/// The listed IDs `model` is a near miss of: the same ID in another case or with other
/// separators, or within two edits of one whose digits it shares.
fn misspelled(catalog: &Catalog, model: &str) -> Vec<String> {
    let wanted = normalized(model);
    let digits = |value: &str| {
        value
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
    };
    let near = catalog.models.iter().any(|listed| {
        listed.id != model
            && (normalized(&listed.id) == wanted
                || (digits(&listed.id) == digits(model)
                    && distance(&listed.id.to_ascii_lowercase(), &model.to_ascii_lowercase()) <= 2))
    });
    if !near {
        return Vec::new();
    }
    nearest_of(
        catalog.models.iter().map(|listed| listed.id.as_str()),
        model,
    )
}

/// The candidates nearest `spelling`: those equal to it but for case and separators, of the same
/// family (its name without the version), or within two edits; closest first, at most five.
pub fn nearest_of<'a>(candidates: impl Iterator<Item = &'a str>, spelling: &str) -> Vec<String> {
    let wanted = normalized(spelling);
    let family = family(spelling);
    let lower = spelling.to_ascii_lowercase();
    let mut found: Vec<(usize, String)> = candidates
        .filter(|candidate| *candidate != spelling)
        .filter_map(|candidate| {
            let edits = distance(&candidate.to_ascii_lowercase(), &lower);
            let same_family = !family.is_empty() && self::family(candidate) == family;
            (normalized(candidate) == wanted || same_family || edits <= 2)
                .then(|| (edits, candidate.to_owned()))
        })
        .collect();
    found.sort();
    found.dedup();
    found
        .into_iter()
        .take(MAX_NEAREST)
        .map(|(_, id)| id)
        .collect()
}

/// The versioned-ID rule: a `-`, a digit, at most 256 bytes, no whitespace, no `-latest`.
fn versioned(model: &str) -> bool {
    model.len() <= 256
        && model.contains('-')
        && model.bytes().any(|b| b.is_ascii_digit())
        && !model.ends_with("-latest")
        && !model.chars().any(char::is_whitespace)
}

fn known_ids(kind: Kind) -> impl Iterator<Item = &'static str> {
    kind.documented_aliases().iter().map(|(_, exact)| *exact)
}

/// A vendor-prefixed exact ID, whose family-and-version form leaves the vendor out:
/// `claude-fable-5-1` is `Fable 5.1`.
fn vendor_prefixed(id: &str) -> bool {
    id.starts_with("claude-")
}

/// Lower case, with `.`, `_` and whitespace read as `-`.
fn normalized(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|c| match c {
            '.' | '_' => '-',
            c if c.is_whitespace() => '-',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// A spelling's name without its version: its separated parts that hold no digit.
fn family(value: &str) -> String {
    let normalized = normalized(value);
    let parts: Vec<&str> = normalized
        .split('-')
        .filter(|part| !part.is_empty() && !part.bytes().any(|b| b.is_ascii_digit()))
        .collect();
    if parts.len() < 2 {
        return String::new();
    }
    parts.join("-")
}

/// The Levenshtein distance of two short ASCII-or-not strings, by characters.
fn distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, r) in right.iter().enumerate() {
            let substitution = previous + usize::from(l != *r);
            previous = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[right.len()]
}

fn shown_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| hint::shown(value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn byte_length(bytes: usize) -> String {
    match bytes {
        1 => "1 byte".into(),
        n => format!("{n} bytes"),
    }
}

#[cfg(test)]
mod tests;
