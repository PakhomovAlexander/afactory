//! Versioned offline Project backlog exchange and durable intake records.

use serde::{Deserialize, Serialize};

use super::{Report, is_report_id};
use crate::{is_digest, task::usage::DecimalU64};

pub const MAX_EXCHANGE_BYTES: u64 = 256 * 1024;
pub const MAX_POLICY_BYTES: u64 = 64 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: PolicySchema,
    pub project_id: String,
    pub authority_key: String,
    pub producer_key: Option<String>,
    pub enrolled_keys: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub enum PolicySchema {
    #[serde(rename = "af/backlog-intake-policy@1")]
    V1,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub schema: SubmissionSchema,
    pub authority_id: String,
    pub producer_id: String,
    pub report: Report,
}

#[derive(Deserialize, Serialize)]
pub enum SubmissionSchema {
    #[serde(rename = "af/backlog-submission@1")]
    V1,
}

impl Submission {
    pub fn validate(&self) -> Result<(), String> {
        if !is_digest(&self.authority_id) || !is_digest(&self.producer_id) {
            return Err("invalid backlog submission identity".into());
        }
        self.report.validate()
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: ReceiptSchema,
    pub project_id: String,
    pub authority_id: String,
    pub producer_id: String,
    pub report_id: String,
    pub report_digest: String,
    pub sequence: DecimalU64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum ReceiptSchema {
    #[serde(rename = "af/backlog-authority-receipt@1")]
    V1,
}

impl Receipt {
    pub fn validate(&self) -> Result<(), String> {
        if [
            &self.project_id,
            &self.authority_id,
            &self.producer_id,
            &self.report_digest,
        ]
        .into_iter()
        .all(|id| is_digest(id))
            && is_report_id(&self.report_id)
            && self.sequence.get() > 0
        {
            Ok(())
        } else {
            Err("invalid backlog authority receipt identity".into())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signed<T> {
    pub schema: SignedSchema,
    pub body: T,
    pub signature: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum SignedSchema {
    #[serde(rename = "af/backlog-signed-submission@1")]
    Submission,
    #[serde(rename = "af/backlog-signed-receipt@1")]
    Receipt,
}

impl<T> Signed<T> {
    pub fn validate_signature(&self, schema: SignedSchema) -> Result<(), String> {
        if self.schema != schema {
            return Err("wrong signed backlog envelope schema".into());
        }
        if self.signature.is_empty() || self.signature.len() > 4096 {
            return Err("invalid backlog exchange signature size".into());
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum IntakeRecord {
    #[serde(rename = "af/BacklogAccepted@1")]
    Accepted {
        report: Box<Report>,
        receipt: Signed<Receipt>,
    },
    #[serde(rename = "af/BacklogAcknowledged@1")]
    Acknowledged { receipt: Signed<Receipt> },
}

impl IntakeRecord {
    pub fn validate(&self) -> Result<(), String> {
        let receipt = match self {
            Self::Accepted { report, receipt } => {
                report.validate()?;
                if receipt.body.project_id != report.project_id
                    || receipt.body.report_id != report.report_id
                {
                    return Err("backlog receipt does not identify its report".into());
                }
                receipt
            }
            Self::Acknowledged { receipt } => receipt,
        };
        receipt.validate_signature(SignedSchema::Receipt)?;
        receipt.body.validate()
    }
}
