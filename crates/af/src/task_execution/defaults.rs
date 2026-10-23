//! `[task.defaults]` of `.af/af.toml` (ADR-0198): the `limits`, `pipeline` and `fallback` a
//! Task file inherits when it does not state them.
//!
//! The table is read from the Authority Snapshot, as every other piece of Task policy is, never
//! from the working tree; what it filled in is what the revision records, so the plan identity
//! is the effective one. A Task file states only what differs: a field it names wins whole.

use super::*;
use review_core::task::pipeline::PipelineDefinitionV1;

/// The `[task]` table of `.af/af.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskTable {
    #[serde(default)]
    defaults: Option<TaskDefaults>,
}

/// `[task.defaults]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskDefaults {
    #[serde(default)]
    limits: Option<FileLimits>,
    #[serde(default)]
    pipeline: Option<String>,
    /// What selection does when the default Pipeline cannot be used; only with `pipeline`.
    #[serde(default)]
    fallback: Option<PipelineFallbackV1>,
}

impl TaskTable {
    /// The checks a full-file parse of `.af/af.toml` runs on the table.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let Some(defaults) = &self.defaults else {
            return Ok(());
        };
        if defaults.fallback.is_some() && defaults.pipeline.is_none() {
            return Err(
                "`.af/af.toml` [task.defaults]: fallback says what happens when the default \
                 pipeline cannot be used, so it needs `pipeline` beside it"
                    .into(),
            );
        }
        if let Some(name) = &defaults.pipeline
            && !is_package_name(name)
        {
            return Err(format!(
                "`.af/af.toml` [task.defaults]: pipeline {name:?} is not a Pipeline name such as \
                 `kernel/implementation-reviewed`"
            ));
        }
        if let Some(limits) = &defaults.limits
            && limits.wall_ms == 0
        {
            return Err("`.af/af.toml` [task.defaults.limits]: wall_ms must be positive".into());
        }
        Ok(())
    }
}

/// The project's Task defaults, read from `.af/af.toml` of the Authority Snapshot; `None` when
/// the project keeps no file or no `[task.defaults]` table.
pub(super) fn captured(cas: &Cas, manifest: &Manifest) -> Result<Option<TaskDefaults>, String> {
    if manifest.get(".af/af.toml").is_none() {
        return Ok(None);
    }
    let bytes = captured_file(cas, manifest, ".af/af.toml")?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!(".af/af.toml: {e}"))?;
    #[derive(Deserialize)]
    struct TaskOnly {
        #[serde(default)]
        task: Option<TaskTable>,
    }
    let read: TaskOnly = toml::from_str(text).map_err(|error| {
        review_config::excerpt::bounded(&format!("authority project `.af/af.toml` [task]: {error}"))
    })?;
    let Some(table) = read.task else {
        return Ok(None);
    };
    table.validate()?;
    Ok(table.defaults)
}

/// Fill what `file` leaves to the project, and say what was filled: `limits`, and `pipeline`
/// when the default Pipeline accepts the Task's kind. A Task file without `limits` and a project
/// without default limits is refused with both places to state them.
pub(super) fn apply(
    file: &mut TaskFile,
    defaults: Option<&TaskDefaults>,
    pipelines: &BTreeMap<String, PipelineDefinitionV1>,
) -> Result<Vec<String>, String> {
    let mut filled = Vec::new();
    if file.limits.is_none() {
        let limits = defaults.and_then(|defaults| defaults.limits.clone());
        let limits = limits.ok_or_else(|| {
            format!(
                "Task file `{}` names no `limits`, and `.af/af.toml` declares no \
                 `[task.defaults.limits]`; add `limits` to the Task file, or the defaults to \
                 `.af/af.toml` (`af help task-file`)",
                file.task_id
            )
        })?;
        filled.push(format!(
            "limits (tokens {}, max_attempts {}, wall_ms {})",
            limits.tokens, limits.max_attempts, limits.wall_ms
        ));
        file.limits = Some(limits);
    }
    if file.pipeline.is_none()
        && let Some(name) = defaults.and_then(|defaults| defaults.pipeline.as_ref())
    {
        let fallback = defaults
            .and_then(|defaults| defaults.fallback)
            .unwrap_or(PipelineFallbackV1::Refuse);
        // A default for another kind of Task is not this Task's: selection decides as if the
        // project named none.
        if pipelines
            .get(name)
            .is_some_and(|pipeline| pipeline.accepts.kinds.contains(&file.kind))
        {
            filled.push(format!(
                "pipeline {name} (fallback {})",
                fallback_word(fallback)
            ));
            file.pipeline = Some(PipelineChoiceV1 {
                name: name.clone(),
                fallback,
            });
        }
    }
    Ok(filled)
}

