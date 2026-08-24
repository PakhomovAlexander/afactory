//! One resolver for Subject and Change Set agreement across execution and projections.

use std::sync::Arc;

use review_core::{ChangeSetV1, SubjectKind, SubjectV1};

use crate::{Cas, StoreError};

#[derive(Debug, Clone)]
pub struct ResolvedSubject {
    pub subject: SubjectV1,
    pub changed_paths: Option<Arc<[String]>>,
}

pub fn resolve_subject(cas: &Cas, subject_id: &str) -> Result<ResolvedSubject, StoreError> {
    resolve(cas, subject_id, true)
}

pub fn resolve_subject_scope(cas: &Cas, subject_id: &str) -> Result<ResolvedSubject, StoreError> {
    resolve(cas, subject_id, false)
}

fn resolve(
    cas: &Cas,
    subject_id: &str,
    validate_patch: bool,
) -> Result<ResolvedSubject, StoreError> {
    let subject_bytes = cas.get(subject_id).map_err(|error| {
        StoreError::Artifact(format!("Subject {subject_id} cannot be read: {error}"))
    })?;
    let subject: SubjectV1 = serde_json::from_slice(&subject_bytes).map_err(|error| {
        StoreError::Artifact(format!("Subject {subject_id} is malformed: {error}"))
    })?;
    subject.validate().map_err(|error| {
        StoreError::Artifact(format!("Subject {subject_id} is invalid: {error}"))
    })?;

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
            let validation = if validate_patch {
                change_set.validate()
            } else {
                change_set.validate_scope_shape()
            };
            validation.map_err(|error| {
                StoreError::Artifact(format!("Change Set {change_set_id} is invalid: {error}"))
            })?;
            if change_set.base_snapshot_id != subject.base_snapshot_id.as_deref().unwrap_or("")
                || change_set.head_snapshot_id != subject.head_snapshot_id
            {
                return Err(StoreError::Artifact(format!(
                    "Change Set {change_set_id} contradicts Subject {subject_id}"
                )));
            }
            Some(Arc::from(change_set.changed_paths))
        }
    };
    Ok(ResolvedSubject {
        subject,
        changed_paths,
    })
}
