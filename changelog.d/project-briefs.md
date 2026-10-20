- Project Briefs: a pipeline's root port `root_default = "project_brief"`, `brief = "<variant>"`,
  declared in an `af.pipeline/2` pipeline (the new version of the pipeline shape; an
  `af.pipeline/1` pipeline carrying the port is refused, and every other pipeline keeps
  `af.pipeline/1`), carries `af/ProjectBrief@1`, a bounded selection of the Survey the Task pinned
  when it was planned, into the Workers that wire it and no other, each listing it in its context
  manifest (`required_by` `port:<name>`). Variants are `[brief.<name>]` tables of `.af/af.toml`
  (`sections`, `max_bytes` 1,024 to 262,144, `decisions`, `hot_paths`; built in: `orient` and
  `none`), refused at policy load outside their closed schema. Whole sections are dropped,
  last-declared first, until the exact canonical bytes fit; a Brief that cannot fit fails the plan.
  The initial capture pins the Survey of the Project of the resolved source Snapshot (a `source`
  bound from another repository gets that repository's Survey or an empty port, never the invoking
  checkout's) and records `af/ProjectSurveyPin@1` and `af/ProjectBriefCapture@1`, so a refresh, a
  removed Project Store or a deleted checkout changes nothing; each plan receipts its Brief ports
  (`af/ProjectBriefReceipt@1`) with `behind_commits` observed at capture, which `af task plan` and
  `af task explain` print. A plan whose Worker request, serialized as the runner renders it, would
  exceed the 1 MiB cap with the Brief is refused before dispatch. No author email enters a Brief,
  while repository text such as a decision title stays verbatim. A Task file may bind the port to a
  recorded Brief output; a Brief the kernel did not construct, a Worker output among them, is
  refused (`not_kernel_constructed`). `af project brief [--variant NAME] [--survey ID] [--json]`
  prints the exact bytes a plan would bind (`af/project-brief-preview@1`), recording and
  registering nothing
  ([ADR-0159](docs/adr/0159-carry-a-bounded-project-brief-on-a-declared-port.md)).
