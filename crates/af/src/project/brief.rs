//! CLI presentation and application dependencies of Project Briefs.

use af_contracts::task::pipeline::PipelineContractV1;
use af_contracts::task::project_brief::{
    ORIENT, PROJECT_BRIEF_PREVIEW_V1, ProjectBriefNoneReasonV1, ProjectBriefPreviewOutcomeV1,
    ProjectBriefPreviewV1, RequestedBriefV1, requested_variant,
};
use af_contracts::task::{ArtifactInputV1, TaskRevisionV1};
pub(crate) use af_project::brief::{
    BriefTable, Resolver, SourceOrigin, StoreUse, admit_bound, check_request_cap, constructs,
    identities, load_policy, manifest_entry, none_line, pin_of, preview_lines, render, value_of,
};
use af_store::Cas;
use std::collections::BTreeMap;
use std::path::Path;
const PREVIEW_PORT: &str = "project";

pub(crate) fn capture(
    cas: &Cas,
    repo: &Path,
    source: &SourceOrigin,
    constructs: bool,
    bound: &BTreeMap<String, ArtifactInputV1>,
) -> Result<Option<String>, String> {
    af_project::brief::capture(&super::Application, cas, repo, source, constructs, bound)
}

pub(crate) fn record_receipts(
    cas: &Cas,
    contract: &PipelineContractV1,
    revision: &mut TaskRevisionV1,
) -> Result<(), String> {
    af_project::brief::record_receipts(
        cas,
        contract,
        revision,
        crate::task_commands::recorded_input_bindings,
    )
}

/// `af project brief`: the exact bytes the port would carry for `variant` (default `orient`) of
/// the newest complete Survey or the one `survey` names, token-free and recording nothing.
/// `--json` prints `af/project-brief-preview@1`, with `none` and its reason (exit 0) where no
/// Brief is rendered; the text form without a Survey exits 1 with the preview's sentence.
pub(crate) fn command(
    variant: Option<String>,
    survey: Option<String>,
    json: bool,
) -> Result<i32, String> {
    let checkout = crate::project::require_checkout(Path::new("."))?;
    let variants = load_policy(&super::Application, &checkout.toplevel)?;
    let name = variant.unwrap_or_else(|| ORIENT.into());
    let none = |reason| {
        let preview = ProjectBriefPreviewV1 {
            schema: PROJECT_BRIEF_PREVIEW_V1.into(),
            variant: name.clone(),
            outcome: ProjectBriefPreviewOutcomeV1::None,
            reason: Some(reason),
            brief_id: None,
            brief: None,
            manifest: None,
        };
        if json {
            print_preview(&preview)?;
            return Ok(0);
        }
        match reason {
            ProjectBriefNoneReasonV1::VariantNone => {
                eprintln!("{}", none_line(PREVIEW_PORT, reason));
                Ok(0)
            }
            // `af project: none (…)`: the preview's sentence, after the command's own prefix.
            ProjectBriefNoneReasonV1::NoSurvey => Err(none_line(PREVIEW_PORT, reason)
                .trim_start_matches(&format!("{PREVIEW_PORT}: "))
                .to_string()),
        }
    };
    let definition = match requested_variant(&variants, &name) {
        None => {
            return Err(format!(
                "no variant {name}: neither a [brief.{name}] table nor a built-in variant \
                 (orient, none) defines it"
            ));
        }
        Some(RequestedBriefV1::Nothing) => return none(ProjectBriefNoneReasonV1::VariantNone),
        Some(RequestedBriefV1::Render(definition)) => definition,
    };
    let Some(pin) = pin_of(
        &super::Application,
        &checkout.repository_id,
        survey.as_deref(),
        StoreUse::Preview,
    )?
    else {
        return none(ProjectBriefNoneReasonV1::NoSurvey);
    };
    let (brief, bytes) = render(&pin, &name, &definition)?;
    if !json {
        println!(
            "{}",
            std::str::from_utf8(&bytes).map_err(|error| error.to_string())?
        );
        return Ok(0);
    }
    let (_, brief_id) = identities(&pin, &brief)?;
    let payload = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let preview = ProjectBriefPreviewV1 {
        schema: PROJECT_BRIEF_PREVIEW_V1.into(),
        variant: name.clone(),
        outcome: ProjectBriefPreviewOutcomeV1::Brief,
        reason: None,
        manifest: Some(manifest_entry(PREVIEW_PORT, &brief_id, payload)?),
        brief_id: Some(brief_id),
        brief: Some(brief),
    };
    print_preview(&preview)?;
    Ok(0)
}

/// The preview's canonical bytes, so its `brief` member is byte for byte the text form.
fn print_preview(preview: &ProjectBriefPreviewV1) -> Result<(), String> {
    preview.validate()?;
    let bytes = crate::project::survey::canonical_bytes(&value_of(preview)?)?;
    println!(
        "{}",
        std::str::from_utf8(&bytes).map_err(|error| error.to_string())?
    );
    Ok(())
}
