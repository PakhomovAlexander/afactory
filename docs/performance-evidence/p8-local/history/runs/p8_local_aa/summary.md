### AF performance: pass (exit 0; partial&#95;scope scope, measured evidence); does not satisfy the required check: only full scope can satisfy it; this run's scope is partial&#95;scope; only an activated policy can; this plan's authority mode is proposed

| Case | Status | Outcome | Interval [L, U] | Baseline median | Candidate median | Change | Failures b/c | Missing |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| canonical&#95;validate&#95;small&#95;&#95;package&#95;small&#95;envelope | within&#95;tolerance | pass | &#91;-1459/1000000 (≈ -0.001459), 2041/1000000 (≈ 0.002041)&#93; | 51291/2000000 | 101/4000 | -791/2000000 (≈ -0.0003955) ms (-79100/51291 (≈ -1.54218)%) | 0/0 | 0 |
| cas&#95;typed&#95;get&#95;small&#95;&#95;package&#95;small&#95;envelope | within&#95;tolerance | pass | &#91;-2813/500000 (≈ -0.005626), 23/4000 (≈ 0.00575)&#93; | 167083/2000000 | 84521/1000000 | 1959/2000000 (≈ 0.0009795) ms (195900/167083 (≈ 1.17247)%) | 0/0 | 0 |
| cli&#95;discovery&#95;&#95;command&#95;version | within&#95;tolerance | pass | &#91;-292833/1000000 (≈ -0.292833), 244167/1000000 (≈ 0.244167)&#93; | 6224729/1000000 | 12512249/2000000 | 62791/2000000 (≈ 0.0313955) ms (3139550/6224729 (≈ 0.504367)%) | 0/0 | 0 |

152 of 152 planned records matched. Measured evidence: every planned record was admitted and succeeded; each pair's baseline and candidate samples were recorded within one shard, on one host, the one its terminal record names (see Shards), interleaved in the plan's seeded AB/BA order; intervals are this run's own paired comparison.

#### Diagnostics

No diagnostic child run is linked: every case's profile and trace is unavailable here.
