# Where the af development cycle spends its time and disk in the research\-pipelines campaign\, and the change the plan should make next

## Scope and method

Evidence\: the 48 bound source entries \(the same entries this Snapshot holds in docs\/design\/research\-tasks\.sources\.json\, one per Task\, each text on one line of that file\)\, the release\_build comparison and its baseline and candidate Measurements\, and the repository files cited\. Every figure below is either quoted from the named source\, Measurement\, comparison or file line\, or was computed here by a python3 script that parses docs\/design\/research\-tasks\.sources\.json and sums or subtracts the recorded span elapsed\_ms\, cache bytes\_available and chargeable\_tokens values\; computed figures say so\. Each cache figure is attributed to the check \(kernel or markdownlint\) whose observation records it\.

The cycle every Task pays is the code policy\'s two required checks\: kernel runs bash scripts\/verify\.sh \(\.af\/code\-policy\.toml line 14\)\, which runs make check with the bound CARGO\_TARGET\_DIR \(scripts\/verify\.sh line 20\)\; make check is fmt\, lint\, test and release\-check \(Makefile line 8\)\, the same target CI runs \(\.github\/workflows\/validate\.yml line 38\)\. markdownlint runs markdownlint\-cli2 over the Markdown files\. Both run within check\_wall\_ms \= 3600000 \(\.af\/code\-policy\.toml line 3\)\.

## Findings

1\. Time goes to the kernel check\, and within it to test execution\, not compilation\.
Computed over all sources\: the 38 recorded kernel check spans sum to 28946211 ms \(about 8\.0 hours\)\; the 38 markdownlint check spans sum to 523842 ms\. The largest markdownlint span is 24048 ms \(research\_r1\_verify\_8\)\; passing kernel spans run from 712414 ms \(bench4\_warm\_1\) to 952576 ms \(research\_r4\_verify\_2\)\.
The paired benchmark on one pinned Snapshot records cold kernel spans of 828106\, 834455\, 828784\, 828604 and 829387 ms \(bench4\_cold\_1 to bench4\_cold\_5\) and warm kernel spans of 712414\, 718908\, 716906\, 718046 and 719401 ms \(bench4\_warm\_1 to bench4\_warm\_5\)\. Computed pairwise\, the warm kernel check is 109986 to 115692 ms shorter\, a warm\/cold ratio of 0\.8603 to 0\.8674\. The step split is not in the sources\, which record whole check spans only\; the section 6 record gives it\: cold lint 41\-47 s and test\-build 49\-52 s become warm 20 s and 13\-15 s\, while test runs 728\-732 s cold and 672\-679 s warm \(docs\/design\/research\-pipelines\.md lines 671 and 672\)\. The test step is cargo test with \-\-test\-threads\=\$\(TEST\_THREADS\) \(Makefile line 23\)\, TEST\_THREADS defaulting to 4 \(Makefile line 5\)\. A warm check therefore removes most compilation\; what remains of a roughly 12\-minute warm kernel check is the test suite running\.

2\. Cache preparation is cheap\; a failed probe or an over\-bound directory is what costs\.
On warm kernel checks the cargo\_target materialization\_ms ranges from 188 \(bench4\_warm\_1 and bench4\_warm\_4\) to 1827 \(research\_r3\_verify\_1\)\; the largest kernel cargo\_home materialization\_ms is 262 \(research\_r3\_verify\_1\)\. The markdownlint check\, which builds nothing\, also receives both warm kinds because \[warm\] build\_cache applies to every check \(\.af\/code\-policy\.toml line 43\)\; its cargo\_target materialization\_ms ranges from 176 \(bench3\_cold\_1\) to 922 \(research\_r4\)\. These are sub\-second to under two seconds against spans of minutes\.
The expensive cases are recorded as ineligible observations\. On research\_r1\_verify the kernel check\'s cargo\_target\:toolchain\_unresolved lookup took 30031 ms and the check ran cold in 940635 ms\; on research\_r1c the same lookup took 30023 ms and the kernel check ran 908827 ms\. Section 6 records the cause\: with the check\'s fresh HOME the rustup proxy downloaded the toolchain before answering \(docs\/design\/research\-pipelines\.md line 610\)\. On research\_r5\_verify\_1 the kernel check\'s cargo\_target\:bound\_exceeded and cargo\_home\:bound\_exceeded observations each record lookup\_ms 30800\, and the kernel check then ran cold in 866300 ms\.

