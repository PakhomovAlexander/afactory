Bare `af` now has a pending 200 ms first-browser-frame performance target in the continuous
laboratory. A 100x30 pseudo-terminal case measures an empty private registry and 20 privately
registered copies of the 10,000-event Task seed, recording launch-to-browser (ending when the
status line reaches the terminal) and launch-to-first-output intervals for each sample. Its
driver ends the browser's whole session on success, failure and cancellation, finds a process
that left the session by the run's token in its environment (one that replaced its environment
before `exec` is beyond it, as beyond any process-group sample), and refuses a sample whose
browser left a process running or whose cleanup it cannot confirm; the shard and a diagnostic
child run keep that sample's scratch for recovery, as they do when their supervisor had to end
the driver. The case was added for the
splash delay diagnosed
in [issue #402](https://github.com/PakhomovAlexander/afactory/issues/402) and measures whatever
binary it is given, the repaired browser included; it does not activate a shipping budget
([ADR-0260](docs/adr/0260-bound-bare-afs-first-browser-frame.md)).
