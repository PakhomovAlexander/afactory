//! A Task file's `context` and the project's `.af/rules.md` (ADR-0198): what the Worker reads
//! before the goal, bound by digest.
//!
//! A short goal may point at what it needs instead of carrying it: `"context":
//! ["docs/design.md#items", "crates/x/src/lib.rs"]`. Each entry names a file of the tree the
//! Task reads, with an optional `#section`; the project's standing rules live in `.af/rules.md`
//! of the Authority Snapshot and are listed first. The files are already in the captured
//! Snapshot, so nothing is copied: one `af/TaskReadFirst@1` artifact names each by its content
//! digest, the revision's provenance references it, and the Worker's requirements text ends with
//! the list.

use super::*;
use review_core::task::findings::{
    MAX_CONTEXT, TASK_READ_FIRST_SCHEMA, TASK_READ_FIRST_V1, TaskReadFirstEntryV1, TaskReadFirstV1,
};

/// Where a project keeps its standing rules.
pub(super) const RULES: &str = ".af/rules.md";

fn entry(manifest: &Manifest, reference: &str, what: &str) -> Result<TaskReadFirstEntryV1, String> {
    let path = reference.split('#').next().unwrap_or_default();
    if !review_config::task::shared::safe_relative_path(path) {
        return Err(format!(
            "context: {} is not a repository path; name a file of the tree as `path/to/file` \
             or `path/to/file#section`",
            preview::short(reference, 128)
        ));
    }
    let found = manifest.get(path).ok_or_else(|| {
        format!(
            "context: {} names {}, which {what} does not hold; name a committed file, or \
             commit it first",
            preview::short(reference, 128),
            preview::short(path, 128)
        )
    })?;
    if found.kind != EntryKind::File && found.kind != EntryKind::Executable {
        return Err(format!(
            "context: {} is not a regular file of {what}",
            preview::short(path, 128)
        ));
    }
    Ok(TaskReadFirstEntryV1 {
        reference: reference.to_owned(),
        path: path.to_owned(),
        digest: found.content.clone(),
    })
}

/// `.af/rules.md` of the Authority Snapshot `authority`, when it holds one.
pub(super) fn rules(authority: &Manifest) -> Result<Option<TaskReadFirstEntryV1>, String> {
    authority
        .get(RULES)
        .is_some()
        .then(|| entry(authority, RULES, "the Authority Snapshot"))
        .transpose()
}

/// The read-first record of a Task: the project's `rules` when it keeps them, then each
/// `context` reference in `source`. `None` when there is nothing to read first, so a Task
/// without either keeps the revision it always had.
pub(super) fn capture(
    context: &[String],
    rules: Option<TaskReadFirstEntryV1>,
    source: &Manifest,
) -> Result<Option<TaskReadFirstV1>, String> {
    if context.len() > MAX_CONTEXT {
        return Err(format!(
            "context names {} files; a Task file names at most {MAX_CONTEXT}",
            context.len()
        ));
    }
    let mut entries = Vec::new();
    for reference in context {
        if entries
            .iter()
            .any(|seen: &TaskReadFirstEntryV1| &seen.reference == reference)
        {
            return Err(format!(
                "context names {} twice",
                preview::short(reference, 128)
            ));
        }
        entries.push(entry(source, reference, "the Task's source tree")?);
    }
    if rules.is_none() && entries.is_empty() {
        return Ok(None);
    }
    let record = TaskReadFirstV1 {
        schema: TASK_READ_FIRST_SCHEMA.into(),
        rules,
        context: entries,
    };
    record.validate()?;
    Ok(Some(record))
}

/// Record `record` as an `af/TaskReadFirst@1` artifact referencing the content it names.
pub(super) fn record(cas: &Cas, record: &TaskReadFirstV1) -> Result<String, String> {
    let mut refs: Vec<String> = record
        .rules
        .iter()
        .chain(&record.context)
        .map(|entry| entry.digest.clone())
        .collect();
    refs.sort();
    refs.dedup();
    let payload = serde_json::to_value(record).map_err(|e| e.to_string())?;
    let (id, _) = cas
        .put_artifact(TASK_READ_FIRST_V1, producer(), refs, None, payload)
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// A Task file's `base` table: the commit whose tree the judging Workers read at `../base`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BaseRefV1 {
    pub(super) commit: String,
}

