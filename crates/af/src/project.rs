//! Typed `.af/af.toml` policy shared by onboarding, review, and Task bootstrap.

use review_config::Loaded;
use review_config::lock::Lockfile;
use semver::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    version: u32,
    project: Project,
    defaults: Defaults,
    /// How changed paths select a pipeline, and what happens when no route or too many match.
    #[serde(default)]
    routing: Option<Routing>,
    /// Declared in order; a route matches when every changed path (both sides of a rename)
    /// matches one of its patterns.
    #[serde(default)]
    routes: Vec<Route>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Routing {
    #[serde(default)]
    unmatched: Unmatched,
    #[serde(default)]
    ambiguous: Ambiguous,
    /// The pipeline to select instead when a Worker's first-Attempt input alone exhausts its
    /// Attempt cap — a bounded strategy chosen by trusted policy, never a truncation.
    #[serde(default)]
    oversized: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Unmatched {
    #[default]
    Default,
    Refuse,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Ambiguous {
    #[default]
    Refuse,
    First,
}

/// One route: canonical repository-path patterns and the pipeline they select. Patterns are
/// anchored at the repository root and match by segment: `*` within one segment, `?` one
/// character, `**` any number of segments (`docs/**`, `**/*.md`, `src/*/lib.rs`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub name: String,
    pub paths: Vec<String>,
    pub pipeline: String,
}

/// How a pipeline was chosen for one invocation. Captured by the pipeline path the Campaign
/// Manifest pins; printed by `plan` and at Campaign open.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RouteDecision {
    /// `explicit` (`--pipeline`), `default`, `route`, or `oversized`.
    pub policy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Every route whose patterns covered the changed paths, in declaration order.
    pub matched: Vec<String>,
    pub changed_paths: usize,
    pub pipeline_path: String,
    /// The pipeline the oversized policy replaced, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<String>,
}

impl RouteDecision {
    pub(crate) fn explicit(pipeline_path: &str) -> Self {
        RouteDecision {
            policy: "explicit",
            name: None,
            matched: Vec::new(),
            changed_paths: 0,
            pipeline_path: pipeline_path.to_string(),
            replaced: None,
        }
    }
}

pub(crate) fn pipeline_path_for(name: &str) -> String {
    format!(".af/pipelines/{name}.toml")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    name: String,
    min_af: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    pipeline: String,
}

