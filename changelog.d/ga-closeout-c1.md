- A failed gate names `af task run ID --retry-gate` only when the kernel would run it: a Task
  whose budget leaves no Attempt for a new check, whose plan approval expired or whose deadline
  passed names `af task show ID --checks` instead, and `--retry-gate` refuses it with the reason
  and `af task plan --file TASK_FILE --fit`. A failed paid Worker that ran beside the gate, with
  no edge to it, refuses the retry too (`a paid node ran beside its failed gate`).
- While a gate retry is pending, `af task show --json` keeps the `gate` object, with the checks
  of the receipt the retry superseded and the pending retry under `retries`.
- An Attempt abandoned by SIGTERM or by a lost writer is charged the usage its Provider reported,
  incomplete counters included, never its reservation; the rest returns to the Task's budget
  ([ADR-0203](docs/adr/0203-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md)).
  An Attempt its own answer or failure ended still keeps the reservation floor.
- An interrupted `af task run` whose node used up its recovery allowance names `af task start
  --file TASK_FILE`, the next command `af task show` names, instead of a resume that refuses.
  Under `af task run --verbose` the resume line comes first and the power assertion line after
  it, each wrapped at 100 columns.
- A verification Task delivers another Task's candidate only when its output is the very
  Snapshot it verified; another Snapshot with the same tree is refused
  ([ADR-0205](docs/adr/0205-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)).
- A judging Worker that restores write permission on `../base` or `../diff-subject.json` and
  changes them has its Attempt refused at seal, naming the path
  ([ADR-0206](docs/adr/0206-continue-a-task-from-its-reviewed-candidate-and-give-judging-workers-the-base.md)).
- A Task Worker that misses its wall is named by its slot (`implementer did not answer within
  …`), not as a reviewer. A check line whose warm directory was evicted ends `; the next check
  runs cold`. The deadline refusal lists each node on the longest path as `1 × wall`, the
  verification reserve, their total and the Task's wall.
- Concurrent content-store writers never replace an object another one already published; the
  existing object is verified and kept. A sandboxed macOS check whose `sysctl` lookup is refused
  reads the load average through `uptime`. The gate's own preflight gives its child builds a
  private `TMPDIR`.
