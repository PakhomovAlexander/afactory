//! A Task file's `context` and the project's `.af/rules.md` (ADR-0212, ADR-0254): what the
//! Worker reads before the goal, bound by digest in the tree it reads.
//!
//! A short goal may point at what it needs instead of carrying it: `"context":
//! ["docs/design.md#items", "crates/x/src/lib.rs"]`. Each entry names a file of the tree the
//! Task reads, with an optional `#section`; the project's standing rules live in `.af/rules.md`
//! of that same tree and are listed first. The files are already in the captured Snapshot, so
//! nothing is copied: one `af/TaskReadFirst@2` artifact names that Snapshot and each file by its
//! content digest there, the revision's provenance references it, and the Worker's requirements
//! text ends with the list. A continued Task reads its candidate, so it lists the candidate's
//! rules, and its plan says when they differ from what the continued Task read.

use super::*;
use review_core::task::findings::{
    MAX_CONTEXT, TASK_READ_FIRST_SCHEMA, TASK_READ_FIRST_V1, TASK_READ_FIRST_V2,
    TaskReadFirstEntryV2, TaskReadFirstV2,
};

/// Where a project keeps its standing rules.
pub(super) const RULES: &str = ".af/rules.md";

fn entry(manifest: &Manifest, reference: &str, what: &str) -> Result<TaskReadFirstEntryV2, String> {
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
    Ok(TaskReadFirstEntryV2 {
        reference: reference.to_owned(),
        path: path.to_owned(),
        digest: found.content.clone(),
    })
}

/// The read-first record of a Task whose Worker reads the tree `snapshot_id` holds, `tree`: that
/// tree's `.af/rules.md` when it holds one, then each `context` reference in it. `None` when
/// there is nothing to read first, so a Task without either keeps the revision it always had.
/// Every entry is that tree's, so a Worker is never told to read bytes its tree does not hold.
pub(super) fn capture(
    context: &[String],
    snapshot_id: &str,
    tree: &Manifest,
) -> Result<Option<TaskReadFirstV2>, String> {
    if context.len() > MAX_CONTEXT {
        return Err(format!(
            "context names {} files; a Task file names at most {MAX_CONTEXT}",
            context.len()
        ));
    }
    let rules = tree
        .get(RULES)
        .is_some()
        .then(|| entry(tree, RULES, "the Task's source tree"))
        .transpose()?;
    let mut entries = Vec::new();
    for reference in context {
        if entries
            .iter()
            .any(|seen: &TaskReadFirstEntryV2| &seen.reference == reference)
        {
            return Err(format!(
                "context names {} twice",
                preview::short(reference, 128)
            ));
        }
        entries.push(entry(tree, reference, "the Task's source tree")?);
    }
    if rules.is_none() && entries.is_empty() {
        return Ok(None);
    }
    let record = TaskReadFirstV2 {
        schema: TASK_READ_FIRST_SCHEMA.into(),
        snapshot_id: snapshot_id.to_owned(),
        rules,
        context: entries,
    };
    record.validate()?;
    Ok(Some(record))
}

