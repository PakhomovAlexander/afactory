- Add a bounded background merge watcher for the manual bugfix/easy workflow: green CI keeps
  waiting for the exact human merge, then verified clean local cleanup writes the caller's durable
  `DONE` report. Changed heads, uncertain cleanup and unsafe worktrees fail closed; Task Stores
  and audit evidence stay retained. Native Scenario contracts are unchanged.
