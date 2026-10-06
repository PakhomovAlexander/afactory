- An Attempt whose Provider reported no usage is charged 0 tokens and its usage is recorded as
  unknown, with its cause, instead of being charged its whole reservation: a Codex Attempt that
  failed with `Selected model is at capacity` no longer costs 400,000 tokens, nor does one
  recovered after its writer's lease expired while the machine slept. The settlement's
  `af/TaskExecutionRecord@5` carries `unknown_usage: { cause }` (`capacity`, `rate_limit`,
  `authentication`, `model_unavailable`, `network`, `lease_expired`, `interrupted` or
  `unreported`); such an Attempt adds nothing to the Task's, node's or verification reserve's
  charged tokens and still counts against the Attempt limits, and Attempts with reported usage
  are charged exactly as before. A Codex Attempt that ends with an `error` or `turn.failed`
  event names its classified cause, such as `Provider model at capacity (capacity)`, instead of
  the bare exit status. `af task show`, `af task list`, the browser's Tasks and Workers panes and
  `af task report` show such usage as unknown, never as 0 spend: a Tokens cell reads
  `1,200 (+1 unknown)`, the round's details name the cause, and the JSON documents carry the
  count. Stores written by earlier releases read as before, with their recorded charges
  ([ADR-0143](docs/adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)).
