# Record rename truncation and continue the diff Subject

**Status:** accepted (2026-09-23); acceptance recorded in
[ADR-0113](0113-ga-reads-only-what-ga-writes.md)

Git bounds exhaustive rename detection because its unmatched add/delete search is quadratic. When
the fixed limit is exceeded, Git still returns a complete patch and complete Add/Delete records,
but it omits uncertain rename linkage and emits a warning on stderr. Silently discarding that
warning makes an empty rename map ambiguous. Aborting capture instead makes large directory moves
and dependency refreshes unreviewable even though their path set and patch remain authoritative.

We decided to continue constructing the diff Subject and record the degraded linkage explicitly.
`TreeDiff` and `ChangeSet@1` carry `rename_detection_truncated`; the reviewer prompt renders it,
and the diff-policy identity records the fixed candidate limit. `changed_paths` still contains
every Add/Delete endpoint, so Report Scope remains complete. Consumers may not interpret an empty
rename map as proof that no rename occurred when the flag is true.

## Considered options

- **Discard Git's successful-exit warning.** This preserves the smallest artifact but makes a
  genuinely rename-free diff indistinguishable from a truncated search. Rejected because replay
  and downstream grouping would silently trust metadata Git explicitly said was incomplete.
- **Abort the whole diff Subject.** This fails closed and avoids changing the artifact. Rejected
  because the missing fact is only linkage: the exact patch and both path endpoints are still
  complete, while Subject partitioning does not exist until M8. An operator would have no way to
  recover from the error.
- **Raise or remove the limit.** Rejected because exhaustive rename detection has quadratic cost
  and candidate-controlled changes could turn capture into unbounded work.
- **Record truncation and continue (chosen).** Preserves bounded capture and complete Scope while
  making the metadata gap durable and visible.

## Consequences

- The diff-policy version includes `rename-limit=1000`; changing the limit or its semantics bumps
  that policy identity.
- `rename_detection_truncated=true` means the patch and `changed_paths` are authoritative but the
  `renames` collection may be incomplete.
- Reviewers and future grouping logic must use the flag before drawing conclusions from an empty
  rename map.
- The kernel keeps reviewing large Subjects before M8 partitioning exists, at the accepted cost
  of less precise rename linkage for those Subjects.
