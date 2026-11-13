- Release retries require the original signed bundle once any release record exists, even an
  empty draft, and refuse every conflicting asset byte, including a different valid signature.
  Bounded complete release and secret-name lookup prevents page-boundary identity reuse and
  false signing or pin-readiness failures, including releases whose Git refs were deleted.
