Performance Lab: a sample, diagnostic or telemetry container's removal is now confirmed only by
the typed outcome `supervise.remove_container` returns (`supervise.Removal`, removed and
confirmed absent, apart from its human detail), never by that detail's wording. A failed
launch whose removal raised an exception worded like `container was not removed` was certain,
and diagnostics removed the namespace a live process could reach; now it stays uncertain and
the namespace is kept (issue #388). A removal whose confirmation ends after its allowance,
however long its `docker` client took to start, is reported unconfirmed (issue #403, item 4).
Every deadline, the removal allowance and the existing messages are unchanged. The telemetry
cleanup docs now state what a failed ownership write keeps. A failure before `state.json` is
replaced keeps the earlier fenced record. A directory sync failing after the replacement leaves
the new record, which was already proved, and the next controller and the workflow's cleanup
are admitted. Regression tests pin both boundaries.
