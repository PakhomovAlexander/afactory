# Inert accepted-candidate composition

`bundle.json` is the `documentation-update` Scenario. Its `pagination-cli` Task is an ordinary
code Task, planned with the command-backed `fixture/implementation` Pipeline from the CLI test fixtures
(`fixtures/task-runtime/pagination`); it is not an implementation-specific runtime mode. The example
starts that pinned Task, prepares its accepted candidate, asks for an exact signed Decision,
uses the existing `publish_pr` Action and `checks_passed`/`merged` Waits, then cleans the owned
workspace. The native integration test exercises a real accepted Task from this fixture family.

All paths, commits, Task plan/revision IDs, numeric service identities and tool digests are
placeholders. No policy is installed and no service is contacted. The example does not execute
without separately provisioned machine authority, actual native Task pins and a human signature.
See [the workspace guide](../../../docs/scenario-git-workspace.md) for the trust boundary.

`config.example.json` demonstrates the explicit exclusive writer mode. It must never be selected
for a branch that humans, other installations or automation may update. The ordinary GitHub
adapter's locked-head mode remains the appropriate contract for administrator-protected heads.
