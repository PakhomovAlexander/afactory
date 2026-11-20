//! Signed file exchange. The Project Store lock serializes intake and sequence assignment.

use std::collections::BTreeMap;
use std::io::{Cursor, ErrorKind};
use std::path::Path;

use af_contracts::backlog::Report;
use af_contracts::backlog::intake::{
    IntakeRecord, MAX_POLICY_BYTES, Policy, Receipt, ReceiptSchema, Signed, SignedSchema,
    Submission, SubmissionSchema,
};
use af_contracts::{EventType, RunEvent};
use af_store::{NewEvent, canonical::blob_content_id, canonicalize, content_id, local_fs};
use minisign::{PublicKey, SecretKey, SecretKeyBox, SignatureBox};
use serde::Serialize;

use super::check_evidence;
use crate::ProjectStore;

const POLICY_FILE: &str = "backlog-intake-v1.json";
const AUTHORITY_STREAM: &str = "backlog-intake";
const ACK_PREFIX: &str = "backlog-ack/";

pub(super) struct AcceptedReport {
    pub report: Report,
    pub receipt: Signed<Receipt>,
}

struct IntakePolicy {
    authority: PublicKey,
    producer: Option<PublicKey>,
    enrolled: BTreeMap<String, PublicKey>,
}

pub(super) fn value_id(value: &impl Serialize) -> Result<String, String> {
    let value = serde_json::to_value(value).map_err(|_| "could not encode backlog intake")?;
    content_id(&value).map_err(|_| "invalid backlog intake identity".into())
}

fn signing_bytes(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(value).map_err(|_| "could not encode backlog intake")?;
    canonicalize(&value).map_err(|_| "invalid backlog signing bytes".into())
}

fn key_id(key: &PublicKey) -> String {
    blob_content_id(&key.to_bytes())
}

fn public_key(encoded: &str) -> Result<PublicKey, String> {
    let key = PublicKey::from_base64(encoded).map_err(|_| "invalid backlog public key")?;
    if key.to_base64() != encoded {
        return Err("backlog public key must have canonical Minisign encoding".into());
    }
    Ok(key)
}

fn signing_key(key: &PublicKey, path: &Path) -> Result<SecretKey, String> {
    let bytes = local_fs::read_bounded(path, 4096)
        .map_err(|error| format!("reading backlog signing key: {error}"))?;
    let encoded = std::str::from_utf8(&bytes).map_err(|_| "invalid backlog signing key")?;
    let secret = SecretKeyBox::from_string(encoded)
        .and_then(SecretKey::from_unencrypted_box)
        .map_err(|_| "backlog signing requires an unencrypted Minisign secret key")?;
    let derived = PublicKey::from_secret_key(&secret).map_err(|_| "invalid backlog signing key")?;
    if derived.to_bytes() != key.to_bytes() {
        return Err("backlog signing key does not match configured public key".into());
    }
    Ok(secret)
}

fn sign<T: Serialize>(
    schema: SignedSchema,
    body: T,
    key: &PublicKey,
    secret: &SecretKey,
) -> Result<Signed<T>, String> {
    let signature = minisign::sign(
        Some(key),
        secret,
        signing_bytes(&body)?.as_slice(),
        Some("af backlog intake"),
        None,
    )
    .map_err(|_| "could not sign backlog exchange")?
    .into_string();
    Ok(Signed {
        schema,
        body,
        signature,
    })
}

fn verify<T: Serialize>(
    signed: &Signed<T>,
    schema: SignedSchema,
    key: &PublicKey,
) -> Result<(), String> {
    signed.validate_signature(schema)?;
    let signature =
        SignatureBox::from_string(&signed.signature).map_err(|_| "invalid backlog signature")?;
    minisign::verify(
        key,
        &signature,
        Cursor::new(signing_bytes(&signed.body)?),
        true,
        false,
        false,
    )
    .map_err(|_| "backlog exchange authentication failed".into())
}

fn read_event(project: &str, event: &RunEvent) -> Result<IntakeRecord, String> {
    if event.event_type != EventType::BacklogIntakeStoredV1 || !event.artifact_refs.is_empty() {
        return Err("invalid backlog intake stream event".into());
    }
    let record: IntakeRecord = serde_json::from_value(event.payload.clone())
        .map_err(|_| "invalid backlog intake record")?;
    record.validate()?;
    let receipt = match &record {
        IntakeRecord::Accepted { report, receipt } => {
            check_evidence(report)?;
            check_receipt_report(&receipt.body, report)?;
            receipt
        }
        IntakeRecord::Acknowledged { receipt } => receipt,
    };
    if receipt.body.project_id != project {
        return Err("backlog intake record belongs to a different Project".into());
    }
    Ok(record)
}

