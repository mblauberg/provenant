# Evaluation-run receipt

`EVALUATION.json` is evidence, not a second delivery lifecycle. It records a
machine evaluation from frozen plan to result. The enclosing `delivery-run`
owns final user acceptance and release.

## Status

| Status | Meaning | Can satisfy assurance? |
|---|---|---:|
| `planned` | Protocol frozen; no execution rows | no |
| `running` | Partial retained execution | no |
| `pass` | Complete rows meet every machine gate | yes |
| `fail` | Complete evidence fails a declared gate | no |
| `incomplete` | Complete accounting exposes unusable coverage | no |
| `cancelled` | Schedule is conserved as skipped/cancelled evidence | no |

`conclusion.machine_only` is always `true`. Acceptance fields are invalid. For a
gating run, delivery records the evaluation ID, plan digest and delivery run ID
before execution; the evaluator cannot create that anchor after seeing results.

## Bound evidence

Every artifact has a safe relative POSIX path, media type, SHA-256, owner,
retention and data policy. The validator rejects absolute paths, traversal,
symlink escape and, with `--verify-hashes`, missing or changed bytes. Raw prompts,
outputs and failure examples stay in retained evidence artifacts rather than the
hot receipt.

The plan binds:

- dataset ID/version, provenance, consent or licence, development/holdout split
  digests and leakage boundary;
- treatment manifests and configuration digests;
- a shared runtime digest plus paired inputs/seeds so comparator arms differ
  only in their declared treatment;
- cases, families, repetitions, paired seeds, shards, timeout, retry and
  exclusion policies;
- deterministic preflight requirements;
- metric ranges, aggregation, threshold, target/comparator arms and regression
  margin; and
- required graders, blinding, independence, self-judging prohibition and
  disagreement protocol.

For `kind: skill-quality`, roles are `candidate`, `without` and `previous`.
`candidate` and `without` are required. A genuinely new skill records
`previous` as `not-applicable` with a reason; it never aliases it to `without`.

## Conservation and lineage

One base attempt exists for every active arm x family x repetition x shard.
Retries reference an earlier failed attempt with the same cell and remain in the
receipt. Each attempt records timestamps, seed, input/output/route artifacts,
terminal status, usage or its unavailability, and requested plus actual adapter,
endpoint, provider family, model and effort. Substitution needs a reason.
Every attempt repeats the frozen plan, shared-runtime, arm-manifest and
arm-configuration digests. Paired arms keep input and undeclared runtime
dimensions identical; an intentional runtime treatment is declared in the plan.
Preflight begins only after the plan anchor. Attempt duration must agree with the
frozen timeout, and a timed-out leg links timer evidence.

Each attempt accounts for every case in its shard. Terminal case states are
`pass`, `fail`, `omitted`, `skipped`, `excluded`, `timed-out`, `invalid` and
`tool-error`. `results.accounting` must exactly match retained rows. Passing
receipts cannot hide a non-semantic state or critical candidate failure.
Separate `attempt_accounting` preserves provider-run reliability: base attempts
plus retained retries and every terminal attempt state must also conserve.

Required graders judge every semantic row. A model grader cannot judge output
from its own provider family. Differing required judgements need a fresh
adjudicator; unresolved disagreement blocks `pass`. Metric values,
numerators/denominators, comparator deltas and non-inferiority decisions are
recomputed from retained scores.

## Validation

There is no receipt validator. The consumer reads the receipt itself and
confirms, in order: the artifact's own SHA-256 matches the digest its binding
records; `evaluation_id`, `plan.digest` and the enclosing delivery run ID match
the anchors frozen before execution; every linked artifact digest matches its
live bytes; accounting conserves against retained rows; and `status` is `pass`
only where every applicable threshold and margin held.

A clean reading proves only a machine pass for the canonical receipt. Before execution the
consumer anchors evaluation ID, plan digest and delivery run ID. Afterwards it
binds the evaluation artifact's own SHA-256, loads it, requires
`contract: evaluation-run` and `schema_version: 2`, and reads the receipt
against the checks above. It must not trust copied dataset, threshold or
summary fields.

Unsupported receipts lack sufficient lineage and provenance to migrate
truthfully. Preserve them as historical evidence and rerun from a fresh frozen
plan.
