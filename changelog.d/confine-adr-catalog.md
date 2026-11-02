- ADR discovery refuses symlink files and directory components, reads only regular files beneath
  its declared directory, and emits no partial catalog on refusal. The
  [ADR guide](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/README.md)
  preserves legacy identities without inferring their status.
