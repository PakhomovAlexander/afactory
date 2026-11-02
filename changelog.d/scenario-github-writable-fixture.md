- Initialize GitHub Scenario CLI test state as a private writable file even when the source
  fixtures are read-only. The installed production adapter's full PR, recovery, review and
  follow-up sequence now exercises read-only fixture copies and verifies they remain unchanged.
