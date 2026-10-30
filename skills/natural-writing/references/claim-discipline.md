# Claim discipline and evidence

`natural-writing` owns the evidence-classification schema for the writing
family: claim classes, wording by class, comparatives, results and
contribution claims, limitations and pending evidence. `engineering-writing`
links here. [academic-prose.md](academic-prose.md) adds only what is specific
to papers and theses (small-sample statistics, reproducibility statements).

`legal-writing` doesn't link here. It uses a distinct, non-overlapping schema,
the Legal Function Test, because a legal document classifies sentences by
legal function (relief, contention, source-backed fact), not by strength of
evidence.

## Claim classes and wording

Classify each claim before editing, then pick a verb that matches its class.

| Class | Means | Safer wording |
| --- | --- | --- |
| `observed` | Directly measured, implemented, run or reproduced | `measured`, `recorded`, `observed`, `reported`, `reproduced` |
| `inferred` | Supported by evidence, not directly measured | `suggests`, `indicates`, `is consistent with`, `likely` |
| `designed` / `protocol` | What the code, contract or methodology requires | `requires`, `checks`, `rejects`, `enforces`, `caps` |
| `limitation` | What was not measured, tested or included | `does not establish`, `was not measured`, `remains untested` |
| `planned` / `future work` | Specified but not built or not run | `is left for future work`, `is intended to` |
| `background` | Supported by cited prior work | cite it |
| `pending` | Placeholder, result macro or unverified artefact | `is pending`, `is conditional on`, `[FLAG: verify]` |

Don't mix classes in one sentence if it creates ambiguity. Hold every claim at its class: `suggests` stays `suggests` and never
becomes `proves`; a pilot never becomes final evidence; a specified extension
never becomes a completed result.

## The implicit-completion tense trap

Plain present tense can silently assert that planned or pending work is
finished. `The service handles failover` and `The method is evaluated on the
held-out dataset` both read as shipped, completed evidence. Where the work is
unbuilt, untested or unverified, scope the claim to the protocol (`the design
routes failover through the standby region`) or state the status directly.
Match tense to evidence.

## Comparatives carry their number

Never use a bare `faster`, `better`, `improved`, `higher` or `more scalable`:
it gives the reader nothing to check. Give the measurement and its conditions, or point to
them: `cold start dropped from 3.1 s to 0.9 s (M2, cache warm)`, not `startup
is much faster`.

When you claim `significant` or `excludes zero`, name the interval or test
used. Don't report a bare `statistically significant` as a pass/fail verdict,
and read a p-value as a measure of compatibility, not a gate.

## Contribution and results claims

Name the contribution and its evidence.

- Weak: `This work makes a novel and impactful contribution.`
- Better: `This work contributes a calibrated forecasting method and evaluates
  it against two declared comparators.`

State what a result reports before interpreting it. Use `superior`, `higher`
or `better` only when the metric, comparator, interval and scope support it.

## Pending and pilot evidence

Keep pending evidence visibly pending instead of smoothing it into final
prose. Pilot artefacts can support workflow readiness and interface checks.
They support final inferential performance only when explicitly defined as
final evidence.

## Limitations and future work

A good limitation is specific and tied to the claim it bounds.

- Weak: `The study has some limitations that should be considered.`
- Better: `The evaluation does not establish field performance because all
  primary outcomes are measured in a controlled setting.`

Calibration runs both ways. State a real implication plainly, reserve hedges
for genuine uncertainty and use one qualifier, not a stack (the hedging
mechanism is in [mechanisms.md](mechanisms.md)). Bound future work and make it
concrete: not `explore more advanced approaches`.

## Claim audit checklist

- Is the claim observed, inferred, designed/protocol-bound, limited, planned,
  background or pending?
- Does the verb match the class?
- Does the sentence name the evaluation or deployment scope?
- Does the limitation bound the right claim?
- Does the contribution claim avoid unsupported novelty?
- Are pending and pilot artefacts kept out of final result claims?

## Mechanised audit

For a number-heavy document, a style pass is where a figure drifts or a claim
loses its qualifier, and a reread misses it. Keep a script that holds each
number-bearing sentence as a literal, recomputes its values from the source
data and fails when either no longer matches. Rerun it after every pass. A
failure on reworded text is the audit working: update the literal only after
confirming the number and claim survived.