impl ProjectFile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let project: Self = toml::from_str(text)
            .map_err(|error| format!("authority project `.af/af.toml`: {error}"))?;
        project.validate()?;
        Ok(project)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("authority project `.af/af.toml` must declare `version = 1`".into());
        }
        if self.project.name.trim().is_empty() {
            return Err("authority project name must not be empty".into());
        }
        validate_safe_name(&self.defaults.pipeline, "default review pipeline")?;
        let mut names = std::collections::BTreeSet::new();
        for route in &self.routes {
            validate_safe_name(&route.name, "route name")?;
            validate_safe_name(&route.pipeline, "route pipeline")?;
            if !names.insert(route.name.as_str()) {
                return Err(format!(
                    "authority project route `{}` is declared twice",
                    route.name
                ));
            }
            if route.paths.is_empty() {
                return Err(format!(
                    "authority project route `{}` declares no path patterns",
                    route.name
                ));
            }
            for pattern in &route.paths {
                validate_pattern(pattern, &route.name)?;
            }
        }
        if let Some(oversized) = self
            .routing
            .as_ref()
            .and_then(|routing| routing.oversized.as_deref())
        {
            validate_safe_name(oversized, "routing.oversized pipeline")?;
        }
        enforce_min_af(&self.project.min_af)
    }

    /// Whether changed paths can change the pipeline at all.
    pub fn routes_configured(&self) -> bool {
        !self.routes.is_empty()
            || self
                .routing
                .as_ref()
                .is_some_and(|routing| routing.oversized.is_some())
    }

    pub fn oversized_pipeline(&self) -> Option<&str> {
        self.routing
            .as_ref()
            .and_then(|routing| routing.oversized.as_deref())
    }

    /// Every pipeline this project may select: the default, every route target, and the
    /// oversized strategy. All of them must exist and be pinned.
    pub fn pipeline_candidates(&self) -> std::collections::BTreeSet<String> {
        let mut candidates = std::collections::BTreeSet::from([self.defaults.pipeline.clone()]);
        candidates.extend(self.routes.iter().map(|route| route.pipeline.clone()));
        candidates.extend(self.oversized_pipeline().map(str::to_string));
        candidates
    }

    /// Select the pipeline for a set of canonical changed paths — deterministic, token-free.
    /// An empty set (a whole-tree Subject, or no `--base`) matches no route.
    pub(crate) fn select_route(&self, changed_paths: &[String]) -> Result<RouteDecision, String> {
        let routing = self.routing.as_ref();
        let matched: Vec<&Route> = if changed_paths.is_empty() {
            Vec::new()
        } else {
            self.routes
                .iter()
                .filter(|route| {
                    changed_paths.iter().all(|path| {
                        route
                            .paths
                            .iter()
                            .any(|pattern| glob_matches(pattern, path))
                    })
                })
                .collect()
        };
        let names: Vec<String> = matched.iter().map(|route| route.name.clone()).collect();
        let decision = |policy: &'static str, route: Option<&Route>| RouteDecision {
            policy,
            name: route.map(|route| route.name.clone()),
            matched: names.clone(),
            changed_paths: changed_paths.len(),
            pipeline_path: pipeline_path_for(
                route.map_or(self.defaults.pipeline.as_str(), |route| {
                    route.pipeline.as_str()
                }),
            ),
            replaced: None,
        };
        match matched.as_slice() {
            [] => match routing.map_or(Unmatched::Default, |routing| routing.unmatched) {
                Unmatched::Default => Ok(decision("default", None)),
                Unmatched::Refuse => Err(format!(
                    "no route covers the {} changed path(s) and routing.unmatched = \"refuse\" ({}); declare a route or select --pipeline explicitly",
                    changed_paths.len(),
                    preview(changed_paths)
                )),
            },
            [route] => Ok(decision("route", Some(route))),
            [first, ..] => match routing.map_or(Ambiguous::Refuse, |routing| routing.ambiguous) {
                Ambiguous::First => Ok(decision("route", Some(first))),
                Ambiguous::Refuse => Err(format!(
                    "routes {} all cover the changed paths and routing.ambiguous = \"refuse\"; make the patterns disjoint, set routing.ambiguous = \"first\", or select --pipeline explicitly",
                    names.join(", ")
                )),
            },
        }
    }

    pub fn review_pipeline(&self) -> &str {
        &self.defaults.pipeline
    }
}

fn preview(paths: &[String]) -> String {
    let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
    if paths.len() > 3 {
        format!("{}, …", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

fn validate_pattern(pattern: &str, route: &str) -> Result<(), String> {
    if pattern.is_empty()
        || pattern.starts_with('/')
        || pattern.ends_with('/')
        || pattern
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(format!(
            "authority project route `{route}` pattern `{pattern}` must be a root-anchored repository path pattern without empty, `.`, or `..` segments"
        ));
    }
    Ok(())
}

/// Segment-wise glob: `**` spans any number of segments, `*` any run within a segment, `?` one
/// character. Anchored at the repository root; `path` is the canonical (encoded) form.
pub(crate) fn glob_matches(pattern: &str, path: &str) -> bool {
    fn segment(pattern: &[u8], text: &[u8]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(b'*'), _) => {
                segment(&pattern[1..], text) || (!text.is_empty() && segment(pattern, &text[1..]))
            }
            (Some(b'?'), Some(_)) => segment(&pattern[1..], &text[1..]),
            (Some(&expected), Some(&actual)) if expected == actual => {
                segment(&pattern[1..], &text[1..])
            }
            _ => false,
        }
    }
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|skip| segments(rest, &path[skip..])),
            Some((head, rest)) => match path.split_first() {
                Some((first, remaining)) => {
                    segment(head.as_bytes(), first.as_bytes()) && segments(rest, remaining)
                }
                None => false,
            },
        }
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

fn validate_safe_name(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
        })
    {
        return Err(format!("authority project {label} must be one safe name"));
    }
    Ok(())
}

fn enforce_min_af(required: &str) -> Result<(), String> {
    let required = parse_version(required)
        .map_err(|error| format!("authority project min_af is invalid: {error}"))?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("running af version is invalid: {error}"))?;
    if current < required {
        return Err(format!(
            "authority requires af >= {required}; running {current}"
        ));
    }
    Ok(())
}

