//! What the CLI says about authority paths the declared `.af/` layout does not name.
//!
//! The classification itself is [`review_config::layout::classify_manifest`], a pure function of
//! a captured Snapshot manifest and the one declared table: it reads recorded entries, never a
//! working tree and never a sandbox. This module owns only the sentences — one advisory line at
//! plan time, and the refusal a project asked for with `[delivery] undeclared_af_paths =
//! "refuse"`.
//!
//! Neither sentence removes anything. No path is dropped from a Snapshot, from a candidate tree
//! or from a delivered worktree; under `refuse` the delivery simply does not happen.

use review_config::layout::PathGroup;

/// How many paths the advisory line names before it counts the rest. The advisory is one line,
/// so it says enough to recognize the problem and leaves the whole list to the `--json` document
/// and to the delivery receipt.
const SHOWN: usize = 10;

/// Where everything the kernel records belongs instead.
const STORE: &str = "$XDG_STATE_HOME/af";

/// One advisory line, or `None` when the table declares every `.af/` path the Snapshot carries.
pub(crate) fn advisory(group: &PathGroup) -> Option<String> {
    if group.is_empty() {
        return None;
    }
    let count = group.count();
    let bytes = group.bytes;
    let paths = listed(group, SHOWN);
    let message = format!(
        "the captured source Snapshot carries {count} undeclared path(s) under .af/ ({bytes} \
         bytes): {paths}. Git holds declarations only; everything af records belongs in the Store \
         under {STORE}. This is advice, not a refusal."
    );
    Some(message)
}

/// Print the advisory on stderr. Advisory only: stdout, the exit code and the Snapshot are
/// exactly what they were, and the `--json` document gains one typed field beside them.
pub(crate) fn warn(group: &PathGroup) {
    if let Some(advice) = advisory(group) {
        eprintln!("warning: {advice}");
    }
}

/// Why delivery stopped under `refuse`. Names every undeclared path, because the operator is
/// being asked to deal with each of them.
pub(crate) fn refusal(group: &PathGroup) -> String {
    let count = group.count();
    let bytes = group.bytes;
    let paths = listed(group, usize::MAX);
    format!(
        "delivery refused: this Task's source Snapshot carries {count} path(s) under .af/ that \
         the declared layout does not name ({bytes} bytes), and the project policy it captured \
         is [delivery] undeclared_af_paths = \"refuse\": {paths}. Nothing was created: no branch, \
         no worktree, no prepared delivery record. Remove those paths from the repository, or set \
         the policy to \"warn\", and plan a new Task on the resulting Snapshot."
    )
}

/// Up to `limit` paths in manifest order, then how many were left unnamed.
fn listed(group: &PathGroup, limit: usize) -> String {
    let shown: Vec<&str> = group.paths.iter().take(limit).map(String::as_str).collect();
    let rest = group.count().saturating_sub(shown.len());
    let mut text = shown.join(", ");
    if rest > 0 {
        text.push_str(&format!(", and {rest} more"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(count: usize, bytes: u64) -> PathGroup {
        let paths = (0..count).map(|n| format!(".af/tasks/{n}")).collect();
        PathGroup { paths, bytes }
    }

    #[test]
    fn nothing_undeclared_says_nothing() {
        assert_eq!(advisory(&PathGroup::default()), None);
    }

    #[test]
    fn the_advisory_is_one_line_with_the_count_the_bytes_and_ten_paths() {
        let advice = advisory(&group(12, 4096)).expect("no advice given");
        assert_eq!(advice.lines().count(), 1, "{advice}");
        assert!(advice.contains("12 undeclared path(s)"), "{advice}");
        assert!(advice.contains("4096 bytes"), "{advice}");
        assert!(advice.contains(".af/tasks/9"), "{advice}");
        assert!(!advice.contains(".af/tasks/10"), "{advice}");
        assert!(advice.contains(", and 2 more"), "{advice}");
        assert!(advice.contains(STORE), "{advice}");
        assert!(advice.contains("not a refusal"), "{advice}");
    }

    #[test]
    fn the_refusal_names_every_path_and_says_nothing_was_created() {
        let reason = refusal(&group(12, 4096));
        for index in 0..12 {
            assert!(reason.contains(&format!(".af/tasks/{index}")), "{reason}");
        }
        let policy = "undeclared_af_paths = \"refuse\"";
        assert!(!reason.contains(", and 2 more"), "{reason}");
        assert!(reason.contains("no prepared delivery record"), "{reason}");
        assert!(reason.contains(policy), "{reason}");
    }
}
