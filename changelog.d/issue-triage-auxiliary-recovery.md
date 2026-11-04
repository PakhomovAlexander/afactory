- Record a definite auxiliary label refusal before any mutation as `aborted` in the v3 journal,
  retaining the intended labels and body and restoring prior canonical comment authority. A later
  eligible sweep can proceed; partial and unknown writes remain fenced until reconciled.
- Adopt an exact existing runner sweep or reminder comment with its body and completion checkpoint
  saved together, without issuing API writes.
