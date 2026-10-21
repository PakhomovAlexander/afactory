//! The command's sections of a run's configuration record (`af/TelemetryConfig@2`, design
//! §7.4): metadata only, computed when the Task runtime writes them as the run's `af.run` opens,
//! at `spans` and above.
//!
//! An environment value is never recorded: a configuration key whose origin is the environment
//! layer has the value `<env>`, its origin the variable's name, and of the `AF_*` and `OTEL_*`
//! variables present only the names are listed. A section that cannot be read is one row saying
//! `unknown` with the reason, never a silent omission.

use std::path::Path;

use review_pipeline::telemetry::{MachineConfig, row};
use review_telemetry::record::ConfigRow;

use crate::config;

/// The value an environment-origin configuration value is recorded as.
pub(crate) const ENV_VALUE: &str = "<env>";

/// The command's sections, read now from the machine and the working directory.
pub(crate) fn machine_config() -> MachineConfig {
    let loaded = config::load(None);
    let toplevel = loaded
        .as_ref()
        .ok()
        .and_then(|config| config.toplevel.clone());
    let (policy_files, texts) = policy_files(toplevel.as_deref());
    let (providers, harnesses) = providers();
    MachineConfig {
        version: version(),
        configuration: match &loaded {
            Ok(config) => configuration(config),
            Err(reason) => vec![row(
                "configuration",
                "configuration",
                "unknown",
                Some(reason.clone()),
            )],
        },
        policy_files,
        providers,
        harnesses,
        host: host(),
        variables: variables(std::env::vars_os().map(|(name, _)| name)),
        texts,
    }
}

/// `af --version --json`, one row per member.
fn version() -> Vec<ConfigRow> {
    let document = crate::selfmgmt::version_document();
    let Some(members) = document.as_object() else {
        return vec![row("version", "version", "unknown", None)];
    };
    members
        .iter()
        .map(|(key, value)| {
            let value = match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            row("version", &format!("version.{key}"), &value, None)
        })
        .collect()
}

/// The effective configuration, one row per key in key order: the layer it came from, its file
/// (home-relative) and line, and its value; `<env>` for a value whose origin is the environment.
pub(crate) fn configuration(config: &config::Config) -> Vec<ConfigRow> {
    configuration_under(config, config::home().ok().as_deref())
}

/// A file origin as the record keeps it: below `home` as `~/…`, never the operator's absolute
/// home path; elsewhere as it is.
fn origin_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// [`configuration`] with the operator's home `home`.
fn configuration_under(config: &config::Config, home: Option<&Path>) -> Vec<ConfigRow> {
    let mut leaves = Vec::new();
    config::flatten(&config.effective, "", &mut leaves);
    leaves
        .into_iter()
        .map(|(key, value)| match config.origins.get(&key) {
            Some(origin) if origin.layer == "environment" => row(
                origin.layer,
                &key,
                ENV_VALUE,
                origin.path.as_ref().map(|name| name.display().to_string()),
            ),
            Some(origin) => row(
                origin.layer,
                &key,
                &value,
                origin.path.as_ref().map(|path| match origin.line {
                    Some(line) => format!("{}:{line}", origin_path(path, home)),
                    None => origin_path(path, home),
                }),
            ),
            None => row("unknown", &key, &value, None),
        })
        .collect()
}

