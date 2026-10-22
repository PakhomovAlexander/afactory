//! Fixed Provider readiness context. No business inputs, package prompt or credentials.

/// The whole stdin of a Provider admission Attempt, and the only text every probe, context digest
/// and test reads: one line, one trailing newline, whose expected reply is exactly `OK`.
///
/// It says what it is for because a content-free instruction is scored on the CLI's static
/// preamble alone: run from the empty attempt directory, Fable 5.1's safety classifier refused
/// the bare instruction 14 times out of 14 and passed this text 6 times out of 6 (ADR-0147, "The
/// admission probe states its purpose").
pub const PROBE_INPUT: &[u8] = b"This is a Provider admission check by the af review tool: \
it only verifies that this account can run the bound model. No file needs to be read and no \
tool needs to be used. Reply with exactly: OK\n";

/// The admission probe sent once more, in other words, when the bound model's safety
/// classifier refused [`PROBE_INPUT`] itself (ADR-0196): a refusal of a 196-byte probe says
/// nothing about the account, so the Task is not lost to it. Its expected reply is `OK` too.
pub const PROBE_INPUT_ALTERNATE: &[u8] = b"Automated readiness check from af, a developer \
tool, before a coding task starts: confirm this session can answer. Nothing needs to be read, \
written or run. Answer with the single word: OK\n";

#[cfg(test)]
mod tests {
    use super::{PROBE_INPUT, PROBE_INPUT_ALTERNATE};

    #[test]
    fn the_alternate_probe_is_one_other_line_that_asks_for_ok() {
        let text = std::str::from_utf8(PROBE_INPUT_ALTERNATE).unwrap();
        assert!(text.is_ascii());
        assert_eq!(text.matches('\n').count(), 1);
        assert!(text.ends_with(": OK\n"));
        assert!(!text.contains("  "));
        assert_ne!(PROBE_INPUT_ALTERNATE, PROBE_INPUT);
    }

    #[test]
    fn the_probe_is_one_line_that_states_its_purpose_and_asks_for_ok() {
        let text = std::str::from_utf8(PROBE_INPUT).unwrap();
        assert!(text.is_ascii());
        assert_eq!(text.matches('\n').count(), 1);
        assert!(text.ends_with(": OK\n"));
        assert!(!text.contains("  "));
        assert!(text.starts_with("This is a Provider admission check by the af review tool: "));
        assert!(text.contains(" No file needs to be read and no tool needs to be used. "));
        assert_eq!(PROBE_INPUT.len(), 196);
    }

    /// The measured text, byte for byte: any edit changes its CAS blob identity.
    #[test]
    fn the_probe_is_the_measured_text_byte_for_byte() {
        let temp = tempfile::tempdir().unwrap();
        let cas = review_store::Cas::open(temp.path().join("cas")).unwrap();
        assert_eq!(
            cas.put(PROBE_INPUT).unwrap(),
            "sha256:9e72a17bdabb9e03d0fc3be5a8d8379ae558d197004a88f8d8c73e507d8fedd4"
        );
    }
}
