- A CLI CPU profile of many fresh processes is representable again: a capture lists its own
  files, and its `af.perf-repetitions/1` index (`repetitions.json`, bound by size and SHA-256
  from `af.perf-profiles/7`) lists every repetition's raw recording and host-observer record in
  order. Report admission, offline and in a comparison export, refuses a missing, changed or
  unlisted repetition. A real capture of 188 repetitions per side had failed to write
  `profiles.json` at all.
- Each recording of a CLI capture is decoded by its own `perf script`, in order, under the
  tool's own time bound and the diagnostic deadline, and a decoding that fails names its
  recording in a report that still serializes.
- A host-observed CLI command's region time is its exec to its exit in the trusted recording,
  no longer the supervised wall that included the observer's attachment and the wait at its
  gate; a recording without both records stops the capture as `command_boundary_unknown`.
