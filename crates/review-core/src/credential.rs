//! The credential boundary one Worker declares.

use serde::{Deserialize, Serialize};

/// Whether one reviewer can read reusable Provider credentials. Formats before pipeline v4 make
/// no such claim. A `trusted_unsafe` Worker may hold ambient Provider credentials; a
/// `credential_free` one holds none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialModeV1 {
    CredentialFree,
    TrustedUnsafe,
}
