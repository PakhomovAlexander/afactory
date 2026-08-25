//! One resolver for Subject and Change Set agreement across execution and projections.

use std::sync::Arc;

use review_core::{ChangeSetV1, SubjectKind, SubjectV1};

use crate::{Cas, StoreError};

#[derive(Debug, Clone)]
pub struct ResolvedSubject {
    pub subject: SubjectV1,
    pub change_set: Option<Arc<ResolvedChangeSet>>,
}

/// A Change Set whose typed value, content identity, and encoded length were established from one
/// verified CAS read. Consumers can share this capability without re-serializing the typed value
/// and accidentally imposing a stronger canonical form than the published schema.
#[derive(Debug, Clone)]
pub struct ResolvedChangeSet {
    artifact_id: String,
    change_set: Arc<ChangeSetV1>,
    encoded_bytes: usize,
}

impl ResolvedChangeSet {
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    pub fn change_set(&self) -> &Arc<ChangeSetV1> {
        &self.change_set
    }

    pub fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSubjectScope {
    pub subject: SubjectV1,
    pub changed_paths: Option<Arc<[String]>>,
}

pub fn resolve_subject(cas: &Cas, subject_id: &str) -> Result<ResolvedSubject, StoreError> {
    let subject = read_subject(cas, subject_id)?;
    let change_set = match subject.kind {
        SubjectKind::WholeTree => None,
        SubjectKind::Diff => {
            let change_set_id = subject.change_set_id.as_deref().ok_or_else(|| {
                StoreError::Artifact(format!("diff Subject {subject_id} has no Change Set"))
            })?;
            let change_set_bytes = cas.get(change_set_id).map_err(|error| {
                StoreError::Artifact(format!(
                    "Subject {subject_id} references unreadable Change Set {change_set_id}: {error}"
                ))
            })?;
            let change_set: ChangeSetV1 =
                serde_json::from_slice(&change_set_bytes).map_err(|error| {
                    StoreError::Artifact(format!(
                        "Change Set {change_set_id} is malformed: {error}"
                    ))
                })?;
            change_set.validate().map_err(|error| {
                StoreError::Artifact(format!("Change Set {change_set_id} is invalid: {error}"))
            })?;
            validate_subject_binding(subject_id, &subject, change_set_id, &change_set)?;
            Some(Arc::new(ResolvedChangeSet {
                artifact_id: change_set_id.to_string(),
                change_set: Arc::new(change_set),
                encoded_bytes: change_set_bytes.len(),
            }))
        }
    };
    Ok(ResolvedSubject {
        subject,
        change_set,
    })
}

pub fn resolve_subject_scope(
    cas: &Cas,
    subject_id: &str,
) -> Result<ResolvedSubjectScope, StoreError> {
    let subject = read_subject(cas, subject_id)?;
    let changed_paths = match subject.kind {
        SubjectKind::WholeTree => None,
        SubjectKind::Diff => {
            let change_set_id = subject.change_set_id.as_deref().ok_or_else(|| {
                StoreError::Artifact(format!("diff Subject {subject_id} has no Change Set"))
            })?;
            let change_set_bytes = cas.get(change_set_id).map_err(|error| {
                StoreError::Artifact(format!(
                    "Subject {subject_id} references unreadable Change Set {change_set_id}: {error}"
                ))
            })?;
            let change_set: ChangeSetV1 =
                serde_json::from_slice(&change_set_bytes).map_err(|error| {
                    StoreError::Artifact(format!(
                        "Change Set {change_set_id} is malformed: {error}"
                    ))
                })?;
            change_set.validate_scope_shape().map_err(|error| {
                StoreError::Artifact(format!("Change Set {change_set_id} is invalid: {error}"))
            })?;
            validate_subject_binding(subject_id, &subject, change_set_id, &change_set)?;
            Some(Arc::from(change_set.changed_paths))
        }
    };
    Ok(ResolvedSubjectScope {
        subject,
        changed_paths,
    })
}

fn read_subject(cas: &Cas, subject_id: &str) -> Result<SubjectV1, StoreError> {
    let subject_bytes = cas.get(subject_id).map_err(|error| {
        StoreError::Artifact(format!("Subject {subject_id} cannot be read: {error}"))
    })?;
    let subject: SubjectV1 = serde_json::from_slice(&subject_bytes).map_err(|error| {
        StoreError::Artifact(format!("Subject {subject_id} is malformed: {error}"))
    })?;
    subject.validate().map_err(|error| {
        StoreError::Artifact(format!("Subject {subject_id} is invalid: {error}"))
    })?;
    Ok(subject)
}

fn validate_subject_binding(
    subject_id: &str,
    subject: &SubjectV1,
    change_set_id: &str,
    change_set: &ChangeSetV1,
) -> Result<(), StoreError> {
    if change_set.base_snapshot_id != subject.base_snapshot_id.as_deref().unwrap_or("")
        || change_set.head_snapshot_id != subject.head_snapshot_id
    {
        return Err(StoreError::Artifact(format!(
            "Change Set {change_set_id} contradicts Subject {subject_id}"
        )));
    }
    Ok(())
}
