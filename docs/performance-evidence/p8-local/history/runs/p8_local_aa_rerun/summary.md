### AF performance: pass (exit 0; partial&#95;scope scope, measured evidence); does not satisfy the required check: only full scope can satisfy it; this run's scope is partial&#95;scope; only an activated policy can; this plan's authority mode is proposed

| Case | Status | Outcome | Interval [L, U] | Baseline median | Candidate median | Change | Failures b/c | Missing |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| canonical&#95;validate&#95;small&#95;&#95;package&#95;small&#95;envelope | within&#95;tolerance | pass | &#91;-1/2000 (≈ -0.0005), 271/250000 (≈ 0.001084)&#93; | 50167/2000000 | 50417/2000000 | 1/8000 (≈ 0.000125) ms (25000/50167 (≈ 0.498336)%) | 0/0 | 0 |
| cas&#95;typed&#95;get&#95;small&#95;&#95;package&#95;small&#95;envelope | within&#95;tolerance | pass | &#91;-7/1000 (≈ -0.007), 1/250 (≈ 0.004)&#93; | 41521/500000 | 267/3200 | 791/2000000 (≈ 0.0003955) ms (19775/41521 (≈ 0.476265)%) | 0/0 | 0 |
| cli&#95;discovery&#95;&#95;command&#95;version | within&#95;tolerance | pass | &#91;-2087/8000 (≈ -0.260875), 258251/1000000 (≈ 0.258251)&#93; | 1562177/250000 | 6243521/1000000 | -5187/1000000 (≈ -0.005187) ms (-129675/1562177 (≈ -0.0830092)%) | 0/0 | 0 |

152 of 152 planned records matched. Measured evidence: every planned record was admitted and succeeded; each pair's baseline and candidate samples were recorded within one shard, on one host, the one its terminal record names (see Shards), interleaved in the plan's seeded AB/BA order; intervals are this run's own paired comparison.

#### Diagnostics

No diagnostic child run is linked: every case's profile and trace is unavailable here.