fn check_receipt_report(receipt: &Receipt, report: &Report) -> Result<(), String> {
    if receipt.project_id != report.project_id
        || receipt.report_id != report.report_id
        || receipt.report_digest != value_id(report)?
    {
        return Err("backlog receipt does not cover the exact report".into());
    }
    Ok(())
}

fn event(record: IntakeRecord) -> Result<NewEvent, String> {
    let payload = serde_json::to_value(record).map_err(|_| "could not encode backlog intake")?;
    Ok(NewEvent::new(EventType::BacklogIntakeStoredV1, payload).at(
        af_contracts::task::collection::collected_time(crate::now_unix_ms()),
    ))
}

impl ProjectStore {
    fn intake_policy(&self) -> Result<Option<IntakePolicy>, String> {
        let bytes =
            match local_fs::read_bounded(&self.directory.join(POLICY_FILE), MAX_POLICY_BYTES) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(format!("reading backlog intake policy: {error}")),
            };
        let policy: Policy =
            serde_json::from_slice(&bytes).map_err(|_| "invalid backlog intake policy")?;
        if policy.project_id != self.project_id {
            return Err("backlog intake policy belongs to a different Project".into());
        }
        let authority = public_key(&policy.authority_key)?;
        let producer = policy.producer_key.as_deref().map(public_key).transpose()?;
        let mut enrolled = BTreeMap::new();
        for encoded in policy.enrolled_keys {
            let key = public_key(&encoded)?;
            if key_id(&key) == key_id(&authority) || enrolled.insert(key_id(&key), key).is_some() {
                return Err(
                    "backlog enrollment requires distinct producer and authority keys".into(),
                );
            }
        }
        if producer
            .as_ref()
            .is_some_and(|key| key_id(key) == key_id(&authority))
        {
            return Err("backlog producer and authority keys must be distinct".into());
        }
        Ok(Some(IntakePolicy {
            authority,
            producer,
            enrolled,
        }))
    }

    pub(super) fn backlog_authority_id(&self) -> Result<Option<String>, String> {
        Ok(self
            .intake_policy()?
            .map(|policy| key_id(&policy.authority)))
    }

    fn require_intake_policy(&self) -> Result<IntakePolicy, String> {
        self.intake_policy()?.ok_or_else(|| {
            format!(
                "no backlog authority configured in {}",
                self.directory.join(POLICY_FILE).display()
            )
        })
    }

    pub fn backlog_submission(&self, id: &str, key: &Path) -> Result<Signed<Submission>, String> {
        let policy = self.require_intake_policy()?;
        let producer = policy
            .producer
            .ok_or("no local backlog producer configured")?;
        let report = self
            .local_backlog_report(id)?
            .ok_or("no local report with that ID")?;
        sign(
            SignedSchema::Submission,
            Submission {
                schema: SubmissionSchema::V1,
                authority_id: key_id(&policy.authority),
                producer_id: key_id(&producer),
                report,
            },
            &producer,
            &signing_key(&producer, key)?,
        )
    }

    pub fn accept_backlog_submission(
        &mut self,
        signed: Signed<Submission>,
        key: &Path,
    ) -> Result<Signed<Receipt>, String> {
        let policy = self.require_intake_policy()?;
        let submission = &signed.body;
        submission.validate()?;
        if submission.report.project_id != self.project_id {
            return Err("backlog submission belongs to a different Project".into());
        }
        if submission.authority_id != key_id(&policy.authority) {
            return Err("backlog submission names a different Project authority".into());
        }
        let producer = policy
            .enrolled
            .get(&submission.producer_id)
            .ok_or("backlog producer is not enrolled")?;
        verify(&signed, SignedSchema::Submission, producer)?;
        let report = &submission.report;
        check_evidence(report)?;
        let secret = signing_key(&policy.authority, key)?;
        let accepted = self.backlog_intake()?;
        if accepted
            .values()
            .any(|row| row.receipt.body.authority_id != submission.authority_id)
        {
            return Err("backlog intake belongs to a different Project authority".into());
        }
        if let Some(previous) = accepted.get(&report.report_id) {
            if previous.report != *report
                || previous.receipt.body.producer_id != submission.producer_id
            {
                return Err("backlog report ID already names different content or producer".into());
            }
            verify(&previous.receipt, SignedSchema::Receipt, &policy.authority)?;
            return Ok(previous.receipt.clone());
        }
        if self
            .local_backlog_report(&report.report_id)?
            .is_some_and(|local| local != *report)
        {
            return Err("backlog report ID already names different local content".into());
        }

        let receipt = sign(
            SignedSchema::Receipt,
            Receipt {
                schema: ReceiptSchema::V1,
                project_id: report.project_id.clone(),
                authority_id: submission.authority_id.clone(),
                producer_id: submission.producer_id.clone(),
                report_id: report.report_id.clone(),
                report_digest: value_id(report)?,
                sequence: (accepted.len() as u64 + 1).into(),
            },
            &policy.authority,
            &secret,
        )?;
        self.events
            .append(
                AUTHORITY_STREAM,
                &self.cas,
                event(IntakeRecord::Accepted {
                    report: Box::new(signed.body.report),
                    receipt: receipt.clone(),
                })?,
            )
            .map_err(|_| "could not commit backlog authority acceptance")?;
        Ok(receipt)
    }

    pub fn acknowledge_backlog_submission(
        &mut self,
        receipt: Signed<Receipt>,
    ) -> Result<String, String> {
        let policy = self.require_intake_policy()?;
        let producer = policy
            .producer
            .ok_or("no local backlog producer configured")?;
        receipt.body.validate()?;
        if receipt.body.project_id != self.project_id
            || receipt.body.authority_id != key_id(&policy.authority)
            || receipt.body.producer_id != key_id(&producer)
        {
            return Err(
                "backlog receipt does not match configured Project, authority and producer".into(),
            );
        }
        verify(&receipt, SignedSchema::Receipt, &policy.authority)?;
        let report = self
            .local_backlog_report(&receipt.body.report_id)?
            .ok_or("no local report for backlog receipt")?;
        check_receipt_report(&receipt.body, &report)?;
        let receipt_id = value_id(&receipt.body)?;
        if let Some(previous) = self.backlog_acknowledgment(&report)? {
            if previous != receipt.body {
                return Err("backlog report already acknowledges a different receipt".into());
            }
            return Ok(receipt_id);
        }
        self.events
            .append(
                &format!("{ACK_PREFIX}{}", report.report_id),
                &self.cas,
                event(IntakeRecord::Acknowledged { receipt })?,
            )
            .map_err(|_| "could not commit backlog acknowledgment")?;
        Ok(receipt_id)
    }

    pub(super) fn backlog_acknowledgment(
        &self,
        report: &Report,
    ) -> Result<Option<Receipt>, String> {
        let events = self
            .events
            .replay(&format!("{ACK_PREFIX}{}", report.report_id))
            .map_err(|_| "could not replay backlog acknowledgment")?;
        match events.as_slice() {
            [] => Ok(None),
            [event] => {
                let IntakeRecord::Acknowledged { receipt } = read_event(&self.project_id, event)?
                else {
                    return Err("invalid event in backlog acknowledgment stream".into());
                };
                check_receipt_report(&receipt.body, report)?;
                Ok(Some(receipt.body))
            }
            _ => Err("backlog acknowledgment stream holds multiple records".into()),
        }
    }

    pub(super) fn backlog_intake(&self) -> Result<BTreeMap<String, AcceptedReport>, String> {
        let events = self
            .events
            .replay(AUTHORITY_STREAM)
            .map_err(|_| "could not replay backlog authority intake")?;
        let mut accepted = BTreeMap::new();
        for event in events {
            let IntakeRecord::Accepted { report, receipt } = read_event(&self.project_id, &event)?
            else {
                return Err("invalid event in backlog authority intake stream".into());
            };
            if receipt.body.sequence.get() != event.sequence + 1
                || accepted
                    .insert(
                        report.report_id.clone(),
                        AcceptedReport {
                            report: *report,
                            receipt,
                        },
                    )
                    .is_some()
            {
                return Err("invalid backlog authority sequence or duplicate report ID".into());
            }
        }
        Ok(accepted)
    }
}
