- Verify actual offline Rust and Python compilation/tests through admitted Task Worker packages
  for implementers and reviewers, including private state, experiment isolation and exact command
  receipts. Keep transport fixtures distinct from live container containment evidence.
- Declare the Rust example's pinned `cc` linker driver so Cargo can link tests through the
  configured tool PATH without inheriting the operator's PATH.
- Exercise subprocess crashes inside cache capture and clone, then reopen the Task Store and
  sweep abandoned private roots; verify lease protection, preserved roots and refusal to admit
  or reuse incomplete caches under the existing Worker development contracts
  ([design](docs/design/worker-local-development.md)).
