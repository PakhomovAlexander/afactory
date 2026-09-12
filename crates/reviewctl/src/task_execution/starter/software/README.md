# Software Task starter

Review the generated policy, definitions, Workers and captured contract fixtures, then initialize
this directory as a Git repository and commit it. The Python command substitutes support the
structured pagination specification in the supplied Task files. They perform no paid model calls.

```sh
git init
git add .
git commit -m 'Configure software Task starters'
af catalog test --source . --json
af task plan --file implementation-reviewed.json --json
af task run implementation-reviewed --json
```

The Task implements pagination, seals its output and calls the shared Review Pipeline with
independent correctness and bounds reviewers. A final independent evaluator checks the exact
Task requirements on the selected Snapshot using the same current check receipts. Both Review
and goal acceptance must pass; the reviewed path uses five Attempts.
The source checkout stays unchanged. To inspect the result in a new worktree:

```sh
af task deliver implementation-reviewed --branch pagination-result \
  --worktree ../pagination-result --confirm implementation-reviewed
```

`implementation-small.json` uses checks and an independent evaluator. The heavy implementation
allows one additional author retry. `implementation-repair-targeted.json` permits one repair and
targeted fix verification; `implementation-repair-heavy.json` requires full discovery on repaired
S2 as well. The default author implements the complete tutorial goal, so repair activates only
when an actual finding is present. All possible verification capacity is reserved in advance.

`review-light.json` and `review-heavy.json` use the same Review definition that implementation
embeds. Light declares one discovery Round; heavy declares two. Running them on the unfinished
initial source cannot approve it. Both reviewers are required in every Round. A missing reviewer
is incomplete, and a recorded negative result remains negative.

The separate verification Pipeline and reusable Planner Worker have public input/output contracts.
The fixed Task preparation plan invokes the packaged Planner Worker. To enable the planning tutorial,
create the starter with `--developer-public-key` pointing to an existing minisign public key.
That key is assigned the developer label `owner`; the signing key stays outside Afactory.

```sh
af task start --file planning.json --json
af task explain generated-pagination --json
af task decision-payload generated-pagination --developer owner --decision approved \
  --reason 'Reviewed the generated stages and verification' --output approval.payload
# Sign approval.payload externally with your existing key, producing approval.minisig.
af task approve generated-pagination --payload approval.payload --signature approval.minisig
af task run generated-pagination --json
af task export generated-pagination --name team/pagination --destination exported --json
```

The supplied no-fit variation changes the `standard` fact. The bounded command Planner returns
a definition assembled from supported typed interfaces, and execution waits for exact signed
developer approval. A fitting Task uses its shared definition without a Planner call. The
generated example has a fifteen-minute total deadline including approval waiting and shares all
planning and execution charges; editing the live catalog cannot change a captured plan.

These Workers are tutorial substitutes, with explicit supported requirements and claim types.
Configure independent shared or local model Workers with the same contracts for broader work.
The captured default uses trusted-local isolation; enabling `require_container` requires supported
container admission. These deterministic examples establish contract behavior, not model speed
or cost claims. Review changed policy and recompute actual package pins before committing it.
