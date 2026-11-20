# Verify the frozen Project backlog assessment

Independently inspect the captured source Snapshot, frozen batch, and checked Document.
Verify every material claim against the cited source and check that it still persists.
Verify a requested capability gap against the available behavior and its cited need.
Check membership, evidence IDs, and current source. For each member of a verified or already
tracked group, verify at least one accepted evidence digest against a cited regular file of at
most 65,536 bytes. The identity is `sha256:` followed by SHA-256 over the bytes
`review.kernel/content-id/v1\0` (with a zero byte terminator) followed by the complete file. Distinguish exact defect or tracked-item
identity from lexical similarity. Verify each proposed public title and body for evidence,
accuracy and privacy. Reject any inclusion of private text, internal Report identifiers or
evidence IDs in those fields. Insufficient evidence supports a needs evidence disposition.
A sensitive claim supports a held disposition and must have no public fields.

Return the native Report Evaluation. Use passed only if the exact checked Document supports
all of its dispositions and any proposed public fields, with every frozen member accounted
for. Record concrete disagreements on failure. This evaluation decides whether the private
assessment is sound. It grants no authority to publish.

Review the Decisions section in the checked Document. The renderer escapes Markdown in this
section. The JSON proposal names the exact batch and source and carries a reason for every
disposition. Reject proposals whose public fields make claims that exceed the cited evidence.
