Fix #911: native wake ordering now skips only validated expired or revoked routes. Missing,
corrupt, substituted or foreign Store routes and database errors stop later native reservation
and admission, preserving the earlier packet's recovery and the original session's uncertainty
fence. Deterministic Store and public-writer regressions cover restart, repair and valid
expiry/revocation. See the [decision](../docs/adr/2026-11-14-refuse-invalid-routes-before-native-wake-admission.md).
Live Pi/Codex certification remains separate; Claude live testing remains deferred.
