- Recurring GitHub backlog enrollment can opt in to a receiver-local Issues-read observer.
  Each frozen batch retains its own checked observation before assessment. Failed reads leave
  work private for retry, and independently verified matches to existing issues settle without
  publication. [Operator setup](../docs/project-backlog.md#opt-in-to-recurring-github-issue-indexing)
- The GitHub observer refuses inconsistent page walks even when an issue closure leaves the
  page count unchanged. Enrolled Projects can pause to admit a manual batch and finish it with
  its own Scenario and index pin after enrollment resumes.
  [Manual mode](../docs/project-backlog.md#optional-manual-mode)
