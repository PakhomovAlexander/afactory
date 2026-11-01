- An executor image whose volume declaration or identity refuses at preflight, submission or the
  claim no longer leaves the machine fence that disabled every later experiment: only
  `docker image inspect` ran, no probe container, so the refusal settles clean; an unconfirmed
  probe container still keeps the fence.
- A CLI CPU capture of many fresh processes lists the build IDs with hits of every recording, in
  order, not only the first: `binary_build_id_present` is no longer `false` when only a short
  first execution had no AF sample. A listing that fails or is malformed refuses the capture,
  naming its recording, instead of being read as no build IDs.