fn fallback_word(fallback: PipelineFallbackV1) -> &'static str {
    match fallback {
        PipelineFallbackV1::Refuse => "refuse",
        PipelineFallbackV1::Select => "select",
        PipelineFallbackV1::Generate => "generate",
    }
}

/// The stderr line `af task plan` prints when the project's defaults filled something in.
pub(super) fn line(filled: &[String]) -> Option<String> {
    (!filled.is_empty()).then(|| {
        format!(
            "defaults: {} from .af/af.toml [task.defaults]",
            filled.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(json: serde_json::Value) -> TaskFile {
        serde_json::from_value(json).unwrap()
    }

    fn defaults(text: &str) -> TaskDefaults {
        let table: TaskTable = toml::from_str(text).unwrap();
        table.validate().unwrap();
        table.defaults.unwrap()
    }

    #[test]
    fn a_short_task_file_takes_the_project_limits_and_says_so() {
        let mut short = file(json!({"task_id":"t","kind":"implement","goal":"g"}));
        assert_eq!(short.schema, "af.task-file/1");
        let project = defaults(
            "[defaults]\npipeline = \"kernel/x\"\n[defaults.limits]\ntokens = 10\n\
             max_attempts = 2\nwall_ms = 1000\n[defaults.limits.verification]\ntokens = 1\n\
             attempts = 1\nwall_ms = 1\n",
        );
        let filled = apply(&mut short, Some(&project), &BTreeMap::new()).unwrap();
        assert_eq!(filled, ["limits (tokens 10, max_attempts 2, wall_ms 1000)"]);
        assert_eq!(short.limits.unwrap().tokens, 10);
        assert!(
            short.pipeline.is_none(),
            "a Pipeline that is not captured is never the default"
        );
        assert_eq!(
            line(&filled).unwrap(),
            "defaults: limits (tokens 10, max_attempts 2, wall_ms 1000) from .af/af.toml \
             [task.defaults]"
        );
    }

    #[test]
    fn a_stated_field_wins_and_a_missing_one_is_refused_with_both_places() {
        let mut stated = file(json!({"task_id":"t","kind":"implement","goal":"g",
            "limits":{"tokens":5,"max_attempts":1,"wall_ms":1,
                "verification":{"tokens":0,"attempts":0,"wall_ms":0}}}));
        assert!(
            apply(&mut stated, None, &BTreeMap::new())
                .unwrap()
                .is_empty()
        );
        let mut bare = file(json!({"task_id":"t","kind":"implement","goal":"g"}));
        let refused = apply(&mut bare, None, &BTreeMap::new()).unwrap_err();
        assert!(refused.contains("[task.defaults.limits]"), "{refused}");
    }

    #[test]
    fn a_fallback_without_a_pipeline_is_refused() {
        let table: TaskTable = toml::from_str("[defaults]\nfallback = \"select\"\n").unwrap();
        assert!(table.validate().unwrap_err().contains("needs `pipeline`"));
        assert!(toml::from_str::<TaskTable>("[defaults]\nstrategy = \"x\"\n").is_err());
    }
}
