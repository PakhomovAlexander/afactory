//! Trusted-host validation of the private evaluator package.
use super::*;

pub(super) const FROZEN_EVALUATOR_SHA256: &str =
    "sha256:4750f9fb87924f33da88f4aff85a894baad1d42faf55adeb0c899a90d6801014";
const FIXTURE_SCHEMA: &str = "af.agent-bench-evaluator/1";
const TEXT_LIMIT: usize = 65_536;

#[derive(Debug, Args)]
pub(crate) struct EvaluatorCheck {
    /// An external benchmark Task with an exact captured source and subject package.
    pub(super) id: String,
    /// Private evaluator JSON file, outside the checkout and Task Store.
    #[arg(long, value_name = "FILE")]
    evaluator: PathBuf,
    /// SHA-256 digest of the exact evaluator file bytes, including whitespace.
    #[arg(long, value_name = "SHA256")]
    pub(super) evaluator_sha256: String,
    /// Project checkout used to locate the Task Store.
    #[arg(long, default_value = ".")]
    pub(super) repo: PathBuf,
    /// Use this explicit Store directory for the subject Task and any grading plan.
    #[arg(long)]
    pub(super) state: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvaluatorPackage {
    schema: String,
    status: String,
    source_snapshot: String,
    report_sha256: String,
    report_artifact: String,
    verification_artifact: String,
    pub(super) rubric_text_verbatim: String,
    pub(super) cases: Vec<EvaluatorCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvaluatorCase {
    pub(super) id: String,
    pub(super) family: String,
    pub(super) abstention: serde_json::Value,
    pub(super) oracle_text_verbatim: String,
}

impl EvaluatorPackage {
    fn validate(&self, frozen: bool, bundle: &Bundle, snapshot: &str) -> Result<(), String> {
        if self.source_snapshot != snapshot
            || !(bare_sha256(&self.report_sha256)
                || (!frozen && af_contracts::is_digest(&self.report_sha256)))
            || !af_contracts::is_digest(&self.report_artifact)
            || !af_contracts::is_digest(&self.verification_artifact)
            || !nonempty(&self.rubric_text_verbatim, TEXT_LIMIT)
            || !nonempty(&self.schema, 256)
            || !nonempty(&self.status, 256)
            || (!frozen && (self.schema != FIXTURE_SCHEMA || self.status != "synthetic"))
        {
            return Err(
                "Evaluator header differs from its expected format or source provenance".into(),
            );
        }
        let expected: BTreeSet<_> = bundle.cases.iter().map(|case| case.id.as_str()).collect();
        let mut seen = BTreeSet::new();
        for case in &self.cases {
            if !expected.contains(case.id.as_str())
                || !seen.insert(case.id.as_str())
                || !nonempty(&case.family, 256)
                || !(case.abstention.is_boolean()
                    || case
                        .abstention
                        .as_str()
                        .is_some_and(|value| nonempty(value, 256)))
                || !nonempty(&case.oracle_text_verbatim, TEXT_LIMIT)
            {
                return Err("Evaluator contains an invalid, foreign or repeated case".into());
            }
        }
        if seen != expected {
            return Err("Evaluator must cover every captured subject case".into());
        }
        Ok(())
    }
}

fn bare_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn load(
    options: &EvaluatorCheck,
    repo: &Path,
    opened: &OpenedTask,
    plan: &ExternalPlan,
) -> Result<EvaluatorPackage, String> {
    if !af_contracts::is_digest(&options.evaluator_sha256)
        || (plan.pins.source_revision == SOURCE_REVISION
            && options.evaluator_sha256 != FROZEN_EVALUATOR_SHA256)
    {
        return Err("Evaluator requires the exact SHA-256 for the captured source".into());
    }
    let path = options
        .evaluator
        .canonicalize()
        .map_err(|_| "Evaluator file unavailable")?;
    if path.starts_with(repo) || path.starts_with(&opened.state) {
        return Err("Evaluator file must be outside the checkout and Task Store".into());
    }
    let bytes = af_store::local_fs::read_bounded(&path, INPUT_LIMIT)
        .map_err(|_| "Evaluator must be a bounded regular file")?;
    if file_sha256(&bytes) != options.evaluator_sha256 {
        return Err("Evaluator digest differs from its exact bytes".into());
    }
    let (snapshot, _, manifest) = af_runtime::task::source::source_snapshot(
        &opened.cas,
        &opened.task.revision.inputs["source"],
    )?;
    let blob = af_store::canonical::blob_content_id(&bytes);
    if manifest.entries.iter().any(|entry| entry.content == blob) {
        return Err("Evaluator package is present in the captured source".into());
    }
    // Serde errors may contain the private input, so do not print them.
    let evaluator: EvaluatorPackage =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private evaluator JSON")?;
    let bundle: Bundle = read_input(&opened.cas, &plan.pins.bundle)?;
    evaluator.validate(
        plan.pins.source_revision == SOURCE_REVISION,
        &bundle,
        &snapshot,
    )?;
    Ok(evaluator)
}

pub(in crate::task_commands::agent_bench) fn check_evaluator(
    options: EvaluatorCheck,
) -> Result<i32, String> {
    let repo = options.repo.canonicalize().map_err(|e| e.to_string())?;
    let opened = open_task(&options.id, &repo, options.state.as_deref())?;
    let plan = read_plan(&opened.cas, &opened.task)?;
    load(&options, &repo, &opened, &plan)?;
    println!("Evaluator package validated for the captured subject source");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_header_requires_bare_lowercase_backing_report_digest() {
        let snapshot = file_sha256(b"synthetic snapshot");
        let mut evaluator = EvaluatorPackage {
            schema: FIXTURE_SCHEMA.into(),
            status: "synthetic".into(),
            source_snapshot: snapshot.clone(),
            report_sha256: file_sha256(b"synthetic backing report")[7..].to_owned(),
            report_artifact: file_sha256(b"synthetic report artifact"),
            verification_artifact: file_sha256(b"synthetic verification artifact"),
            rubric_text_verbatim: "synthetic rubric".into(),
            cases: vec![EvaluatorCase {
                id: "N01".into(),
                family: "synthetic".into(),
                abstention: json!(false),
                oracle_text_verbatim: "synthetic oracle".into(),
            }],
        };
        let bundle = Bundle {
            schema: "benchmark-subject".into(),
            common_instruction: "Answer the question.".into(),
            cases: vec![PromptCase {
                id: "N01".into(),
                prompt: "Where is the entry point?".into(),
            }],
        };
        assert!(evaluator.validate(true, &bundle, &snapshot).is_ok());
        evaluator.report_sha256 = format!("sha256:{}", evaluator.report_sha256);
        assert!(evaluator.validate(true, &bundle, &snapshot).is_err());
        assert!(evaluator.validate(false, &bundle, &snapshot).is_ok());
        evaluator.report_sha256 = evaluator.report_sha256[7..].to_ascii_uppercase();
        assert!(evaluator.validate(true, &bundle, &snapshot).is_err());
    }
}
