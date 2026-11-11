//! Typed `.af/af.toml` policy shared by onboarding, review, and Task bootstrap.

use af_pipeline_config::layout::UndeclaredAfPathsPolicy;
use af_pipeline_config::lock::Lockfile;
use semver::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    version: u32,
    project: Project,
    /// What delivery does about paths the declared `.af/` layout does not name. Admitted here
    /// so the full-file parse validates the table; capture reads it through `parse_delivery`.
    #[serde(default)]
    #[allow(dead_code)]
    delivery: Delivery,
    /// What the Surveyor measures by (ADR-0159). Admitted here so the full-file parse refuses
    /// an unknown key; the Surveyor reads the merged ladder through `Policy::load`.
    #[serde(default)]
    #[allow(dead_code)]
    survey: Option<crate::project::survey::policy::SurveyTable>,
    /// The Project Brief variants (ADR-0160). Admitted here so the full-file parse refuses an
    /// unknown key; capture and `af project brief` read the merged ladder through
    /// `af_project::brief::load_policy`, which also checks every value.
    #[serde(default)]
    #[allow(dead_code)]
    brief: Option<std::collections::BTreeMap<String, crate::project::brief::BriefTable>>,
    /// The `[task.defaults]` a Task file inherits (ADR-0212). Admitted and checked here so the
    /// full-file parse refuses an unknown key; Task capture reads it from the Authority
    /// Snapshot.
    #[serde(default)]
    task: Option<crate::task_commands::defaults::TaskTable>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    name: String,
    min_af: String,
}

/// `[delivery]` — what an explicit local delivery does about the project's own authority tree.
/// Optional in every direction: a project that says nothing keeps the advisory default.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    #[serde(default)]
    undeclared_af_paths: UndeclaredAfPathsPolicy,
}

impl ProjectFile {
    /// Shared partial readers still refuse retired project versions and Campaign keys.
    pub(crate) fn validate_current_fields(text: &str) -> Result<(), String> {
        let value: toml::Value = toml::from_str(text)
            .map_err(|e| af_pipeline_config::excerpt::bounded(&e.to_string()))?;
        if value
            .get("version")
            .is_some_and(|v| v.as_integer() != Some(2))
            || ["defaults", "routing", "routes"]
                .iter()
                .any(|key| value.get(*key).is_some())
        {
            return Err("authority project `.af/af.toml`: use version = 2 and `[task.defaults]`; Campaign defaults and routing are retired".into());
        }
        Ok(())
    }

    /// The delivery policy the full-file parse admitted; the unit tests read it here, while
    /// capture reads the same table through `parse_delivery`.
    #[cfg(test)]
    pub fn undeclared_af_paths(&self) -> UndeclaredAfPathsPolicy {
        self.delivery.undeclared_af_paths
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let project: Self = toml::from_str(text).map_err(|error| {
            af_pipeline_config::excerpt::bounded(&format!(
                "authority project `.af/af.toml`: {error}"
            ))
        })?;
        project.validate()?;
        Ok(project)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 2 {
            return Err("authority project `.af/af.toml` must declare `version = 2`; Campaign defaults and routing are retired; use `[task.defaults]` and an explicit Review Task file".into());
        }
        if self.project.name.trim().is_empty() {
            return Err("authority project name must not be empty".into());
        }
        if let Some(task) = &self.task {
            task.validate()?;
        }
        enforce_min_af(&self.project.min_af)
    }

    /// Read `[delivery]` on its own, out of a captured `.af/af.toml`.
    ///
    /// A Task captures this policy at plan time from its Authority Snapshot, before anything has
    /// admitted the whole project file, and must not start requiring more of that file than it
    /// did: unknown *tables* are ignored here, because [`ProjectFile::parse`] is what admits
    /// them. An unknown key inside `[delivery]` is still an error, never a silent setting.
    pub fn parse_delivery(text: &str) -> Result<UndeclaredAfPathsPolicy, String> {
        Self::validate_current_fields(text)?;
        #[derive(Deserialize)]
        struct DeliveryOnly {
            #[serde(default)]
            delivery: Delivery,
        }
        let read: DeliveryOnly = toml::from_str(text).map_err(|error| {
            af_pipeline_config::excerpt::bounded(&format!(
                "authority project `.af/af.toml` [delivery]: {error}"
            ))
        })?;
        Ok(read.delivery.undeclared_af_paths)
    }
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
#[cfg(test)]
mod tests {
    use af_pipeline_config::layout::UndeclaredAfPathsPolicy;

