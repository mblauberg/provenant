# Lifecycle routing baseline

Date: 10 July 2026
Dataset: `evals/lifecycle-routing.yaml`
Base revision: `d864ca3d8c6144cab2e0a632f2cb877b9baa4c50`

## Decision

Add the public `deliver` entry skill for approved cross-domain outcomes.
Retain `implement` as the software entrypoint.

The pre-change catalogue had no end-to-end route for approved research,
analysis, document or agent-product delivery. `scope` stopped after the
contract; specialist skills owned methods; `implement` correctly excluded
non-software work. The new trigger closes that gap without displacing the
adjacent software, scoping or release routes.

The name is short, action-led and parallel to `scope`, `implement` and
`release`. The human instructed the approved specification to be implemented
entirely; once the specification's routing condition passed, that instruction
authorised selection of its named `deliver` entrypoint. Final lifecycle
acceptance remains a separate human gate.

## Method

Fable 5 launched three blind fresh-context Claude subagent trials per snapshot.
Each trial classified all 15 held-out prompts using only skill names and
frontmatter descriptions available in that snapshot. The dataset contains five
profile-positive, five negative and five boundary cases. Expected labels were
withheld from classifiers. Three hash-bound batch invocations per snapshot
retain actual adapter/family/model lineage, input digests and parsed outputs.
Both canonical receipts passed the lifecycle-routing validator used at the
time; this historical evaluation is not a current gate.

| Snapshot | Correct | Total | Rate | Result |
|---|---:|---:|---:|---|
| Base catalogue, no `deliver` | 30 | 45 | 66.7% | Descriptive baseline |
| Working catalogue with `deliver` | 45 | 45 | 100% | Passes 90% selection gate |

All 15 baseline misses were the five cross-domain delivery cases repeated in
each trial. Post-change, software remained on `implement`, unsettled intent on
`scope`, promotion on `release`, and all cross-domain cases moved to `deliver`.

## Limits and refresh

The repeated trials are Claude-family only and showed identical selections,
so they demonstrate trigger separation more strongly than linguistic breadth.
Independent Codex corroboration and final cross-family review are recorded in
the HREF-002 run. Future datasets should add paraphrases from privacy-reviewed
misroutes. Refresh after a material description rename, new lifecycle skill,
or model/harness routing change.
