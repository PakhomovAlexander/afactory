- A pull request to `main` labeled exactly `no-perf` is not measured by the `Performance`
  workflow: adding the label cancels the running measurement, removing it measures again, and
  the stable `Performance check` reports the exemption as `exempt (label no-perf), not
  measured`, never as a measured pass. Pushes to `main`, the nightly schedule, dispatches,
  calibration, release handoffs and reruns ignore the label. Agents apply it only to a pull
  request whose whole diff is plainly prose with no performance impact. A Remote Check
  mapping entry may list `no_perf_paths`; a gate pull request whose whole change from the
  Task's source lies in them gets the label, which af removes before a head update that no
  longer qualifies and never creates in the repository
  ([ADR-0196](docs/adr/0196-skip-pull-request-measurements-only-for-an-explicit-no-perf-label.md)).
