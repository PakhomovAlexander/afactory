//! The kind of immutable Subject a review judges.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubjectKind {
    Diff,
    WholeTree,
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            SubjectKind::Diff => "diff",
            SubjectKind::WholeTree => "whole-tree",
        })
    }
}