3\. Disk\: the warm cargo\_target directory grows with every Task until the eviction bound forces a cold check\.
After a cold kernel check the markdownlint check finds cargo\_target at 8211239576 bytes \(bench3\_cold\_1\) and 8216496382 to 8216501386 bytes \(bench4\_cold\_1 to bench4\_cold\_5\)\; cargo\_home is 108889726 bytes on every warm observation of the bench and R1\-R5 Tasks\. Computed as the bytes the markdownlint check found minus the bytes the kernel check found just before it\, each warm kernel check on the pinned Snapshot adds 421733128 to 422585324 bytes \(bench3\_warm\_1\, bench4\_warm\_1 to bench4\_warm\_5\)\. Across the research Tasks\, whose sources changed\, the same difference runs from 422538022 bytes \(research\_r1\_verify\_2\) to 1436926468 bytes \(research\_r5\)\.
The consequence is recorded twice\. research\_r2\'s kernel check found 13496073144 bytes\; the kernel evicted 17979645773 bytes \(bound\_exceeded\) and the check failed at 61320 ms\, before any reviewer ran\. After research\_r2\_verify\_1\'s cold kernel check\, ten warm kernel checks \(research\_r2\_verify\_2 through research\_r5\) took the directory from 8371045355 bytes to the 17564366285 bytes research\_r5\'s markdownlint check found\, above the eviction bound max\_bytes \= 17179869184 \(\.af\/code\-policy\.toml line 45\; hard\_max\_bytes \= 34359738368 on line 47\)\, so research\_r5\_verify\_1\'s kernel check ran cold at 866300 ms\. Before the campaign section 1 recorded a 22 GB Store \(docs\/design\/research\-pipelines\.md line 38\) and about 14 GB of stray cargo targets \(line 39\)\; no source re\-measures either\.

4\. The release build is off the gate path\, and its experiment traded size for time\.
Baseline Measurement\: three cold release builds of 65408\, 63121 and 63176 ms \(median 63176\)\, binary\_bytes 44843248 each\, target\_bytes median 528792959\. Candidate Measurement\: 49144\, 49552 and 49059 ms \(median 49144\)\, binary\_bytes 53219120 each\, target\_bytes median 547812425\. The comparison on release\_build\_time is improved on elapsed\_ms by 14032 ms\, ratio 1754\/7897\, outcome passed against min\_improvement\_ratio \"0\.1\" \(\.af\/code\-policy\.toml line 77\)\; binary\_bytes regressed by 8375872 bytes and target\_bytes by 19019466 bytes\. The earlier experiment research\_release\_build\_2 recorded a baseline median of 63586 ms and ended incomplete before its candidate was measured\. The measure builds cargo build \-\-release \-p af \(scripts\/measure\-release\.sh line 10\)\; make check never builds a release profile \(Makefile line 8\)\, and release builds run only in the release workflow \(\.github\/workflows\/release\.yml line 105\)\. The candidate section 6 names\, lto \= \"off\" in the root release profile \(docs\/design\/research\-pipelines\.md line 942\)\, is not in this Snapshot\: Cargo\.toml declares only dev\-profile package overrides \(Cargo\.toml line 20 onward\)\. Whether the time is worth the larger binary is left to a human \(docs\/design\/research\-pipelines\.md line 948\)\. It does not shorten the gate\.

