//! SARIF 2.1.0 export of review findings (ADR-0248): `af task findings --format sarif` and `af
//! review report --format sarif`.
//!
//! One SARIF log with one run whose tool driver is af. Each finding is one result: its rule is
//! the reviewer that reported it (`af-review/<reviewer>`), its level its severity, its message
//! the agent prompt, its location the finding's file and line (a change-wide finding has a
//! logical location instead), and its properties the severity, status, prompt and verified fix.
//! What the run covered travels as an invocation whose `executionSuccessful` is false when a
//! reviewer did not run, with one notification per such reviewer, and in the run's properties.
//! The run's `properties.schema` names the profile, `af/findings-sarif@1`
//! (`schemas/findings-sarif-v1.json`).

use review_core::Severity;
use review_core::task::handoff::{CoverageV1, FindingPromptV1, VerifiedFixV1};
use serde_json::{Map, Value, json};

pub(crate) const SARIF_VERSION: &str = "2.1.0";
/// The OASIS SARIF 2.1.0 schema (errata 01), named by every log af prints.
pub(crate) const SARIF_SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";
/// af's profile of a SARIF log, in `runs[0].properties.schema`.
pub(crate) const FINDINGS_SARIF_V1: &str = "af/findings-sarif@1";
/// The rule id prefix: a result's rule is the reviewer that reported it.
const RULE_PREFIX: &str = "af-review/";

/// One finding as SARIF reads it.
pub(crate) struct SarifFinding<'a> {
    pub(crate) reviewer: &'a str,
    pub(crate) severity: Severity,
    pub(crate) title: &'a str,
    pub(crate) finding_id: Option<&'a str>,
    pub(crate) status: Option<&'a str>,
    pub(crate) prompt: &'a FindingPromptV1,
    /// A Resolution an operator or reviewer recorded declining the finding (rejected or
    /// wontfix): shown as an accepted external suppression.
    pub(crate) declined: bool,
}

/// What one SARIF run reports: the Task or Campaign, its round, coverage and findings.
pub(crate) struct SarifRun<'a> {
    /// `("task_id", ID)` or `("campaign", LABEL)`.
    pub(crate) owner: (&'static str, &'a str),
    pub(crate) round: Option<u64>,
    pub(crate) snapshot_id: Option<&'a str>,
    /// `None` when nothing was reviewed: no round, or no run, recorded.
    pub(crate) coverage: Option<&'a CoverageV1>,
    pub(crate) verified_fixed: &'a [VerifiedFixV1],
    pub(crate) findings: Vec<SarifFinding<'a>>,
}

fn level(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocker => "error",
        Severity::Major => "warning",
        Severity::Minor => "note",
    }
}

fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocker => "blocker",
        Severity::Major => "major",
        Severity::Minor => "minor",
    }
}

/// A repository-relative path as a URI reference: every byte outside the unreserved set and
/// `/` percent-encoded.
fn uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn location(prompt: &FindingPromptV1) -> Value {
    match &prompt.location.file {
        Some(file) => {
            let mut physical = Map::new();
            physical.insert("artifactLocation".into(), json!({"uri": uri(file)}));
            if let Some(line) = prompt.location.line {
                physical.insert("region".into(), json!({"startLine": line}));
            }
            json!({"physicalLocation": physical})
        }
        None => json!({
            "logicalLocations": [{"name": "change-wide", "kind": "module"}]
        }),
    }
}

