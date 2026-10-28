- The repository's catalog now pins its default Task pipeline, `default/implementation` and
  `default/verification`: one implementing iteration (`claude-opus-5-5`, high), two required
  reviewers (bugs on `gpt-5.6-terra`, correctness on `gpt-6-sol`, both high), one advisory
  reviewer for gate time and quality in the `uix` slot (`gpt-6-astra`, medium) and an evaluator
  (`gpt-6-astra`, high), so any checkout plans it; `docs/tasks.md` ("The default pipeline") says
  how to run it and bind its Providers
  ([ADR-0253](docs/adr/0253-the-default-task-pipeline-lives-in-the-catalog.md)).
