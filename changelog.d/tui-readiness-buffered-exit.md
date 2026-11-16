- Fix the performance test readiness helper to consume a complete buffered PID record after
  driver exit, while rejecting missing, incomplete or invalid readiness and retaining cleanup
  safety checks. Wrap the Scenario port-type help paragraph to fit a 100-column terminal.
- Serialize release fixture database transactions so API observations retain concurrent CI-plan
  updates. Await recorded plan reads in the full-profile lifecycle test and include persisted
  release state and driver status in bounded wait failures.
