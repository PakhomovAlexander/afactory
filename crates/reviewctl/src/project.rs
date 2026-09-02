//! Typed `.af/af.toml` policy shared by onboarding, review, and Task bootstrap.

use std::collections::BTreeMap;

use review_config::lock::Lockfile;
use semver::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    version: u32,
    project: Project,
    defaults: Defaults,
    #[serde(default)]
    worker: BTreeMap<String, Worker>,
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
    #[serde(default)]
    task_pipeline: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Worker {
    package: String,
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
        if let Some(task_pipeline) = &self.defaults.task_pipeline {
            validate_safe_name(task_pipeline, "default Task pipeline")?;
        }
        for (name, worker) in &self.worker {
            validate_safe_name(name, "Worker name")?;
            validate_safe_name(&worker.package, "Worker package")?;
        }
        enforce_min_af(&self.project.min_af)
    }

    pub fn review_pipeline(&self) -> &str {
        &self.defaults.pipeline
    }

    pub fn task_pipeline(&self) -> Result<&str, String> {
        self.defaults
            .task_pipeline
            .as_deref()
            .ok_or_else(|| ".af/af.toml must declare defaults.task_pipeline".into())
    }
}

/// Conservative pre-dispatch requirement for one Attempt per static Worker plus the minimum
/// durable charge for each model Provider smoke. Actual smoke spend replaces the floor at run
/// time; onboarding uses it before any Provider has been selected.
pub fn static_run_requirement(
    attempt_tokens: u64,
    static_workers: usize,
    model_workers_without_actual_spend: usize,
) -> Result<u64, String> {
    let attempts = attempt_tokens
        .checked_mul(static_workers as u64)
        .ok_or("static Worker budget arithmetic overflow")?;
    attempts
        .checked_add(model_workers_without_actual_spend as u64)
        .ok_or_else(|| "static Worker Provider budget arithmetic overflow".into())
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
    let Some(pinned) = lock.af_version.as_deref() else {
        return Ok(None);
    };
    let pinned = Version::parse(pinned).map_err(|error| {
        format!("authority lock `{lock_path}` records an invalid af_version `{pinned}`: {error}")
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

#[cfg(test)]
mod tests {
    use super::{ProjectFile, static_run_requirement};

    fn project(extra: &str, min_af: &str) -> String {
        format!(
            "version = 1\n[project]\nname = \"demo\"\nmin_af = \"{min_af}\"\n\
             [defaults]\npipeline = \"review\"\ntask_pipeline = \"implement\"\n\
             [worker.correctness]\npackage = \"correctness\"\n{extra}"
        )
    }

    #[test]
    fn accepts_short_compatible_minimum_version() {
        let parsed = ProjectFile::parse(&project("", "0.6")).unwrap();
        assert_eq!(parsed.review_pipeline(), "review");
        assert_eq!(parsed.task_pipeline().unwrap(), "implement");
    }

    #[test]
    fn rejects_unknown_policy_that_would_be_ignored() {
        let error = ProjectFile::parse(&project(
            "[env.default]\nisolation = \"host\"\nnetwork = \"ambient\"\n",
            "0.6",
        ))
        .unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn rejects_a_newer_required_binary() {
        let error = ProjectFile::parse(&project("", "999.0")).unwrap_err();
        assert!(error.contains("requires af >= 999.0.0"), "{error}");
    }

    #[test]
    fn static_requirement_includes_one_token_per_model_smoke() {
        assert_eq!(static_run_requirement(300_000, 2, 2).unwrap(), 600_002);
    }
}
