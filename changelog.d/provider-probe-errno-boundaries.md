- A Provider capability probe that fails with `os error 11` or `os error 111` (such as
  `Connection refused`) is no longer reported as a host permission denial (`probe_denied`): errno
  values are matched on word boundaries, and a CLI the host refuses to start is judged by its typed
  error kind ([#393](https://github.com/PakhomovAlexander/afactory/issues/393)).
