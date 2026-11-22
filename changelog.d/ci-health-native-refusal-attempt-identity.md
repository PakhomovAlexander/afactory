- CI health refused-diagnosis reporting now retains native Attempt IDs instead of requiring
  artifact digests, allowing deterministic unknown findings, alert-only remedy and outcome
  notifications to complete while preserving the refused Task receipt and skipping Watch.
- Native diagnosis settlement records now use `af/scenario-ci-diagnosis@2`; version 1 fails closed.