/// A declared base as captured: the commit Git resolved and its tree.
pub(super) struct DeclaredBase {
    commit: String,
    content_digest: String,
    manifest: Manifest,
}

/// Capture the committed tree `base` names from `repo`, refusing a name Git cannot resolve.
pub(super) fn capture_base(
    repo: &Repo,
    cas: &Cas,
    base: &BaseRefV1,
) -> Result<DeclaredBase, String> {
    let shown = preview::short(&base.commit, 80);
    if base.commit.trim().is_empty() || base.commit.starts_with('-') {
        return Err(format!("base.commit {shown:?} is not a commit"));
    }
    let captured = Capture::new(repo, cas)
        .committed(&base.commit)
        .map_err(|error| {
            format!(
                "base.commit {shown}: {error}; name a commit of this repository                  (`git rev-parse` resolves it)"
            )
        })?;
    let commit = captured
        .source_revision
        .clone()
        .ok_or_else(|| format!("base.commit {shown} resolved to no commit"))?;
    Ok(DeclaredBase {
        commit,
        content_digest: captured.content_digest,
        manifest: captured.manifest,
    })
}

/// Record a declared base as its Snapshot and an `af/TaskBase@1` artifact the source
/// environment finds in the revision's provenance.
pub(super) fn record_base(
    cas: &Cas,
    base: &DeclaredBase,
    repository_id: &str,
) -> Result<String, String> {
    use review_core::task::findings::{TASK_BASE_SCHEMA, TASK_BASE_V1, TaskBaseV1};
    let origin = cas
        .put_json(
            &json!({"schema":"af.task-source-origin/1","repository_id":repository_id,
            "source_revision":base.commit,"content_digest":base.content_digest}),
        )
        .map_err(|e| e.to_string())?;
    let snapshot_id = capture_snapshot(cas, &base.manifest, &origin, None)?;
    let record = TaskBaseV1 {
        schema: TASK_BASE_SCHEMA.into(),
        commit: base.commit.clone(),
        snapshot_id: snapshot_id.clone(),
    };
    record.validate()?;
    let payload = serde_json::to_value(&record).map_err(|e| e.to_string())?;
    let (id, _) = cas
        .put_artifact(
            TASK_BASE_V1,
            producer(),
            vec![snapshot_id.clone(), origin],
            Some(snapshot_id),
            payload,
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_source_git::Entry;

    fn manifest(files: &[(&str, &[u8])]) -> Manifest {
        Manifest::new(
            files
                .iter()
                .map(|(path, bytes)| Entry {
                    path: (*path).into(),
                    kind: EntryKind::File,
                    content: review_source_git::digest_bytes(bytes),
                    size: bytes.len() as u64,
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn rules_come_first_and_each_context_file_is_bound_by_its_digest() {
        let authority = manifest(&[(RULES, b"rules")]);
        let source = manifest(&[("docs/design.md", b"v1"), ("src/lib.rs", b"fn x() {}")]);
        let context = vec!["docs/design.md#items".to_owned(), "src/lib.rs".to_owned()];
        let rules = rules(&authority).unwrap();
        let record = capture(&context, rules.clone(), &source).unwrap().unwrap();
        assert_eq!(record.rules.as_ref().unwrap().path, RULES);
        assert_eq!(record.context[0].path, "docs/design.md");
        assert_eq!(
            record.context[0].digest,
            review_source_git::digest_bytes(b"v1")
        );
        let changed = manifest(&[("docs/design.md", b"v2"), ("src/lib.rs", b"fn x() {}")]);
        let again = capture(&context, rules, &changed).unwrap().unwrap();
        assert_ne!(again, record, "a changed file is a changed record");
    }

    #[test]
    fn nothing_to_read_records_nothing_and_a_missing_file_is_named() {
        let empty = manifest(&[("a.rs", b"a")]);
        assert!(rules(&empty).unwrap().is_none());
        assert!(capture(&[], None, &empty).unwrap().is_none());
        let refused = capture(&["docs/absent.md#x".to_owned()], None, &empty).unwrap_err();
        assert!(refused.contains("docs/absent.md"), "{refused}");
        let escape = capture(&["../secret".to_owned()], None, &empty).unwrap_err();
        assert!(escape.contains("not a repository path"), "{escape}");
    }
}