/// Record `record` as an `af/TaskReadFirst@2` artifact referencing its tree and the content it
/// names.
pub(super) fn record(cas: &Cas, record: &TaskReadFirstV2) -> Result<String, String> {
    let mut refs: Vec<String> = record
        .rules
        .iter()
        .chain(&record.context)
        .map(|entry| entry.digest.clone())
        .chain(std::iter::once(record.snapshot_id.clone()))
        .collect();
    refs.sort();
    refs.dedup();
    let payload = serde_json::to_value(record).map_err(|e| e.to_string())?;
    let (id, _) = cas
        .put_artifact(TASK_READ_FIRST_V2, producer(), refs, None, payload)
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// The read-first record `revision` carries in its provenance, `None` when it lists nothing to
/// read. An `af/TaskReadFirst@1` record, which an earlier release wrote without naming its tree,
/// is refused by name (ADR-0145), never read as one of this release's.
pub(super) fn recorded(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<TaskReadFirstV2>, String> {
    for id in &revision.provenance.input_artifact_ids {
        let Some(envelope) = cas.get_optional_artifact(id).map_err(|e| e.to_string())? else {
            continue;
        };
        if envelope.artifact_type == TASK_READ_FIRST_V1 {
            return Err(format!(
                "Task `{}` lists its read-first files as {TASK_READ_FIRST_V1}, which an earlier \
                 release wrote without naming the tree they were read from; this release reads \
                 only {TASK_READ_FIRST_V2} (ADR-0254). Plan the work again from its Task file \
                 with `af task start --file TASK_FILE`",
                preview::text(&revision.task_id)
            ));
        }
        if envelope.artifact_type != TASK_READ_FIRST_V2 {
            continue;
        }
        let record: TaskReadFirstV2 =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        record.validate()?;
        return Ok(Some(record));
    }
    Ok(None)
}

/// What the plan of a continued Task says about its rules: the tree they come from, and, when
/// the candidate's `.af/rules.md` is not what the continued Task's Worker was given, both
/// digests. `None` when the new Task lists nothing to read and the continued one listed no rules.
pub(super) fn continued_rules_lines(
    continued: &str,
    before: Option<&TaskReadFirstV2>,
    after: Option<&TaskReadFirstV2>,
) -> Vec<String> {
    let digest = |record: Option<&TaskReadFirstV2>| {
        record
            .and_then(|record| record.rules.as_ref())
            .map(|rules| rules.digest.clone())
    };
    let (was, now) = (digest(before), digest(after));
    let mut lines = Vec::new();
    if let Some(after) = after {
        lines.push(format!(
            "read first: from Snapshot {}, the reviewed candidate of Task `{}` this Task continues",
            after.snapshot_id,
            preview::text(continued)
        ));
    }
    match (&was, &now) {
        (Some(was), Some(now)) if was != now => lines.push(format!(
            "rules: the candidate's {RULES} ({now}) differs from what Task `{}` read ({was}); \
             this Task's Worker reads the candidate's",
            preview::text(continued)
        )),
        (Some(was), None) => lines.push(format!(
            "rules: the candidate holds no {RULES}; Task `{}` read one ({was}), and this \
             Task's Worker is given none",
            preview::text(continued)
        )),
        (None, Some(now)) => lines.push(format!(
            "rules: the candidate adds {RULES} ({now}); Task `{}` read none, and this Task's \
             Worker reads the candidate's",
            preview::text(continued)
        )),
        _ => {}
    }
    lines
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

    fn id(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    #[test]
    fn rules_come_first_and_each_file_is_bound_by_its_digest_in_the_named_tree() {
        let source = manifest(&[
            (RULES, b"rules"),
            ("docs/design.md", b"v1"),
            ("src/lib.rs", b"fn x() {}"),
        ]);
        let context = vec!["docs/design.md#items".to_owned(), "src/lib.rs".to_owned()];
        let record = capture(&context, &id('1'), &source).unwrap().unwrap();
        assert_eq!(record.snapshot_id, id('1'));
        assert_eq!(record.rules.as_ref().unwrap().path, RULES);
        assert_eq!(
            record.rules.as_ref().unwrap().digest,
            review_source_git::digest_bytes(b"rules")
        );
        assert_eq!(record.context[0].path, "docs/design.md");
        assert_eq!(
            record.context[0].digest,
            review_source_git::digest_bytes(b"v1")
        );
        // The rules are the tree's own: a tree whose rules changed lists its own bytes.
        let changed = manifest(&[
            (RULES, b"other rules"),
            ("docs/design.md", b"v2"),
            ("src/lib.rs", b"fn x() {}"),
        ]);
        let again = capture(&context, &id('2'), &changed).unwrap().unwrap();
        assert_ne!(again, record, "a changed file is a changed record");
        assert_eq!(
            again.rules.as_ref().unwrap().digest,
            review_source_git::digest_bytes(b"other rules")
        );
    }

    #[test]
    fn nothing_to_read_records_nothing_and_a_missing_file_is_named() {
        let empty = manifest(&[("a.rs", b"a")]);
        assert!(capture(&[], &id('1'), &empty).unwrap().is_none());
        let refused = capture(&["docs/absent.md#x".to_owned()], &id('1'), &empty).unwrap_err();
        assert!(refused.contains("docs/absent.md"), "{refused}");
        let escape = capture(&["../secret".to_owned()], &id('1'), &empty).unwrap_err();
        assert!(escape.contains("not a repository path"), "{escape}");
    }

    #[test]
    fn a_continued_plan_names_the_tree_and_a_change_of_rules() {
        let with = |snapshot: char, rules: &[u8]| {
            capture(&[], &id(snapshot), &manifest(&[(RULES, rules)]))
                .unwrap()
                .unwrap()
        };
        let before = with('1', b"old");
        let same = with('2', b"old");
        let lines = continued_rules_lines("first", Some(&before), Some(&same));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(&id('2')) && lines[0].contains("`first`"));
        let changed = with('3', b"new");
        let lines = continued_rules_lines("first", Some(&before), Some(&changed));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[1].contains(&review_source_git::digest_bytes(b"new"))
                && lines[1].contains(&review_source_git::digest_bytes(b"old"))
                && lines[1].contains("reads the candidate's"),
            "{lines:?}"
        );
        let removed = continued_rules_lines("first", Some(&before), None);
        assert!(removed[0].contains("holds no .af/rules.md"), "{removed:?}");
        assert!(continued_rules_lines("first", None, None).is_empty());
    }

    #[test]
    fn a_read_first_record_of_an_earlier_release_is_refused_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let old = cas
            .put_artifact(
                TASK_READ_FIRST_V1,
                producer(),
                vec![],
                None,
                json!({"schema":"af.task-read-first/1","rules":{"reference":RULES,"path":RULES,
                    "digest":id('a')}}),
            )
            .unwrap()
            .0;
        let mut revision: TaskRevisionV1 = serde_json::from_value(json!({
            "task_id":"first","revision":1,"kind":"implement","goal":"g","inputs":{},
            "required_outputs":{},"acceptance":{},
            "provenance":{"adapter_id":id('c'),"input_artifact_ids":[old]},
            "authority":{"policy_id":id('d'),"allowed_effects":[],"data_destinations":[]},
            "limits":{"tokens":1,"max_attempts":1,"deadline_unix_ms":1,
                "verification":{"tokens":0,"attempts":0,"wall_ms":0}},
            "strategy":"small","facts":{}
        }))
        .unwrap();
        let refused = recorded(&cas, &revision).unwrap_err();
        assert!(
            refused.contains("af/TaskReadFirst@1") && refused.contains("`first`"),
            "{refused}"
        );
        let record = capture(&[], &id('1'), &manifest(&[(RULES, b"r")]))
            .unwrap()
            .unwrap();
        revision.provenance.input_artifact_ids = vec![super::record(&cas, &record).unwrap()];
        assert_eq!(recorded(&cas, &revision).unwrap(), Some(record));
    }
}
