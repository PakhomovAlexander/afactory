# Native catalog contract fixtures

These packages preserve current native generic verification, Pi planning and performance
Scenario contracts independently of Afactory's installed operator catalog. Their namespaces
are fixture identities, not supported operator selections or aliases. Tests copy this authority
into disposable consuming repositories and use hermetic Providers. No live execution is intended.

The catalog pins every package through `af catalog digest`; production operator choices live in
[issue-workflows](../issue-workflows/README.md). Retired installed directories can be removed
without deleting these generic API regression contracts. The kernel experiment fixture also
uses this explicit implementer contract. No compatibility reader or runtime fallback is added.
