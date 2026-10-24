- An Attempt survives one failed call inside its own wall and token reservation
  ([ADR-0202](docs/adr/0202-recover-an-attempt-from-a-transient-provider-failure-and-a-reply-that-does-not-parse.md)):
  a typed reply that does not parse is read tolerantly and then re-asked once, with both replies
  kept and named (#283); Codex typed calls pass their reply contract as `--output-schema` (#33);
  a network, capacity, timeout or silent-exit failure is waited out and retried up to three
  times, each wait recorded as a `TaskProviderEvent@1` that `af task show` names while it lasts
  (#295); an identity probe that times out or answers malformed is re-probed instead of read as
  an account change, and a Provider CLI update with the same account passes (#289); a refused
  admission probe is sent once more in other words, and every admission is decided before any
  gate starts; a report draft is validated for shape before sealing and the author is
  re-dispatched with the validation (#167). The shipped reviewer and evaluator slots allow two
  Attempts, and `af task show` prints each failed node's cause and first line. A sealed report
  whose verification missed the plan deadline still cannot be verified later (#166).