fn parse_version(value: &str) -> Result<Version, semver::Error> {
    let components = value.split('.').count();
    match components {
        1 => Version::parse(&format!("{value}.0.0")),
        2 => Version::parse(&format!("{value}.0")),
        _ => Version::parse(value),
    }
}

/// Compares the running `af` with the release that wrote an authority lock. A lock written by a
/// newer release is refused: this binary cannot know what that release meant by its pins. A lock
/// written by an older release proceeds and returns a note naming `--refresh-lock`. A lock with
/// no pin, or one written by this exact release, is silent.
pub(crate) fn check_lock_af_version(
    lock: &Lockfile,
    lock_path: &str,
) -> Result<Option<String>, String> {
    let Some(pinned) = lock.af_version() else {
        return Ok(None);
    };
    let pinned = Version::parse(pinned).map_err(|error| {
        format!("authority lock `{lock_path}` records an invalid af version `{pinned}`: {error}")
    })?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("running af version is invalid: {error}"))?;
    if pinned > current {
        return Err(format!(
            "authority lock `{lock_path}` was pinned by af {pinned}; this is af {current}, an older release that cannot trust those pins. Upgrade af, or re-pin with `af onboard --refresh-lock` from the release you run"
        ));
    }
    if pinned < current {
        return Ok(Some(format!(
            "authority lock `{lock_path}` was pinned by af {pinned}; this is af {current} — `af onboard --refresh-lock` re-pins it"
        )));
    }
    Ok(None)
}

/// One static Worker's first-Attempt reservation, as planning and admission count it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct StaticReservation {
    pub node: String,
    pub tokens: u64,
    /// `node` when the Worker declared its own cap, `pipeline` for the shared attempt cap.
    pub source: &'static str,
}

/// Every static Worker's first-Attempt reservation, in node order. Empty when uncapped.
pub(crate) fn static_reservations(loaded: &Loaded) -> Vec<StaticReservation> {
    let Some(budgets) = loaded.budgets() else {
        return Vec::new();
    };
    loaded
        .reviewers()
        .keys()
        .map(|node| match loaded.node_attempt_caps().get(node) {
            Some(cap) => StaticReservation {
                node: node.clone(),
                tokens: *cap,
                source: "node",
            },
            None => StaticReservation {
                node: node.clone(),
                tokens: budgets.attempt,
                source: "pipeline",
            },
        })
        .collect()
}

