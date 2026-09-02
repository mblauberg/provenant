# Claim discipline and evidence

`natural-writing` owns this evidence-classification schema for the writing
family. `engineering-writing` links here and adds only its own domain
extension (README feature-list scoping); the academic extension
(reproducibility statements, small-sample statistics, results claims) is in
[academic-prose.md](academic-prose.md). `legal-writing` uses a distinct,
non-overlapping schema: the Legal Function Test owned by the `legal-writing`
skill, because a legal document classifies
sentences by legal function (relief, contention, source-backed fact), not by
observed/inferred evidence strength. It does not link here for that reason.

## Claim classes

Before editing, classify each claim:

- `observed`: directly measured, implemented, run, or reproduced.
- `inferred`: supported by evidence but not directly measured.
- `designed` / `protocol`: what the code, contract, or methodology requires.
- `limitation`: states what was not measured, tested, or included.
- `planned` / `future work`: specified but not built or not run.
- `background`: supported by cited prior work.
- `pending`: placeholder, result macro, or unverified artefact.

Do not mix classes in one sentence if it creates ambiguity.

## Wording by evidence class

| Class | Safer wording |
| --- | --- |
| observed | `measured`, `recorded`, `observed`, `reported`, `reproduced` |
| inferred | `suggests`, `indicates`, `is consistent with`, `likely` |
| designed / protocol | `requires`, `checks`, `rejects`, `enforces`, `caps` |
| limitation | `does not establish`, `was not measured`, `remains untested` |
| planned / future work | `is left for future work`, `is intended to` |
| pending | `is pending`, `is conditional on`, `[FLAG: verify]` |

## The implicit-completion tense trap

Neutral present tense can silently assert that planned or pending work is
finished: `The service handles failover` or `The method is evaluated on the
held-out dataset` reads as shipped, completed evidence. Where the work is
unbuilt, untested, or unverified, scope the claim to the protocol (`the
design routes failover through the standby region`) or state the status
directly, and match tense to evidence.

## Comparatives carry their number

Ban bare `faster`, `better`, `improved`, `higher`, `more scalable` on their
own. Give the measurement and its conditions, or point to it:
`cold start dropped from 3.1 s to 0.9 s (M2, cache warm)`, not `startup is
much faster`. Disclose the interval or test used when a `significant` or
`excludes zero` claim is made; do not report a bare statistically
significant as a pass/fail verdict, and read a p-value as a measure of
compatibility, not a gate.

## Contribution and results claims

A contribution claim must name the contribution and its evidence:

Weak: `This work makes a novel and impactful contribution.`
Better: `This work contributes a calibrated forecasting method and
evaluates it against two declared comparators.`

Do not interpret a result before stating what it reports, and avoid
`superior`, `higher`, or `better` unless the metric, comparator, and scope
support it.

## Pending and pilot evidence

Keep pending evidence visibly pending; do not smooth it into final prose.
Pilot artefacts can support workflow readiness and interface checks; they do
not support final inferential performance unless explicitly defined as final
evidence.

## Limitations and future work

A good limitation is specific and tied to the claim it bounds:

Weak: `The study has some limitations that should be considered.`
Better: `The evaluation does not establish field performance because all
primary outcomes are measured in a controlled setting.`

Calibration runs both ways: do not over-hedge. State a real implication
plainly, reserve hedges for genuine uncertainty, and avoid stacked
qualifiers. Future work should be bounded and concrete, not `explore more
advanced approaches`.

## Claim audit checklist

- Is the claim observed, inferred, designed/protocol-bound, limited,
  planned, background, or pending?
- Does the verb match the evidence class?
- Does the sentence name the evaluation or deployment scope?
- Does the limitation bound the right claim?
- Does the contribution claim avoid unsupported novelty?
- Are pending or pilot artefacts kept out of final result claims?
