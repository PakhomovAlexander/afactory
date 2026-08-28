# Address Campaign state by opaque ID

**Status:** accepted (2026-08-28)

The Campaign label supplied by `--campaign` has historically been interpolated directly into the
default state path. Rejecting non-normal path components prevents basic traversal, but it leaves a
human-facing label responsible for both presentation and filesystem identity. Campaign enumeration
also needs to recover the label and immutable authority from existing state without inventing a
second mutable catalog.

New default Campaign state directories use `c-<sha256>` IDs. The digest is computed over the
domain-separated byte string `af/campaign-id@1\0` followed by the validated UTF-8 label. The label
remains durably recoverable from the existing `campaign-<label>` run ID; Campaign authority remains
the existing `CampaignOpened@1` event and referenced `CampaignManifest@1`. No persisted Review
Kernel event or artifact shape changes.

Labels are trimmed, non-empty single components and reject control characters, platform
separators, and reserved traversal forms. The state root and selected directory are resolved before
use, and the selected directory must remain beneath that root. Existing label-named directories
remain readable as a permanent compatibility path. If both legacy and opaque directories exist for
one label, Afactory refuses the ambiguity. Enumeration does not follow symlinked Campaign entries.

## Considered options

- **Continue using the validated label as the directory name.** Rejected because presentation data
  would remain filesystem authority and every future label feature would reopen traversal and
  portability analysis.
- **Percent-encode the label.** Rejected because canonical escaping rules become another durable
  format, can be decoded inconsistently, and still expose user-controlled names to filesystem
  semantics.
- **Persist a separate mutable Campaign catalog.** Rejected because it duplicates labels and
  authority already present in the append-only event store and creates synchronization and crash
  recovery obligations for query-only enumeration.
- **Derive an opaque ID and project the label and authority from existing state (chosen).** This
  gives one deterministic path identity, requires no new persisted kernel contract, and retains a
  strict compatibility reader for existing Campaigns.

## Consequences

- A new Campaign label maps deterministically to one opaque state directory beneath the configured
  review-state root.
- Operators see both ID and label; IDs are stable but deliberately not reversible without Campaign
  state.
- Legacy state remains readable but is not silently migrated or duplicated.
- Moving or hand-copying Campaign databases under arbitrary directory names fails enumeration;
  operators must preserve either the opaque ID or the exact legacy label.
- The run ID still contains the validated label. Hiding labels from local storage is not a goal of
  this decision.
