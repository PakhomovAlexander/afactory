//! `af schema show`: the JSON Schemas this binary ships, by the document ids `af help json`
//! lists (ADR-0247).
//!
//! Each schema is the file under `schemas/` compiled into the binary, so `af schema show ID`
//! prints exactly the bytes the repository publishes for the documents this release prints. A
//! unit test holds this table to the ids the `json` topic names, both ways, and every schema to
//! the `schema` constant of its id.

use clap_complete::engine::CompletionCandidate;

/// `af/<kind>@<version>`, `schemas/<kind>-v<version>.json` and that file's bytes.
macro_rules! schema {
    ($kind:literal, $version:literal) => {
        (
            concat!("af/", $kind, "@", $version),
            concat!("schemas/", $kind, "-v", $version, ".json"),
            include_str!(concat!("../../../schemas/", $kind, "-v", $version, ".json")),
        )
    };
}

/// Every shipped schema: its document id (the `schema` field the document carries), its file
/// under `schemas/`, and its bytes.
pub(crate) const SCHEMAS: &[(&str, &str, &str)] = &[
    schema!("command-catalog", 1),
    schema!("error", 2),
    schema!("findings-sarif", 1),
    schema!("provider-detect", 4),
    schema!("provider-models", 1),
    schema!("provider-setup", 2),
    schema!("provider-status", 4),
    schema!("review-ledger", 1),
    schema!("review-render", 2),
    schema!("review-report", 5),
    schema!("review-show", 1),
    schema!("task-attention", 1),
    schema!("task-cancelled", 1),
    schema!("task-checks", 2),
    schema!("task-collected-inspection", 2),
    schema!("task-detached", 2),
    schema!("task-diff", 1),
    schema!("task-event", 1),
    schema!("task-findings", 2),
    schema!("task-inspection", 13),
    schema!("task-list-entry", 3),
    schema!("task-list", 3),
    schema!("task-materialized", 1),
    schema!("task-report", 2),
    schema!("task-selection", 3),
    schema!("task-status", 2),
];

/// `af schema show [ID]`: the schema `id` names, byte for byte, or without an id every id with
/// its schema's title. An id this release does not ship is refused (exit 1), naming the
/// versions of the same document it does ship.
pub(crate) fn show(id: Option<&str>) -> Result<i32, String> {
    let Some(id) = id else {
        print!("{}", listing());
        return Ok(0);
    };
    let (_, _, text) = SCHEMAS
        .iter()
        .find(|(shipped, _, _)| *shipped == id)
        .ok_or_else(|| refusal(id))?;
    print!("{text}");
    Ok(0)
}

/// One line per shipped schema: its id, then its title.
fn listing() -> String {
    let width = SCHEMAS.iter().map(|(id, _, _)| id.len()).max().unwrap_or(0);
    SCHEMAS
        .iter()
        .map(|(id, _, text)| format!("{id:<width$}  {}\n", title(text)))
        .collect()
}

/// The `title` of a shipped schema.
fn title(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|schema| schema["title"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn refusal(id: &str) -> String {
    let kind = id.split_once('@').map_or(id, |(kind, _)| kind);
    let same: Vec<&str> = SCHEMAS
        .iter()
        .map(|(shipped, _, _)| *shipped)
        .filter(|shipped| shipped.split_once('@').map(|(kind, _)| kind) == Some(kind))
        .collect();
    let id = crate::terminal_text(id);
    if same.is_empty() {
        format!("this af ships no schema {id}; `af schema show` lists the ids it ships")
    } else {
        format!(
            "this af ships no schema {id}, only {}; run af schema show {}",
            same.join(", "),
            same[0]
        )
    }
}

/// Completion candidates for `af schema show ID`: the shipped ids.
pub(crate) fn complete_id(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    SCHEMAS
        .iter()
        .map(|(id, _, _)| *id)
        .filter(|id| id.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `af/NAME@N` the `json` topic names, in order of first mention.
    fn listed_in_the_json_topic() -> Vec<String> {
        let (_, _, text) = crate::topics::find("json").expect("json topic");
        let mut ids = Vec::new();
        for (start, _) in text.match_indices("af/") {
            let rest = &text[start + 3..];
            let kind: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
                .collect();
            let Some(version) = rest[kind.len()..].strip_prefix('@') else {
                continue;
            };
            let version: String = version.chars().take_while(char::is_ascii_digit).collect();
            let id = format!("af/{kind}@{version}");
            if !kind.is_empty() && !version.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    #[test]
    fn the_shipped_schemas_are_exactly_the_documents_af_help_json_lists() {
        let mut listed = listed_in_the_json_topic();
        listed.sort();
        let mut shipped: Vec<String> = SCHEMAS.iter().map(|(id, _, _)| id.to_string()).collect();
        let sorted = {
            let mut sorted = shipped.clone();
            sorted.sort();
            sorted
        };
        assert_eq!(shipped, sorted, "keep the table sorted by id");
        shipped.dedup();
        assert_eq!(shipped.len(), SCHEMAS.len(), "an id is shipped twice");
        assert_eq!(listed, shipped, "af help json and af schema show differ");
    }

    #[test]
    fn every_shipped_schema_describes_the_document_of_its_id() {
        for (id, file, text) in SCHEMAS {
            let schema: serde_json::Value =
                serde_json::from_str(text).unwrap_or_else(|error| panic!("{file}: {error}"));
            // A SARIF log names its af profile in its run's properties (ADR-0248).
            let constant = match schema["properties"]["schema"]["const"].as_str() {
                Some(_) => &schema["properties"]["schema"]["const"],
                None => {
                    &schema["$defs"]["run"]["properties"]["properties"]["properties"]["schema"]["const"]
                }
            };
            assert_eq!(constant, *id, "{file}");
            assert!(!title(text).is_empty(), "{file} has no title");
        }
    }

    #[test]
    fn an_unshipped_id_names_the_versions_that_are_shipped() {
        assert_eq!(
            refusal("af/task-status@1"),
            "this af ships no schema af/task-status@1, only af/task-status@2; run af schema \
             show af/task-status@2"
        );
        assert_eq!(
            refusal("task-status-v2.json"),
            "this af ships no schema task-status-v2.json; `af schema show` lists the ids it ships"
        );
        let listing = listing();
        assert_eq!(listing.lines().count(), SCHEMAS.len());
        assert!(listing.starts_with("af/command-catalog@1  "), "{listing}");
    }
}