/// The SARIF 2.1.0 log of `run`.
pub(crate) fn document(run: &SarifRun<'_>) -> Value {
    let mut reviewers: Vec<&str> = run.findings.iter().map(|f| f.reviewer).collect();
    if let Some(coverage) = run.coverage {
        reviewers.extend(coverage.reviewers.iter().map(|r| r.reviewer.as_str()));
    }
    reviewers.sort_unstable();
    reviewers.dedup();
    let rules: Vec<Value> = reviewers
        .iter()
        .map(|reviewer| {
            json!({
                "id": format!("{RULE_PREFIX}{reviewer}"),
                "name": reviewer,
                "shortDescription": {"text": format!("Findings of the af reviewer {reviewer}")},
            })
        })
        .collect();
    let results: Vec<Value> = run
        .findings
        .iter()
        .map(|finding| {
            let mut result = Map::new();
            result.insert(
                "ruleId".into(),
                json!(format!("{RULE_PREFIX}{}", finding.reviewer)),
            );
            result.insert(
                "ruleIndex".into(),
                json!(reviewers.binary_search(&finding.reviewer).unwrap_or(0)),
            );
            result.insert("level".into(), json!(level(finding.severity)));
            result.insert(
                "message".into(),
                json!({"text": finding.prompt.text.clone()}),
            );
            result.insert("locations".into(), json!([location(finding.prompt)]));
            if let Some(id) = finding.finding_id {
                result.insert("partialFingerprints".into(), json!({"afFindingId/v1": id}));
            }
            if finding.declined {
                result.insert(
                    "suppressions".into(),
                    json!([{"kind": "external", "status": "accepted",
                            "justification": format!("declined: {}",
                                finding.status.unwrap_or("declined"))}]),
                );
            }
            result.insert(
                "properties".into(),
                json!({
                    "title": match finding.title.trim() {
                        "" => "(untitled)",
                        title => title,
                    },
                    "severity": severity_word(finding.severity),
                    "reviewer": finding.reviewer,
                    "status": finding.status,
                    "agent_prompt": finding.prompt,
                }),
            );
            Value::Object(result)
        })
        .collect();
    let notifications: Vec<Value> = match run.coverage {
        Some(coverage) => coverage
            .not_run()
            .map(|reviewer| {
                json!({
                    "level": "error",
                    "message": {"text": format!(
                        "The reviewer {} did not run, so no finding from it means not reviewed: {}",
                        reviewer.reviewer,
                        reviewer.reason.as_deref().unwrap_or("no result recorded")
                    )},
                })
            })
            .collect(),
        None => vec![json!({
            "level": "error",
            "message": {"text": "No review round was recorded, so nothing was reviewed."},
        })],
    };
    let mut properties = Map::new();
    properties.insert("schema".into(), json!(FINDINGS_SARIF_V1));
    properties.insert(run.owner.0.into(), json!(run.owner.1));
    properties.insert("round".into(), json!(run.round));
    properties.insert("snapshot_id".into(), json!(run.snapshot_id));
    properties.insert("coverage".into(), json!(run.coverage));
    properties.insert("verified_fixed".into(), json!(run.verified_fixed));
    json!({
        "$schema": SARIF_SCHEMA,
        "version": SARIF_VERSION,
        "runs": [{
            "tool": {"driver": {
                "name": "af",
                "version": env!("CARGO_PKG_VERSION"),
                "informationUri": "https://github.com/PakhomovAlexander/afactory",
                "rules": rules,
            }},
            "invocations": [{
                "executionSuccessful": run.coverage.is_some_and(CoverageV1::complete),
                "toolExecutionNotifications": notifications,
            }],
            "results": results,
            "properties": properties,
        }],
    })
}

/// Print `run` as one SARIF log on stdout, refusing a log that breaks the SARIF 2.1.0 structure.
pub(crate) fn print(run: &SarifRun<'_>) -> Result<(), String> {
    let log = document(run);
    check(&log)?;
    println!(
        "{}",
        serde_json::to_string(&log).map_err(|error| error.to_string())?
    );
    Ok(())
}

