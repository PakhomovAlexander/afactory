- Forward declared Worker context through container stdin so local development harnesses can
  read their input. Preserve separate bounded output streams and report live probe failures
  with status, truncation and captured stdout/stderr diagnostics.
