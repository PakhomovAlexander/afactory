- Local class execution (`make check-classes`, `make check-fast`, `make check-full` and the Task
  gate's `scripts/verify.sh --profile fast`) no longer runs the live container probes, so it needs
  no Docker. CI's `container-probes` job still runs them for every e2e selection and validation
  still requires its success; `make review-kernel-container-probes` still runs them on demand and
  fails without a container runtime. A local pass is no evidence that the probes passed.
