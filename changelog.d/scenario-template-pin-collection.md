- Keep Scenario template capture and pin publication under the native Store writer lock,
  validating captured Worker packages before publication so concurrent Task collection cannot
  remove their closure, including reused objects. Reject corrupt pin sequences without changing
  history. See the [sealed authority decision](../docs/adr/2026-11-13-seal-local-worker-authority-in-scenario-templates.md).
