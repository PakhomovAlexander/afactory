- `scripts/issue-workflows.py compose` accepts stage templates whose `af.task-bindings/1`
  carries the canonical empty `slots: {}` and `packages: {}` tables the runtime writes, and
  still refuses non-empty slot or package overrides, unknown keys and non-personal accounts
  (#1206).