/// The digests of `.af/af.toml` and `.af/af.lock` in the repository the command runs in, and
/// `.af/af.toml` as a text the record names.
fn policy_files(toplevel: Option<&Path>) -> (Vec<ConfigRow>, Vec<(&'static str, String, u64)>) {
    let mut rows = Vec::new();
    let mut texts = Vec::new();
    for (key, name) in [
        ("af.af_toml.digest", "af.toml"),
        ("af.af_lock.digest", "af.lock"),
    ] {
        let Some(toplevel) = toplevel else {
            rows.push(row("policy", key, "absent", None));
            continue;
        };
        match std::fs::read(toplevel.join(".af").join(name)) {
            Ok(bytes) => {
                let digest = review_telemetry::trace::sha256_digest(&bytes);
                if name == "af.toml" {
                    texts.push(("af_toml", digest.clone(), bytes.len() as u64));
                }
                rows.push(row("policy", key, &digest, None));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                rows.push(row("policy", key, "absent", None));
            }
            Err(error) => rows.push(row("policy", key, "unknown", Some(error.to_string()))),
        }
    }
    (rows, texts)
}

/// The Provider registry entries, then the harness versions the preflight probed and the
/// Claude Code managed-settings digests (empty until the destination proof reads them).
fn providers() -> (Vec<ConfigRow>, Vec<ConfigRow>) {
    let (entries, versions) = crate::providers::telemetry_registry();
    let providers = match entries {
        Ok(entries) => entries
            .into_iter()
            .map(|(id, value)| row("provider", &id, &value, None))
            .collect(),
        Err(reason) => vec![row("provider", "registry", "unknown", Some(reason))],
    };
    let mut harnesses: Vec<ConfigRow> = versions
        .into_iter()
        .map(|(program, version)| row("harness", &program, &version, None))
        .collect();
    harnesses.push(row("harness", "claude_code.managed_settings", "", None));
    (providers, harnesses)
}

/// The host: its platform and processor count.
fn host() -> Vec<ConfigRow> {
    vec![
        row("host", "host.os", std::env::consts::OS, None),
        row("host", "host.arch", std::env::consts::ARCH, None),
        row(
            "host",
            "host.cpus",
            &std::thread::available_parallelism()
                .map(|count| count.get().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            None,
        ),
    ]
}

/// The names, never the values, of the `AF_*` and `OTEL_*` variables present, sorted.
pub(crate) fn variables(names: impl Iterator<Item = std::ffi::OsString>) -> Vec<ConfigRow> {
    let mut names: Vec<String> = names
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("AF_") || name.starts_with("OTEL_"))
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| row("variable", &name, "", None))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design §7.4: a configuration value whose origin is the environment is `<env>` with the
    /// variable's name as its origin; a file value keeps its value, file and line; of the
    /// variables only the names are kept.
    #[test]
    fn telemetry_config_record_withholds_environment_values() {
        let directory = tempfile::tempdir().unwrap();
        let user = directory.path().join("user");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("config.toml"),
            "[telemetry]\nflush_bytes = \"128KiB\"\n",
        )
        .unwrap();
        let loaded = config::load_rooted(
            Some(directory.path()),
            true,
            &config::MachineRoots {
                system: directory.path().join("system"),
                user: user.clone(),
                environment: vec![
                    ("AF_TELEMETRY__LEVEL".into(), "violet-content".into()),
                    ("AF_STORAGE__MAX_BYTES".into(), "\"7GiB\"".into()),
                ],
            },
        )
        .unwrap();
        let rows = configuration_under(&loaded, None);
        let find = |key: &str| {
            rows.iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("no row {key}: {rows:?}"))
        };
        let level = find("telemetry.level");
        assert_eq!(level.value, ENV_VALUE);
        assert_eq!(level.layer, "environment");
        assert_eq!(level.origin.as_deref(), Some("AF_TELEMETRY__LEVEL"));
        assert_eq!(find("storage.max_bytes").value, ENV_VALUE);
        let flush = find("telemetry.flush_bytes");
        assert_eq!(flush.value, "\"128KiB\"");
        assert_eq!(flush.layer, "user");
        assert_eq!(
            flush.origin.as_deref(),
            Some(format!("{}:2", user.join("config.toml").display()).as_str())
        );
        assert!(
            rows.iter()
                .all(|row| !row.value.contains("violet") && !row.value.contains("7GiB")),
            "{rows:?}"
        );
        let names = variables(
            [
                "AF_TELEMETRY__LEVEL",
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "PATH",
                "HOME",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        );
        assert_eq!(
            names
                .iter()
                .map(|row| (row.key.as_str(), row.value.as_str()))
                .collect::<Vec<_>>(),
            [
                ("AF_TELEMETRY__LEVEL", ""),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "")
            ]
        );
    }

    /// Design §7.4 and §8 (P3): a configuration origin under the operator's home is written
    /// home-relative with its line, never as an absolute path naming the home; an origin
    /// elsewhere keeps its path.
    #[test]
    fn telemetry_config_origin_under_home_is_home_relative() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let user = home.join(".config/af");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("config.toml"),
            "[telemetry]\nflush_bytes = \"128KiB\"\n",
        )
        .unwrap();
        let system = directory.path().join("system");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(
            system.join("config.toml"),
            "[storage]\nmax_bytes = \"7GiB\"\n",
        )
        .unwrap();
        let loaded = config::load_rooted(
            Some(directory.path()),
            true,
            &config::MachineRoots {
                system: system.clone(),
                user,
                environment: Vec::new(),
            },
        )
        .unwrap();
        let rows = configuration_under(&loaded, Some(&home));
        let origin = |key: &str| {
            rows.iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("no row {key}: {rows:?}"))
                .origin
                .clone()
        };
        assert_eq!(
            origin("telemetry.flush_bytes").as_deref(),
            Some("~/.config/af/config.toml:2")
        );
        assert!(
            rows.iter()
                .filter_map(|row| row.origin.as_deref())
                .all(|origin| !origin.contains(&home.display().to_string())),
            "{rows:?}"
        );
        assert_eq!(
            origin("storage.max_bytes"),
            Some(format!("{}:2", system.join("config.toml").display()))
        );
    }
}