5\. Spend\: tokens go to models\, not to checks\; reply\-shape refusals cost whole Tasks\.
Computed over all sources\, chargeable\_tokens sum to 11748244\; the seven implementation Tasks \(research\_r1\, research\_r1b\, research\_r1c\, research\_r2\, research\_r3\, research\_r4\, research\_r5\) account for 6592808 and the eighteen verification Tasks for 4616473\. research\_r1\'s 1508251 includes an interrupted Attempt charged its full reservation \(docs\/design\/research\-pipelines\.md line 582\)\. Every check\-stage attempt wall in the sources records chargeable\_tokens 0\: the gate costs wall time\, not tokens\. Three verification Tasks ended incomplete because a reviewer reply was refused for its shape\: research\_r1\_verify\_7 \(321723 tokens\)\, research\_r3\_verify\_2 \(347598\) and research\_r4\_verify\_1 \(256631\)\, 925952 together \(computed\)\; research\_sdlc\_report\_2 \(228859\) ended incomplete on an unsorted citation set\. Gate failures unrelated to the candidate\'s intent also recur\: research\_r2\_verify\_1\'s kernel check failed at 121309 ms and research\_r3\_verify\_1\'s at 849384 ms\, and the first warm gate\, bench3\_warm\_1\, failed its kernel check at 536567 ms\.

## Recommendation

Make the gate\'s test execution the subject of the next research Task\, using the R2 measure\-and\-compare machinery unchanged\, before anything else\.

Why this change\: the recorded evidence puts the kernel check at 712414 to 719401 ms warm on the pinned Snapshot \(bench4\_warm\_1 to bench4\_warm\_5\)\, and section 6 attributes 672\-679 s of a warm check to the test step \(docs\/design\/research\-pipelines\.md line 672\)\. Every Task pays it\: 38 kernel spans summing to about 8\.0 hours in this campaign alone\. The warm cache has already taken compilation down to roughly 20 s of lint and 13\-15 s of test\-build \(line 672\)\; the release\-build candidate saves 14032 ms per cold release build that the gate never runs\; cache preparation is under two seconds\. No other recorded lever is of the same order\.

