- Made the performance supervision test for a late container-removal confirmation
  deterministic (issue #582):
  `tests.test_supervise.ContainedSample.test_a_late_confirmation_in_real_time_is_unconfirmed`
  now drives `remove_container` with a fully controlled clock that stands still until the
  real `rm` invocation has completed and then reads 0.1 s, so the confirming `ps` client
  always starts, and its real 0.3 s start leaves the clock at 0.3 s, past the 0.2 s
  allowance, on hosts however slow or however scheduled.
