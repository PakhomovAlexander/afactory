- Add ADRs with stable date-topic identities and discover them with
  `python3 -I scripts/adr.py list`, without editing a shared index or allocating a number.
  Existing accepted identities are preserved; the proposed Pi SDK host decision now has
  its own date-topic identity. See the
  [authoring decision](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/2026-11-02-add-independent-adrs-and-derive-the-catalog.md).
- Resolve Scenario Task receipts against the current Task contract schema after integrating Pi;
  the Task result wire shape and Scenario/Ant execution behavior are unchanged.
