Fix native nightly publication recovery by reading back the exact remote release and tag
and checking every asset against the sealed signed build and certified report. Refuse failed
publication receipts and a current tag pruned by retention, without repeated remote mutations.
The closed `af.nightly-publish/2` receipt durably records structured notification and pruning
failures, including release-list and release-delete failures. Version 1 receipts are refused;
standalone amendment and retention policy remain unchanged. Historical Review findings remain
historical evidence; this repair does not change their Ledger dispositions.
