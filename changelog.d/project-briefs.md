- Project Briefs: a pipeline's root port `root_default = "project_brief"`, `brief = "<variant>"`
  carries `af/ProjectBrief@1`, a bounded selection of the Survey the Task pinned when it was
  planned, into the Workers that wire it and no other, each listing it in its context manifest
  (`required_by` `port:<name>`). Variants are `[brief.<name>]` tables of `.af/af.toml`
  (`sections`, `max_bytes` 1,024 to 262,144, `decisions`, `hot_paths`; built in: `orient` and
  `none`), refused at policy load outside their closed schema. Whole sections are dropped,
  last-declared first, until the exact canonical bytes fit; a Brief that cannot fit fails the
  plan. The initial capture records `af/ProjectSurveyPin@1` and `af/ProjectBriefCapture@1`, so a
  refresh, a removed Project Store or a deleted checkout changes nothing; each plan receipts its
  Brief ports (`af/ProjectBriefReceipt@1`) with `behind_commits` observed at capture, which `af
  task plan` and `af task explain` print. A Task file may bind the port to a recorded Brief
  output; a Brief the kernel did not construct, a Worker output among them, is refused
  (`not_kernel_constructed`). `af project brief [--variant NAME] [--survey ID] [--json]` prints
  the exact bytes a plan would bind (`af/project-brief-preview@1`), recording nothing
  ([ADR-0153](docs/adr/0153-carry-a-bounded-project-brief-on-a-declared-port.md)).
