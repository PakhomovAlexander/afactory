//! A Task's structured acceptance (ADR-0255): the items a Task file states the result must meet,
//! each under an ID the evaluator judges it by.
//!
//! A Task file's optional `acceptance` list is captured at plan time as one
//! `af/TaskAcceptance@1` record in the revision's provenance. The check node copies it into the
//! `af/TaskCheckReceipt@2` it records, and a Pipeline that declares the optional `acceptance`
//! root port hands the record to its evaluator as its own input. A passing evaluation that does
//! not name every item by its ID does not verify the Task. A Task file without the list records
//! nothing, and its revision, plan and receipts are what they were.

use serde::{Deserialize, Serialize};

use super::{is_name, require};

pub const TASK_ACCEPTANCE_V1: &str = "af/TaskAcceptance@1";
pub const TASK_ACCEPTANCE_SCHEMA: &str = "af.task-acceptance/1";
/// The root port a Pipeline declares to hand the record to its evaluator.
pub const ACCEPTANCE_PORT: &str = "acceptance";
/// The most items one Task file states.
pub const MAX_ACCEPTANCE_ITEMS: usize = 32;
/// The most characters of one item's text.
pub const MAX_ACCEPTANCE_TEXT: usize = 4096;

/// One acceptance item: the ID the evaluator names it by, and what the result must meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAcceptanceItemV1 {
    pub id: String,
    pub text: String,
}

impl TaskAcceptanceItemV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_name(&self.id) {
            return Err(format!(
                "acceptance item id {:?} is not a name: letters, digits, `_` and `-`, starting \
                 with a letter or digit, at most 128 bytes",
                self.id
            ));
        }
        require(
            !self.text.trim().is_empty()
                && self.text.chars().count() <= MAX_ACCEPTANCE_TEXT
                && !self
                    .text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "acceptance item text is nonblank, at most 4096 characters, without control \
             characters but newline and tab",
        )
    }
}

/// The items one Task states, in the order its Task file lists them, each ID once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAcceptanceV1 {
    pub schema: String,
    pub items: Vec<TaskAcceptanceItemV1>,
}

impl TaskAcceptanceV1 {
    pub fn new(items: Vec<TaskAcceptanceItemV1>) -> Result<Self, String> {
        let record = Self {
            schema: TASK_ACCEPTANCE_SCHEMA.into(),
            items,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_ACCEPTANCE_SCHEMA,
            "Task acceptance record has another schema",
        )?;
        validate_items(&self.items)
    }

    /// The IDs a passing evaluation's `reason` leaves unnamed, in item order.
    pub fn unjudged<'a>(&'a self, reason: &str) -> Vec<&'a str> {
        unjudged(&self.items, reason)
    }
}

/// One to [`MAX_ACCEPTANCE_ITEMS`] valid items, each ID once.
pub fn validate_items(items: &[TaskAcceptanceItemV1]) -> Result<(), String> {
    if items.is_empty() || items.len() > MAX_ACCEPTANCE_ITEMS {
        return Err(format!(
            "acceptance lists {} items; a Task states 1 to {MAX_ACCEPTANCE_ITEMS}",
            items.len()
        ));
    }
    for (index, item) in items.iter().enumerate() {
        item.validate()?;
        if items[..index].iter().any(|seen| seen.id == item.id) {
            return Err(format!("acceptance names item id {:?} twice", item.id));
        }
    }
    Ok(())
}

/// The IDs of `items` that `reason` does not name as a whole word: an ID counts as named where
/// it stands between characters that cannot be part of a name, so `a-1` names neither `a` nor
/// `a-10`.
pub fn unjudged<'a>(items: &'a [TaskAcceptanceItemV1], reason: &str) -> Vec<&'a str> {
    let words: std::collections::BTreeSet<&str> = reason
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .collect();
    items
        .iter()
        .map(|item| item.id.as_str())
        .filter(|id| !words.contains(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, text: &str) -> TaskAcceptanceItemV1 {
        TaskAcceptanceItemV1 {
            id: id.into(),
            text: text.into(),
        }
    }

    #[test]
    fn a_record_states_one_to_thirty_two_named_items_each_once() {
        let record = TaskAcceptanceV1::new(vec![
            item("bounds", "Negative offsets are refused"),
            item("empty-page", "An offset past the end returns no items"),
        ])
        .unwrap();
        assert_eq!(record.schema, TASK_ACCEPTANCE_SCHEMA);
        assert!(TaskAcceptanceV1::new(vec![]).is_err(), "never empty");
        let twice = TaskAcceptanceV1::new(vec![item("a", "x"), item("a", "y")]).unwrap_err();
        assert!(twice.contains("\"a\" twice"), "{twice}");
        let unnamed = TaskAcceptanceV1::new(vec![item("-a", "x")]).unwrap_err();
        assert!(unnamed.contains("\"-a\" is not a name"), "{unnamed}");
        assert!(TaskAcceptanceV1::new(vec![item("a", "  ")]).is_err());
        assert!(TaskAcceptanceV1::new(vec![item("a", "bell\u{7}")]).is_err());
        assert!(TaskAcceptanceV1::new(vec![item("a", "two\nlines\tok")]).is_ok());
        let many = (0..=MAX_ACCEPTANCE_ITEMS)
            .map(|n| item(&format!("i{n}"), "x"))
            .collect();
        assert!(TaskAcceptanceV1::new(many).is_err());
        let mut other = record;
        other.schema = "af.task-acceptance/2".into();
        assert!(other.validate().is_err());
    }

    #[test]
    fn an_item_is_judged_when_the_reason_names_its_id_as_a_whole_word() {
        let record =
            TaskAcceptanceV1::new(vec![item("a-1", "x"), item("a", "y"), item("b_2", "z")])
                .unwrap();
        assert!(
            record
                .unjudged("a-1: met. a: met (tests pass); b_2 met.")
                .is_empty()
        );
        assert_eq!(record.unjudged("a-10 met, b_2 met"), ["a-1", "a"]);
        assert_eq!(record.unjudged("all items met"), ["a-1", "a", "b_2"]);
    }
}
