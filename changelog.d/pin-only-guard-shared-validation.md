- Make the standalone pin-only guard reuse the preparation helper's semantic and byte validator.
  Unknown `[af]` fields in either lock are refused, including additions, changes and removals;
  valid version/digest updates still preserve every non-pin table and byte.
