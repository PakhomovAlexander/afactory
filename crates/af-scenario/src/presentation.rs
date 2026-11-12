//! Pure projection of named native text outputs. No dispatch or synthesized data.
use crate::{contract::*, state::Run};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub schema: String,
    pub observed_unix_ms: u64,
    pub stale_after_ms: u64,
    pub blocks: Vec<Block>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    /// Intentional Markdown, unlike escaped structured cells and labels.
    Markdown { content: String },
    Table {
        title: String,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
}
impl Document {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/result-document@1",
            "Unsupported result document version",
        )?;
        require(
            self.observed_unix_ms > 0
                && self.observed_unix_ms <= MAX_TIME
                && self.stale_after_ms > 0
                && self.stale_after_ms <= MAX_TIME
                && self.blocks.len() <= 32,
            "Invalid presentation time or block bound",
        )?;
        let wire = serde_json::to_value(self).map_err(|e| e.to_string())?;
        af_contracts::json::admit(&wire)
            .map_err(|e| format!("Unsupported presentation number: {e}"))?;
        af_store::canonicalize(&wire)
            .map_err(|e| format!("Unsupported presentation number: {e}"))?;
        require(
            serde_json::to_vec(self).map_err(|e| e.to_string())?.len() <= 65_536,
            "Presentation exceeds native text bound",
        )?;
        for block in &self.blocks {
            match block {
                Block::Markdown { content } => {
                    require(
                        !content
                            .chars()
                            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
                        "Markdown contains terminal control characters",
                    )?;
                }
                Block::Table {
                    title,
                    columns,
                    rows,
                } => {
                    text(title)?;
                    require(
                        !columns.is_empty() && columns.len() <= 32 && rows.len() <= 512,
                        "Unsupported table dimensions",
                    )?;
                    let mut seen = std::collections::BTreeSet::new();
                    for column in columns {
                        text(column)?;
                        require(seen.insert(column), "Duplicate table column")?;
                    }
                    require(
                        rows.iter().all(|row| row.len() == columns.len()),
                        "Table row does not match author columns",
                    )?;
                }
            }
        }
        Ok(())
    }
    pub fn empty(&self) -> bool {
        self.blocks.iter().all(|b| match b {
            Block::Markdown { content } => content.is_empty(),
            Block::Table { rows, .. } => rows.is_empty(),
        })
    }
}
/// Escapes HTML, Markdown syntax, controls and table separators. Nested values use canonical
/// JSON; no truncation or inferred summaries. Newlines are visible HTML line breaks.
pub fn cell(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' => out.push_str("<br>"),
            '\r' => out.push_str("&#13;"),
            c if c.is_control() => {
                let _ = write!(out, "&#{};", c as u32);
            }
            '\\' | '|' | '`' | '*' | '_' | '[' | ']' | '#' | '~' => {
                let _ = write!(out, "&#{};", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}
/// Transactionally derived read index, never an execution or accounting ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub ant: Option<String>,
    pub occurrence: Option<String>,
    pub run: Option<String>,
    pub active: bool,
    pub status: String,
    pub error: Option<String>,
    pub outputs: std::collections::BTreeMap<String, Option<Datum>>,
}
impl Recorded {
    pub fn from_run(run: &Run, occurrence: Option<String>) -> Self {
        let definition = &run.pin.bundle.definitions[&run.pin.bundle.root];
        Self {
            ant: run.ant.clone(),
            occurrence,
            run: Some(run.id.clone()),
            active: run.status.active(),
            status: format!("{:?}", run.status).to_lowercase(),
            error: run
                .failure
                .clone()
                .or_else(|| run.cancellation.as_ref().map(|c| c.reason.clone())),
            outputs: definition
                .result
                .iter()
                .map(|(name, binding)| (name.clone(), run.resolve("root", binding).ok()))
                .collect(),
        }
    }
    /// Explicit raw-text presentation. Observation time comes from a separate
    /// recorded integer output, never the poll clock or Run modification time.
    pub fn markdown_snapshot(
        &self,
        output: &str,
        observed_output: &str,
        stale_after_ms: u64,
        now: u64,
    ) -> Result<Snapshot> {
        require(
            output != observed_output,
            "Markdown and observation outputs must differ",
        )?;
        require(
            stale_after_ms > 0 && stale_after_ms <= MAX_TIME,
            "Invalid freshness bound",
        )?;
        let datum = self
            .outputs
            .get(output)
            .ok_or("Named output not declared by Scenario")?;
        let observed = self
            .outputs
            .get(observed_output)
            .ok_or("Observation output not declared by Scenario")?;
        let Some(datum) = datum else {
            return self.snapshot(output, now);
        };
        let Datum::Text(content) = datum else {
            return Err("Raw Markdown requires a recorded text output".into());
        };
        let Some(Datum::Integer(observed_unix_ms)) = observed else {
            return Err(
                "Raw Markdown requires an available recorded integer observation time".into(),
            );
        };
        let document = Document {
            schema: "af/result-document@1".into(),
            observed_unix_ms: *observed_unix_ms,
            stale_after_ms,
            blocks: vec![Block::Markdown {
                content: content.clone(),
            }],
        };
        document.validate()?;
        let mut projection = self.clone();
        projection.outputs.insert(
            output.into(),
            Some(Datum::Text(
                serde_json::to_string(&document).map_err(|e| e.to_string())?,
            )),
        );
        projection.snapshot(output, now)
    }
    pub fn snapshot(&self, output: &str, now: u64) -> Result<Snapshot> {
        let datum = self
            .outputs
            .get(output)
            .ok_or("Named output not declared by Scenario")?;
        let mut snapshot = Snapshot::unavailable(output, now, self.error.clone());
        snapshot.ant = self.ant.clone();
        snapshot.run = self.run.clone();
        snapshot.occurrence = self.occurrence.clone();
        snapshot.producer_status = Some(self.status.clone());
        let Some(datum) = datum else {
            if matches!(self.status.as_str(), "failed" | "cancelled") {
                snapshot.state = "error".into();
            }
            return Ok(snapshot);
        };
        let content = match datum {
            Datum::Text(content) => content,
            Datum::Observation(observation) => &observation.evidence,
            Datum::Action(receipt) => &receipt.evidence,
            _ => return Err("Markdown output requires native text, observation evidence or action evidence containing af/result-document@1".into()),
        };
        let document: Document = serde_json::from_str(content)
            .map_err(|e| format!("Unsupported presentation shape: {e}"))?;
        document.validate()?;
        require(
            document.observed_unix_ms <= now,
            "Output observation is in the future; clock or producer error",
        )?;
        snapshot.revision = Some(hash(&document)?);
        snapshot.observed_unix_ms = Some(document.observed_unix_ms);
        snapshot.freshness = if now - document.observed_unix_ms > document.stale_after_ms {
            "stale"
        } else {
            "fresh"
        }
        .into();
        snapshot.state = if matches!(self.status.as_str(), "failed" | "cancelled") {
            "error"
        } else if document.empty() {
            if self.active {
                "partial_empty"
            } else {
                "final_empty"
            }
        } else if self.active {
            "partial"
        } else {
            "final"
        }
        .into();
        snapshot.document = Some(document);
        Ok(snapshot)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: String,
    pub ant: Option<String>,
    pub occurrence: Option<String>,
    pub run: Option<String>,
    pub output: String,
    pub revision: Option<String>,
    pub observed_unix_ms: Option<u64>,
    pub rendered_unix_ms: u64,
    pub state: String,
    pub producer_status: Option<String>,
    pub freshness: String,
    pub error: Option<String>,
    pub document: Option<Document>,
}
impl Snapshot {
    pub fn unavailable(output: &str, now: u64, error: Option<String>) -> Self {
        Self {
            schema: "af/result-snapshot@1".into(),
            ant: None,
            occurrence: None,
            run: None,
            output: output.into(),
            revision: None,
            observed_unix_ms: None,
            rendered_unix_ms: now,
            state: "unavailable".into(),
            producer_status: None,
            freshness: "unavailable".into(),
            error,
            document: None,
        }
    }
    pub fn from_run(run: &Run, output: &str, now: u64) -> Result<Self> {
        Recorded::from_run(run, None).snapshot(output, now)
    }
    pub fn markdown(&self) -> Result<String> {
        if let Some(document) = &self.document {
            document.validate()?;
        }
        let mut out = format!(
            "<!-- af-result-snapshot begin {} -->\n\n",
            self.rendered_unix_ms
        );
        let _ = writeln!(
            out,
            "# Output {}\n\nState: {}; freshness: {}.\n",
            cell(&self.output),
            self.state,
            self.freshness
        );
        let _ = writeln!(
            out,
            "Producer status: {}.\n",
            cell(self.producer_status.as_deref().unwrap_or("unavailable"))
        );
        for (label, value) in [
            ("Ant", self.ant.as_deref()),
            ("Occurrence", self.occurrence.as_deref()),
            ("Run", self.run.as_deref()),
            ("Revision", self.revision.as_deref()),
        ] {
            let _ = writeln!(out, "{label}: {}  ", cell(value.unwrap_or("unavailable")));
        }
        let _ = writeln!(
            out,
            "Observed Unix ms: {}; rendered Unix ms: {}.\n",
            self.observed_unix_ms
                .map_or_else(|| "unavailable".into(), |t| t.to_string()),
            self.rendered_unix_ms
        );
        if let Some(error) = &self.error {
            let _ = writeln!(out, "Error: {}\n", cell(error));
        }
        if let Some(doc) = &self.document {
            for block in &doc.blocks {
                match block {
                    Block::Markdown { content } => {
                        let _ = writeln!(out, "{content}\n");
                    }
                    Block::Table {
                        title,
                        columns,
                        rows,
                    } => {
                        let _ = writeln!(out, "## {}\n", cell(title));
                        let _ = writeln!(
                            out,
                            "| {} |",
                            columns
                                .iter()
                                .map(|s| cell(s))
                                .collect::<Vec<_>>()
                                .join(" | ")
                        );
                        let _ = writeln!(out, "| {} |", vec!["---"; columns.len()].join(" | "));
                        for row in rows {
                            let _ = writeln!(
                                out,
                                "| {} |",
                                row.iter()
                                    .map(|v| cell(&match v {
                                        Value::String(s) => s.clone(),
                                        v => String::from_utf8(
                                            af_store::canonicalize(v)
                                                .expect("JSON value is canonicalizable")
                                        )
                                        .expect("canonical JSON is UTF-8"),
                                    }))
                                    .collect::<Vec<_>>()
                                    .join(" | ")
                            );
                        }
                        out.push('\n');
                    }
                }
            }
        } else {
            out.push_str("No recorded output available.\n\n");
        }
        let _ = writeln!(
            out,
            "<!-- af-result-snapshot end {} -->",
            self.rendered_unix_ms
        );
        Ok(out)
    }
}
/// One complete atomic document; temporary bytes are never visible under the destination.
pub fn atomic_write(path: &std::path::Path, markdown: &str) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(markdown.as_bytes())
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temp.persist(path).map_err(|e| e.to_string())?;
    std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{START, valid};
    use serde_json::json;
    fn record(blocks: Vec<Block>) -> Recorded {
        let doc = Document {
            schema: "af/result-document@1".into(),
            observed_unix_ms: START,
            stale_after_ms: 1000,
            blocks,
        };
        doc.validate().unwrap();
        valid("result-document-v1.json", &doc);
        Recorded {
            ant: Some("sensor".into()),
            occurrence: Some("occurrence".into()),
            run: Some("run".into()),
            active: true,
            status: "waiting".into(),
            error: None,
            outputs: std::collections::BTreeMap::from([
                (
                    "measurements".into(),
                    Some(Datum::Text(serde_json::to_string(&doc).unwrap())),
                ),
                ("pending".into(), None),
            ]),
        }
    }
    #[test]
    fn arbitrary_tables_prose_escaping_partial_final_stale_and_named_outputs() {
        let mut record = record(vec![
            Block::Markdown {
                content: "**Intentional** prose.".into(),
            },
            Block::Table {
                title: "Weather".into(),
                columns: vec!["Place".into(), "Reading".into()],
                rows: vec![vec![
                    json!("a|b\n<script>\u{1b}"),
                    json!({"z":[1,2],"a":true}),
                ]],
            },
            Block::Table {
                title: "Inventory".into(),
                columns: vec!["Book".into()],
                rows: vec![vec![json!("*literal*")]],
            },
        ]);
        let snapshot = record.snapshot("measurements", START).unwrap();
        valid("result-snapshot-v1.json", &snapshot);
        assert_eq!(snapshot.state, "partial");
        assert_eq!(snapshot.freshness, "fresh");
        let markdown = snapshot.markdown().unwrap();
        assert!(markdown.contains("**Intentional**"));
        assert!(markdown.contains("a&#124;b<br>&lt;script&gt;&#27;"));
        assert!(markdown.contains("&#42;literal&#42;"));
        assert!(!markdown.contains('\u{1b}'));
        assert_eq!(
            record.snapshot("pending", START).unwrap().state,
            "unavailable"
        );
        assert!(record.snapshot("missing", START).is_err());
        assert!(record.snapshot("measurements", START - 1).is_err());
        record.active = false;
        record.status = "succeeded".into();
        let final_view = record.snapshot("measurements", START + 1001).unwrap();
        assert_eq!(final_view.state, "final");
        assert_eq!(final_view.freshness, "stale");
        assert_eq!(snapshot.revision, final_view.revision);
        record.status = "failed".into();
        record.error = Some("Failed observation".into());
        assert_eq!(
            record.snapshot("measurements", START).unwrap().state,
            "error"
        );
    }
    #[test]
    fn explicitly_selected_raw_markdown_preserves_times_identity_and_controls() {
        let mut record = record(vec![]);
        let raw = "# Inventory\n\n| Book | Count |\n| --- | --- |\n| Rust | 2 |";
        record
            .outputs
            .insert("measurements".into(), Some(Datum::Text(raw.into())));
        record
            .outputs
            .insert("observed".into(), Some(Datum::Integer(START)));
        assert!(record.snapshot("measurements", START).is_err());
        let snapshot = record
            .markdown_snapshot("measurements", "observed", 1000, START)
            .unwrap();
        assert_eq!(snapshot.ant, record.ant);
        assert_eq!(snapshot.run, record.run);
        assert_eq!(snapshot.occurrence, record.occurrence);
        assert_eq!(snapshot.observed_unix_ms, Some(START));
        assert_eq!(snapshot.state, "partial");
        assert!(snapshot.markdown().unwrap().contains(raw));
        let stale = record
            .markdown_snapshot("measurements", "observed", 1000, START + 1001)
            .unwrap();
        assert_eq!(stale.freshness, "stale");
        assert_eq!(snapshot.revision, stale.revision);
        assert!(
            record
                .markdown_snapshot("measurements", "observed", 1000, START - 1)
                .is_err()
        );
        record.outputs.insert("observed".into(), None);
        assert!(
            record
                .markdown_snapshot("measurements", "observed", 1000, START)
                .is_err()
        );
        record
            .outputs
            .insert("observed".into(), Some(Datum::Integer(START)));
        record
            .outputs
            .insert("measurements".into(), Some(Datum::Text("\u{1b}[2J".into())));
        assert!(
            record
                .markdown_snapshot("measurements", "observed", 1000, START)
                .is_err()
        );
        record
            .outputs
            .insert("measurements".into(), Some(Datum::Text(String::new())));
        record.active = false;
        record.status = "succeeded".into();
        assert_eq!(
            record
                .markdown_snapshot("measurements", "observed", 1000, START)
                .unwrap()
                .state,
            "final_empty"
        );
    }
    #[test]
    fn empty_invalid_shapes_revision_changes_and_atomic_documents() {
        let mut record = record(vec![]);
        let partial = record.snapshot("measurements", START).unwrap();
        assert_eq!(partial.state, "partial_empty");
        record.active = false;
        record.status = "succeeded".into();
        assert_eq!(
            record.snapshot("measurements", START).unwrap().state,
            "final_empty"
        );
        let other = super::tests::record(vec![Block::Markdown {
            content: "changed".into(),
        }])
        .snapshot("measurements", START)
        .unwrap();
        assert_ne!(partial.revision, other.revision);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("latest.md");
        atomic_write(&path, &partial.markdown().unwrap()).unwrap();
        atomic_write(&path, &other.markdown().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            other.markdown().unwrap()
        );
        assert_eq!(temp.path().read_dir().unwrap().count(), 1);
        let mut doc = other.document.unwrap();
        doc.blocks = vec![Block::Table {
            title: "bad".into(),
            columns: vec!["one".into()],
            rows: vec![vec![]],
        }];
        assert!(doc.validate().is_err());
        doc.blocks = vec![Block::Markdown {
            content: "bad\u{1b}[2J".into(),
        }];
        assert!(doc.validate().is_err());
        doc.blocks = vec![Block::Table {
            title: "large".into(),
            columns: vec!["one".into()],
            rows: vec![vec![json!("x".repeat(65536))]],
        }];
        assert!(doc.validate().is_err());
        for number in [json!(9007199254740992_u64), json!(-0.0), json!(1e-9)] {
            doc.blocks = vec![Block::Table {
                title: "numeric boundary".into(),
                columns: vec!["value".into()],
                rows: vec![vec![number]],
            }];
            assert!(doc.validate().is_err());
            let mut snapshot = partial.clone();
            snapshot.document = Some(doc.clone());
            assert!(snapshot.markdown().is_err());
        }
    }
}