    use super::ProjectFile;

    fn project(extra: &str, min_af: &str) -> String {
        format!(
            "version = 2\n[project]\nname = \"demo\"\nmin_af = \"{min_af}\"\n\
             {extra}"
        )
    }

    #[test]
    fn accepts_short_compatible_minimum_version() {
        ProjectFile::parse(&project("", "0.6")).unwrap();
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
    fn the_delivery_policy_is_optional_and_a_typo_in_it_is_an_error() {
        let silent = ProjectFile::parse(&project("", "0.6")).unwrap();
        assert_eq!(silent.undeclared_af_paths(), UndeclaredAfPathsPolicy::Warn);
        let table = "[delivery]\nundeclared_af_paths = \"refuse\"\n";
        let strict = ProjectFile::parse(&project(table, "0.6")).unwrap();
        let refuse = UndeclaredAfPathsPolicy::Refuse;
        assert_eq!(strict.undeclared_af_paths(), refuse);
        // The same value read on its own, the way a Task captures it: the rest of the file is
        // somebody else's business, but this table is still admitted exactly.
        assert_eq!(ProjectFile::parse_delivery(table).unwrap(), refuse);
        assert_eq!(ProjectFile::parse_delivery("").unwrap().as_str(), "warn");
        let alone = "[unrelated]\nkey = 1\n";
        assert_eq!(ProjectFile::parse_delivery(alone).unwrap().as_str(), "warn");
        for bad in [
            "[delivery]\nundeclared_af_path = \"refuse\"\n",
            "[delivery]\nundeclared_af_paths = \"strip\"\n",
        ] {
            assert!(ProjectFile::parse_delivery(bad).is_err(), "{bad}");
            assert!(ProjectFile::parse(&project(bad, "0.6")).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_a_newer_required_binary() {
        let error = ProjectFile::parse(&project("", "999.0")).unwrap_err();
        assert!(error.contains("requires af >= 999.0.0"), "{error}");
    }

    #[test]
    fn retired_project_versions_defaults_and_routes_are_refused() {
        let valid = project("", "0.6");
        for old in [
            valid.replace("version = 2", "version = 1"),
            format!("{valid}\n[defaults]\npipeline = \"review\"\n"),
            format!("{valid}\n[routing]\nambiguous = \"first\"\n"),
            format!(
                "{valid}\n[[routes]]\nname = \"docs\"\npaths = [\"docs/**\"]\npipeline = \"review\"\n"
            ),
        ] {
            assert!(ProjectFile::parse(&old).is_err(), "{old}");
        }
    }
    #[test]
    fn current_project_policy_schema_matches_generated_and_refused_authority() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../schemas/project-policy-v2.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for text in [
            project("", "0.6"),
            project(
                "[delivery]\nundeclared_af_paths = \"refuse\"\n[survey]\nidentity = \"email_hash\"\nmax_commits = 12\n[brief.orient]\nsections = [\"summary\"]\nmax_bytes = 1024\n[task.defaults.limits]\ntokens = 1000\nmax_attempts = 2\nwall_ms = 10000\n[task.defaults.limits.verification]\ntokens = 100\nattempts = 1\nwall_ms = 1000\n",
                "0.6",
            ),
            project(
                "[task.defaults]\npipeline = \"kernel/review\"\nfallback = \"refuse\"\n",
                "0.6",
            ),
        ] {
            ProjectFile::parse(&text).unwrap();
            let value: toml::Value = toml::from_str(&text).unwrap();
            let json = serde_json::to_value(value).unwrap();
            assert!(validator.is_valid(&json));
            for (key, value) in [
                ("version", serde_json::json!(1)),
                ("defaults", serde_json::json!({"pipeline":"review"})),
                ("routes", serde_json::json!([])),
                ("routing", serde_json::json!({})),
            ] {
                let mut old = json.clone();
                old[key] = value;
                assert!(!validator.is_valid(&old));
                let text = toml::to_string(&old).unwrap();
                assert!(ProjectFile::parse(&text).is_err());
                assert!(ProjectFile::parse_delivery(&text).is_err());
            }
        }
    }
}
