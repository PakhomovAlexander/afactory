//! The credential boundary one Worker declares.

use serde::{Deserialize, Serialize};

/// How one reviewer obtains external capability. Formats before pipeline v4 have no such claim.
/// `trusted_unsafe` Workers may hold ambient Provider credentials; `credential_free` ones hold
/// none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialModeV1 {
    CredentialFree,
    TrustedUnsafe,
}
