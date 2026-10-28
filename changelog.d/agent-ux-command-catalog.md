- `af help --json` prints a machine-readable catalog of every command, and `af help COMMAND
  --json` (`af help task run --json`) of one command and those below it: one
  `af/command-catalog@1` document (`schemas/command-catalog-v1.json`) generated from the command
  definitions the binary parses, with each command's usage, subcommands and arguments, and each
  runnable command's effects: whether it may charge tokens, the state it may write (`store`,
  `files`, `machine`, `remote`) and what it needs from a person (`login`, `approval`,
  `terminal`), each with the flag that turns it on. A command without an effects entry fails the
  build's tests ([ADR-0247](docs/adr/0247-generate-a-machine-readable-command-catalog.md)).
- `af schema show ID` prints the JSON Schema of any document `af help json` lists, as `schemas/`
  holds it (`af schema show af/task-status@2`); `af schema show` lists the IDs.