/// The required structure of a SARIF 2.1.0 log, as the OASIS schema states it for what af
/// writes: the `version` and `$schema`; each run's `tool.driver` with its `name` and unique
/// rule ids; each result's `ruleId` (a rule the driver declares, at its `ruleIndex`),
/// `message.text`, `locations` (a physical location with a URI and a `startLine` of at least 1,
/// or a logical one) and `level`; each invocation's `executionSuccessful`; and the af profile in
/// the run's properties. Vendoring the official schema is a follow-up (ADR-0248).
pub(crate) fn check(log: &Value) -> Result<(), String> {
    let fail = |what: &str| Err(format!("not a SARIF 2.1.0 log: {what}"));
    if log["version"] != SARIF_VERSION {
        return fail("`version` is not 2.1.0");
    }
    if log["$schema"] != SARIF_SCHEMA {
        return fail("`$schema` is not the SARIF 2.1.0 schema");
    }
    let Some(runs) = log["runs"].as_array() else {
        return fail("`runs` is not an array");
    };
    for run in runs {
        let driver = &run["tool"]["driver"];
        if driver["name"].as_str().is_none_or(str::is_empty) {
            return fail("a run's `tool.driver.name` is missing");
        }
        let rules: Vec<&str> = match driver.get("rules") {
            None => Vec::new(),
            Some(rules) => match rules.as_array() {
                Some(rules) => rules.iter().filter_map(|r| r["id"].as_str()).collect(),
                None => return fail("`tool.driver.rules` is not an array"),
            },
        };
        let mut unique = rules.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != rules.len()
            || driver
                .get("rules")
                .and_then(Value::as_array)
                .is_some_and(|all| all.len() != rules.len())
        {
            return fail("a rule has no id, or two rules share one");
        }
        if run["properties"]["schema"] != FINDINGS_SARIF_V1 {
            return fail("the run's `properties.schema` is not af/findings-sarif@1");
        }
        for invocation in run["invocations"].as_array().into_iter().flatten() {
            if !invocation["executionSuccessful"].is_boolean() {
                return fail("an invocation has no `executionSuccessful`");
            }
            for note in invocation["toolExecutionNotifications"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if !note["message"]["text"].is_string() {
                    return fail("a notification has no `message.text`");
                }
            }
        }
        let Some(results) = run["results"].as_array() else {
            return fail("a run's `results` is not an array");
        };
        for result in results {
            let Some(rule) = result["ruleId"].as_str() else {
                return fail("a result has no `ruleId`");
            };
            let index = result["ruleIndex"]
                .as_u64()
                .and_then(|i| usize::try_from(i).ok());
            if index.and_then(|i| rules.get(i)) != Some(&rule) {
                return fail("a result's `ruleIndex` does not name its `ruleId`");
            }
            if !matches!(
                result["level"].as_str(),
                Some("none" | "note" | "warning" | "error")
            ) {
                return fail("a result's `level` is not none, note, warning or error");
            }
            if result["message"]["text"].as_str().is_none_or(str::is_empty) {
                return fail("a result has no `message.text`");
            }
            let Some(locations) = result["locations"].as_array() else {
                return fail("a result has no `locations` array");
            };
            for location in locations {
                let physical = &location["physicalLocation"];
                if physical.is_null() {
                    if location["logicalLocations"]
                        .as_array()
                        .is_none_or(Vec::is_empty)
                    {
                        return fail("a location has neither a physical nor a logical location");
                    }
                    continue;
                }
                if physical["artifactLocation"]["uri"]
                    .as_str()
                    .is_none_or(str::is_empty)
                {
                    return fail("a physical location has no `artifactLocation.uri`");
                }
                if let Some(region) = physical.get("region")
                    && region["startLine"].as_u64().is_none_or(|line| line < 1)
                {
                    return fail("a region's `startLine` is not at least 1");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::handoff::{self, PromptSource, VerifiedBy};
    use std::collections::BTreeMap;

    fn id(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    /// The published profile schema, `schemas/findings-sarif-v1.json`.
    fn profile() -> jsonschema::Validator {
        let schema: Value =
            serde_json::from_str(include_str!("../../../schemas/findings-sarif-v1.json")).unwrap();
        jsonschema::validator_for(&schema).unwrap()
    }

    fn prompt(file: Option<&str>, line: Option<i64>, snapshot: &str) -> FindingPromptV1 {
        handoff::prompt(
            &PromptSource {
                reviewer: "bugs",
                severity: Severity::Major,
                title: "Reject negative offset",
                file,
                line,
                snapshot_id: snapshot,
                evidence: "A negative offset slices from the end.",
                fix: Some("Raise ValueError."),
            },
            VerifiedBy::Task,
        )
    }

    /// Acceptance: the SARIF output has the SARIF 2.1.0 required structure (`version`,
    /// `$schema`, `runs[].tool.driver`, `results[].ruleId`, `message`, `locations`, `level`),
    /// and the published profile schema accepts it.
    #[test]
    fn a_sarif_log_has_the_sarif_2_1_0_required_structure() {
        let snapshot = id('a');
        let located = prompt(Some("src/a b.rs"), Some(3), &snapshot);
        let wide = prompt(None, None, &snapshot);
        let coverage = handoff::coverage(
            Some(&["src/a b.rs".to_owned()]),
            BTreeMap::from([
                ("bugs".to_owned(), None),
                (
                    "correctness".to_owned(),
                    Some("its Attempt failed".to_owned()),
                ),
            ]),
            ["src/a b.rs"],
        );
        let fixed = [VerifiedFixV1 {
            finding_id: id('f'),
            record_id: id('b'),
            snapshot_id: snapshot.clone(),
        }];
        let log = document(&SarifRun {
            owner: ("task_id", "chain-base"),
            round: Some(1),
            snapshot_id: Some(&snapshot),
            coverage: Some(&coverage),
            verified_fixed: &fixed,
            findings: vec![
                SarifFinding {
                    reviewer: "bugs",
                    severity: Severity::Major,
                    title: "Reject negative offset",
                    finding_id: Some(&snapshot),
                    status: Some("open"),
                    prompt: &located,
                    declined: false,
                },
                SarifFinding {
                    reviewer: "bugs",
                    severity: Severity::Blocker,
                    title: "Change-wide",
                    finding_id: None,
                    status: Some("rejected"),
                    prompt: &wide,
                    declined: true,
                },
            ],
        });
        check(&log).unwrap();
        let validator = profile();
        assert!(
            validator.is_valid(&log),
            "{:?}",
            validator.iter_errors(&log).collect::<Vec<_>>()
        );
        assert_eq!(log["version"], "2.1.0");
        let run = &log["runs"][0];
        assert_eq!(run["tool"]["driver"]["name"], "af");
        let results = run["results"].as_array().unwrap();
        assert_eq!(results[0]["ruleId"], "af-review/bugs");
        assert_eq!(results[0]["level"], "warning");
        assert_eq!(results[1]["level"], "error");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "src/a%20b.rs"
        );
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            3
        );
        assert!(results[1]["locations"][0]["logicalLocations"].is_array());
        assert_eq!(results[1]["suppressions"][0]["status"], "accepted");
        // The reviewer that did not run is a failed invocation, never a clean one.
        let invocation = &run["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], false);
        let note = invocation["toolExecutionNotifications"][0]["message"]["text"]
            .as_str()
            .unwrap();
        assert!(note.contains("correctness did not run"), "{note}");
        assert_eq!(
            run["properties"]["verified_fixed"][0]["finding_id"],
            id('f')
        );

        // Nothing reviewed is said as such, and the log is still well formed.
        let none = document(&SarifRun {
            owner: ("campaign", "pr-42"),
            round: None,
            snapshot_id: None,
            coverage: None,
            verified_fixed: &[],
            findings: vec![],
        });
        check(&none).unwrap();
        assert!(validator.is_valid(&none));
        assert_eq!(
            none["runs"][0]["invocations"][0]["executionSuccessful"],
            false
        );
    }

    /// The structural check refuses each required part missing or wrong, so it cannot pass a
    /// log that is not SARIF 2.1.0.
    #[test]
    fn the_structural_check_refuses_a_log_missing_a_required_part() {
        let snapshot = id('a');
        let located = prompt(Some("a.rs"), Some(1), &snapshot);
        let log = document(&SarifRun {
            owner: ("task_id", "t"),
            round: Some(1),
            snapshot_id: Some(&snapshot),
            coverage: None,
            verified_fixed: &[],
            findings: vec![SarifFinding {
                reviewer: "bugs",
                severity: Severity::Minor,
                title: "t",
                finding_id: None,
                status: None,
                prompt: &located,
                declined: false,
            }],
        });
        check(&log).unwrap();
        let validator = profile();
        type Break = fn(&mut Value);
        let broken: [(&str, Break); 9] = [
            ("version", |v| v["version"] = json!("2.0.0")),
            ("$schema", |v| {
                v.as_object_mut().unwrap().remove("$schema");
            }),
            ("driver", |v| {
                v["runs"][0]["tool"]
                    .as_object_mut()
                    .unwrap()
                    .remove("driver");
            }),
            ("ruleId", |v| {
                v["runs"][0]["results"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("ruleId");
            }),
            ("message", |v| {
                v["runs"][0]["results"][0]["message"] = json!({});
            }),
            ("locations", |v| {
                v["runs"][0]["results"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("locations");
            }),
            ("level", |v| {
                v["runs"][0]["results"][0]["level"] = json!("fatal")
            }),
            ("startLine", |v| {
                v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"] =
                    json!(0);
            }),
            ("profile", |v| {
                v["runs"][0]["properties"]["schema"] = json!("af/findings-sarif@2");
            }),
        ];
        for (part, breaks) in broken {
            let mut bad = log.clone();
            breaks(&mut bad);
            assert!(check(&bad).is_err(), "{part} accepted by the check");
            assert!(!validator.is_valid(&bad), "{part} accepted by the schema");
        }
    }
}