What to do\:
\(1\) A human adds a second measure beside release\_build in \.af\/code\-policy\.toml \(a Worker may not write \.af\/\)\: a gate\_test measure whose command runs the gate\'s own test target with the kernel\-bound CARGO\_TARGET\_DIR\, as scripts\/verify\.sh line 20 does for make check\, with warm \= true so compilation is taken out of the figure \(docs\/design\/research\-pipelines\.md line 237\)\, repetitions \= 3 as release\_build declares \(\.af\/code\-policy\.toml line 54\)\, and a per\-repetition wall\_ms whose total fits check\_wall\_ms \= 3600000 \(line 3\)\. release\_build\'s wall\_ms \= 1200000 \(line 56\) fits three times\, and every recorded kernel check span\, which includes the test run\, is below it \(the largest is 952576 ms\, research\_r4\_verify\_2\)\. Add a gate\_test\_time objective on elapsed\_ms\, direction lower\, min\_improvement\_ratio \"0\.1\" and min\_repetitions 3\, as release\_build\_time does \(line 77\)\.
\(2\) Run an experiment Task on kernel\/experiment whose implementer may change only the test runner selection and its concurrency\: TEST\_RUNNER and TEST\_THREADS \(Makefile lines 4 and 5\) and \.config\/nextest\.toml\. The Makefile already names nextest as an explicit cross\-binary benchmark kept out of the gate until validated \(Makefile line 3\)\. The candidate must execute the same test set\, doctests included \(Makefile line 20\)\; no test may be skipped\, ignored or retried \(nextest\'s ci profile keeps retries \= 0\, \.config\/nextest\.toml line 4\)\, and cargo stays the gate until the comparison passes and a human adopts the change\. Neither this report nor the plan asserts an improvement\: the declared objective and the recorded comparison are the result\, as R6 required\.

After that\, in this order\:
\(a\) Stop the warm cargo\_target directory from growing to the eviction bound\: ten warm kernel checks took it from 8371045355 to 17564366285 bytes and cost research\_r5\_verify\_1 a cold 866300 ms kernel check\. The fix belongs in removing prior revisions\' artifacts\, measured by the same cache observations\; max\_bytes \(line 45\) and hard\_max\_bytes \(line 47\) stay as they are\.
\(b\) Implement the recorded follow\-up that hands a reply\-shape refusal back to the same Attempt as feedback \(docs\/design\/research\-pipelines\.md line 849\)\: the three verification Tasks lost to such refusals charged 925952 tokens together\.
\(c\) Leave the lto \= \"off\" decision to a human\; it is not in this Snapshot and does not touch the gate\.

Nothing here raises a budget\, bound or wall\, removes a test\, or changes a check\'s command without a passing recorded comparison\.

## Not measured

No source records per\-step timings inside the research Tasks\; the step split used above comes from the section 6 benchmark record\, not from a Measurement\. No measurement exists of the test step under nextest or another thread count\, so no improvement is claimed for the recommended experiment\. No source records Store or CAS size after R5\'s af task gc\, CPU time\, or machine load for the research Tasks\. The markdownlint span fell from 15265\-24048 ms in every Task up to research\_r3 to 4896\-9155 ms from research\_r3\_verify\_1 onward\; no recorded evidence names the cause\. The entries for bench2\_cold\_2\, bench2\_warm\_1\, bench2\_warm\_2\, bench3\_cold\_2\, research\_r3\_verify\_3\, research\_release\_build\_1\, research\_sdlc\_report\_1\, research\_sdlc\_report\_2 and research\_sdlc\_report\_3 carry no runtime observations\.

## Sources

- Task bench3\-cold\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:d2080b14428c1ecafc061d54d3ecd93ade43249ef227ffb86fc25d961cca5b2f

- Task bench3\-warm\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:1b798463c55c58fa72768942e487ff86c43351e3b573efb9439daafab5ad493f

- Task bench4\-cold\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:2e60ca2f6796401649793393217e9d8bf47f90d261f1a7f16904b0fde6c8237f

- Task bench4\-cold\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:a0a8a88b5ea568de9f735003b3f12412c75dbd48110bc12e051437bc8245ffc2

- Task bench4\-cold\-3 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:e3b92bbc3ac655fc062fe969d383c41188d7cdc86b024a62356709382b815aa4

- Task bench4\-cold\-4 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:31771184155bb13b623e785507f65537e5eaf4eed405294ccebd0167938b0a7a

- Task bench4\-cold\-5 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:42a0bae472105fe77ad7c459d94ad90d70816fdb3b1e2b63dbe3c8d2efd7f312

- Task bench4\-warm\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:eb2ab7dd761d96f36849156229b38cbbd07638edaf6f342cb81174069213602d

- Task bench4\-warm\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:54ddb659b4db08fab929a2f6c5e851fb80f5c2d829284f09b054c0eda0dea5be

- Task bench4\-warm\-3 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:9df0ddf948f2311f8958db89ffa7cf76566ae911d5b2588f08b87e5ed15291c0

- Task bench4\-warm\-4 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:654b17833881788802f190612ba880c00cf3453c610e01602aac44537a0e1171

- Task bench4\-warm\-5 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:ac4b408c4f349904a80516f5763e42398fbf3de25072ef31849f35dc2c61c511

- Task research\-r1\-verify — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:2cf50e723f33a5d96de85989d5025a38b66fbfb1d90e68d75cde009c37a8a7a1

- Task research\-r1\-verify\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:346bb96a7b1d3e61aa934203cd5469112548bde314e4611a452fe06ecdb77b63

- Task research\-r1\-verify\-7 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:ad42c1921f9cd65c3d2909fd7b5d36c122f6dae1728af67fe43aa20dc1fd228a

- Task research\-r1\-verify\-8 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:90eeae238e4f61b36c5c75bf0f95f759c2fd4c1d817d62ec58b8e95aec983994

- Task research\-r1c — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:263727811343068777816e594b6a1070ebda805f3f616ee27f90ee677e489114

- Task research\-r2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:1992ff75a357be0a10fbf6b305eaad382bd2479e9a3af8f83dad46be4b8e3cc1

- Task research\-r2\-verify\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:75c4956502f86da8b24517bea061780344473b157e4f1c24d195fed7ca8af2f7

- Task research\-r2\-verify\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:d96dc82f47edd5c333d62d910a7be0dd2979b8698dc53aedfc6a661819bb81e3

- Task research\-r3\-verify\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:6152571894bdf39ae6670dbefbe9bf4d7473baa995f5bb01f1b3c976ab38d3a4

- Task research\-r3\-verify\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:3f2177f60a29fdb6d45e2840fa76cecccd6f2545649786e2ed44c3ad7667a1c8

- Task research\-r4 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:c9e7e44958b50347a88c401f50ee014231d952a264eb800fa4f90a45260aa0fa

- Task research\-r4\-verify\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:5928179c3883e65b07e3ec54372aca644d24f5b9a45cf10f165c544b80fcb043

- Task research\-r4\-verify\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:814649a78d2c6cd51b3c3f72096db22c50eea6b4b12f9b6d0074a2806be3455e

- Task research\-r5 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:6ad64e04c8d2de7119ad21ac9a9f5cda9294f6c5b15e7821f5c4b6bfd9255b62

- Task research\-r5\-verify\-1 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:b18d60022bd9cb16ed8644aba881a055874468dfb8461edee0b04511aefa614a

- Task research\-release\-build\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:91eae24c6dfe620879293d47dc91eb13a73c310091a5dfe43b79444fde761042

- Task research\-release\-build\-3 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:ef75b9874a28da9c0f851e300adf55617bafcb6853e7a86e2f10c80551cbaf1a

- Task research\-sdlc\-report\-2 — repo\:docs\/design\/research\-tasks\.sources\.json — revision sha256\:8d6cbddf34aa15adc7c3d55c69d8b27733c093ce4da46227e96d3794d7d4bae4

## Repository citations

- `.af/code-policy.toml:3`

- `.af/code-policy.toml:14`

- `.af/code-policy.toml:43`

- `.af/code-policy.toml:45`

- `.af/code-policy.toml:47`

- `.af/code-policy.toml:54`

- `.af/code-policy.toml:56`

- `.af/code-policy.toml:77`

- `.config/nextest.toml:4`

- `.github/workflows/release.yml:105`

- `.github/workflows/validate.yml:38`

- `Cargo.toml:20`

- `Makefile:3`

- `Makefile:4`

- `Makefile:5`

- `Makefile:8`

- `Makefile:20`

- `Makefile:23`

- `docs/design/research-pipelines.md:38`

- `docs/design/research-pipelines.md:39`

- `docs/design/research-pipelines.md:237`

- `docs/design/research-pipelines.md:582`

- `docs/design/research-pipelines.md:610`

- `docs/design/research-pipelines.md:671`

- `docs/design/research-pipelines.md:672`

- `docs/design/research-pipelines.md:849`

- `docs/design/research-pipelines.md:942`

- `docs/design/research-pipelines.md:948`

- `docs/design/research-tasks.sources.json:32`

- `docs/design/research-tasks.sources.json:44`

- `docs/design/research-tasks.sources.json:50`

- `docs/design/research-tasks.sources.json:56`

- `docs/design/research-tasks.sources.json:62`

- `docs/design/research-tasks.sources.json:68`

- `docs/design/research-tasks.sources.json:74`

- `docs/design/research-tasks.sources.json:80`

- `docs/design/research-tasks.sources.json:86`

- `docs/design/research-tasks.sources.json:92`

- `docs/design/research-tasks.sources.json:98`

- `docs/design/research-tasks.sources.json:104`

- `docs/design/research-tasks.sources.json:116`

- `docs/design/research-tasks.sources.json:122`

- `docs/design/research-tasks.sources.json:152`

- `docs/design/research-tasks.sources.json:158`

- `docs/design/research-tasks.sources.json:170`

- `docs/design/research-tasks.sources.json:176`

- `docs/design/research-tasks.sources.json:182`

- `docs/design/research-tasks.sources.json:188`

- `docs/design/research-tasks.sources.json:206`

- `docs/design/research-tasks.sources.json:212`

- `docs/design/research-tasks.sources.json:230`

- `docs/design/research-tasks.sources.json:236`

- `docs/design/research-tasks.sources.json:242`

- `docs/design/research-tasks.sources.json:248`

- `docs/design/research-tasks.sources.json:254`

- `docs/design/research-tasks.sources.json:266`

- `docs/design/research-tasks.sources.json:272`

- `docs/design/research-tasks.sources.json:284`

- `scripts/measure-release.sh:10`

- `scripts/verify.sh:20`
