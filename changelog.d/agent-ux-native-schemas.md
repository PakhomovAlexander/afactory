- Review-Campaign reviewers now reply under a native schema, as typed Task Workers already did:
  Claude Code receives the Reviewer Result schema as `--json-schema`, Codex its strict projection
  as `--output-schema`, so the CLI refuses a structurally wrong reply inside the call instead of
  the kernel failing a paid Attempt on it. The flat result contract and the kernel's parsing and
  admission are unchanged. A reviewer whose CLI ends without a constrained reply now fails as
  `provider_failure`, charged the usage that call reported, rather than `invalid_output_contract`
  ([ADR-0246](docs/adr/0246-carry-the-reviewer-result-schema-natively-on-the-campaign-path.md)).
- `af review render` prints the reply schema a model Worker's dispatch passes on its own `reply`
  header line; `--json` now prints `af/review-render@2` (`schemas/review-render-v2.json`) with a
  `reply_schema` field, `null` for a command Worker.