/// The most the static Workers can hold reserved at once: every first Attempt together. This is
/// what the run cap must admit before any Worker dispatches. `None` when uncapped.
pub(crate) fn max_simultaneous_reservation(loaded: &Loaded) -> Result<Option<u64>, String> {
    if loaded.budgets().is_none() {
        return Ok(None);
    }
    static_reservations(loaded)
        .iter()
        .try_fold(0_u64, |sum, reservation| {
            sum.checked_add(reservation.tokens)
                .ok_or_else(|| "static Worker reservation arithmetic overflow".to_string())
        })
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::{ProjectFile, glob_matches};

    fn project(extra: &str, min_af: &str) -> String {
        format!(
            "version = 1\n[project]\nname = \"demo\"\nmin_af = \"{min_af}\"\n\
             [defaults]\npipeline = \"review\"\n{extra}"
        )
    }

    #[test]
    fn accepts_short_compatible_minimum_version() {
        let parsed = ProjectFile::parse(&project("", "0.6")).unwrap();
        assert_eq!(parsed.review_pipeline(), "review");
    }

    #[test]
    fn rejects_unknown_policy_that_would_be_ignored() {
        for extra in [
            "[env.default]\nisolation = \"host\"\nnetwork = \"ambient\"\n",
            "[worker.correctness]\npackage = \"correctness\"\n",
            "task_pipeline = \"implement\"\n",
        ] {
            let error = ProjectFile::parse(&project(extra, "0.6")).unwrap_err();
            assert!(error.contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn rejects_a_newer_required_binary() {
        let error = ProjectFile::parse(&project("", "999.0")).unwrap_err();
        assert!(error.contains("requires af >= 999.0.0"), "{error}");
    }

    fn routed(routing: &str, routes: &str) -> ProjectFile {
        ProjectFile::parse(&format!(
            "version = 1\n[project]\nname = \"demo\"\nmin_af = \"0.6\"\n\
             [defaults]\npipeline = \"review\"\n{routing}{routes}"
        ))
        .unwrap()
    }

    const DOCS: &str =
        "[[routes]]\nname = \"docs\"\npaths = [\"docs/**\", \"*.md\"]\npipeline = \"docs\"\n";
    const MARKDOWN: &str =
        "[[routes]]\nname = \"markdown\"\npaths = [\"**/*.md\"]\npipeline = \"docs\"\n";

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn globs_are_root_anchored_and_segment_wise() {
        assert!(glob_matches("docs/**", "docs/a/b.md"));
        assert!(glob_matches("docs/**", "docs"));
        assert!(!glob_matches("docs/**", "src/docs/x"));
        assert!(glob_matches("*.md", "README.md"));
        assert!(!glob_matches("*.md", "docs/README.md"));
        assert!(glob_matches("**/*.md", "docs/deep/README.md"));
        assert!(glob_matches("**/*.md", "README.md"));
        assert!(glob_matches("src/*/lib.rs", "src/core/lib.rs"));
        assert!(!glob_matches("src/*/lib.rs", "src/core/inner/lib.rs"));
        assert!(glob_matches("a?c", "abc"));
        // Canonical encoding: a non-UTF-8 byte is `%FF` in the path both sides see.
        assert!(glob_matches("**", "dir/a%FFb"));
        assert!(glob_matches("dir/a*b", "dir/a%FFb"));
    }

    #[test]
    fn a_route_must_cover_every_changed_path_including_both_rename_sides() {
        let project = routed("", DOCS);
        let docs = project
            .select_route(&paths(&["docs/old.md", "docs/new.md", "README.md"]))
            .unwrap();
        assert_eq!(docs.policy, "route");
        assert_eq!(docs.name.as_deref(), Some("docs"));
        assert_eq!(docs.pipeline_path, ".af/pipelines/docs.toml");
        assert_eq!(docs.changed_paths, 3);

        let mixed = project
            .select_route(&paths(&["docs/a.md", "src/lib.rs"]))
            .unwrap();
        assert_eq!(mixed.policy, "default");
        assert_eq!(mixed.pipeline_path, ".af/pipelines/review.toml");
        assert!(mixed.matched.is_empty());

        let whole_tree = project.select_route(&[]).unwrap();
        assert_eq!(whole_tree.policy, "default");
        assert_eq!(whole_tree.changed_paths, 0);
    }

    #[test]
    fn unmatched_and_ambiguous_follow_explicit_policy_never_silent_fallback() {
        let refuse = routed("[routing]\nunmatched = \"refuse\"\n", DOCS);
        let error = refuse.select_route(&paths(&["src/lib.rs"])).unwrap_err();
        assert!(error.contains("no route covers"), "{error}");

        let ambiguous = routed("", &format!("{DOCS}{MARKDOWN}"));
        let error = ambiguous.select_route(&paths(&["docs/a.md"])).unwrap_err();
        assert!(error.contains("routes docs, markdown"), "{error}");

        let first = routed(
            "[routing]\nambiguous = \"first\"\n",
            &format!("{DOCS}{MARKDOWN}"),
        );
        let decision = first.select_route(&paths(&["docs/a.md"])).unwrap();
        assert_eq!(decision.name.as_deref(), Some("docs"));
        assert_eq!(decision.matched, vec!["docs", "markdown"]);
    }

    #[test]
    fn route_targets_and_the_oversized_pipeline_are_candidates_and_are_validated() {
        let project = routed("[routing]\noversized = \"scatter\"\n", DOCS);
        assert_eq!(
            project
                .pipeline_candidates()
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["docs", "review", "scatter"]
        );
        assert!(project.routes_configured());
        assert!(!routed("", "").routes_configured());
        for (routing, routes, expected) in [
            (
                "",
                "[[routes]]\nname = \"x\"\npaths = []\npipeline = \"docs\"\n",
                "no path patterns",
            ),
            (
                "",
                "[[routes]]\nname = \"x\"\npaths = [\"/abs\"]\npipeline = \"docs\"\n",
                "root-anchored",
            ),
            ("", &format!("{DOCS}{DOCS}"), "declared twice"),
            ("[routing]\noversized = \"bad name\"\n", "", "one safe name"),
        ] {
            let error = ProjectFile::parse(&format!(
                "version = 1\n[project]\nname = \"demo\"\nmin_af = \"0.6\"\n\
                 [defaults]\npipeline = \"review\"\n{routing}{routes}"
            ))
            .unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }
}
