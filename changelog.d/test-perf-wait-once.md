- Tests that loop over cases bound by a real lease or deadline now prepare every case, wait once
  on the real clock until every case's lease or deadline has passed, and then run each case,
  which first asserts that its own lease or deadline has passed (issue #204). No production
  timing constant, real clock or assertion changed; tests whose subject is the wait are untouched.
  `task_runtime::usage_recovery::worker_and_provider_cas_failure_recover_full_reported_usage_without_another_call`
  and `incomplete_native_observation_survives_cas_outage_and_both_admission_and_worker_recovery`
  went from two real lease waits each to one (about 30.5 s to 15.5 s), and each case now also
  asserts that the Task's absolute deadline has not passed when it recovers.
  `task_campaign_review::host::an_expired_review_records_incomplete_without_inventing_unstarted_gate_facts`
  went from three real deadline waits to one (5.2 s), and each mode now asserts the exact
  dispatch refusal `Task plan deadline expired` instead of any error. In review-store,
  `recording_resume_rejects_revoked_decision_or_replaced_latest_report` first gained exact refusal
  causes for every case: the expired decision and the revoked decision each refuse with the
  approval conflict, pinned to expiry or revocation alone by the decision's state; the replaced
  report's resume refuses for carrying no publication failure; and its stale-report append refuses
  at the expired-pause gate, whose other conditions the preceding refusal proves. A temporary
  variant without the wait then failed: the expired-decision and replaced-report resumes refused
  with `Recording recovery requires the exact expired admitted publication pause` instead. The
  revoked case refuses the same way with or without the wait, because revocation is checked
  before the deadline, so its own past-deadline assertion is what catches a missing wait. Only
  then were its three deadline waits coalesced into one (9.2 s to 3.2 s). No target was left
  unchanged.
