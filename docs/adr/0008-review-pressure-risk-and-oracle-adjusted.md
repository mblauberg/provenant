# ADR 0008 — Risk/oracle-adjusted certifying review

**Status:** Accepted 2026-07-13 (human, scoping round 7); unimplemented
review-policy follow-ups superseded 2026-07-15 (human, [issue
#97](https://github.com/mblauberg/provenant/issues/97)); implementation
identifiers amended to descriptive owners 2026-07-16 ([issue
#135](https://github.com/mblauberg/provenant/issues/135)).

## Context

`HARNESS.md` currently requires fresh native and other-primary review for
substantial work, then additional bonus-family attempts for crucial and
terminal work. Earlier implementation identifiers inherited a milestone
codename and obscured their durable owners.

### Addendum — 2026-07-22, issue #361

The context above records the earlier policy and remains unchanged. The live
policy now resides in `HARNESS.md`: substantial work uses multiple targeted
lenses plus required other-primary review; distinct-family review is used when
available and never substitutes for other-primary; terminal work adds stronger
targeted and adversarial pressure and records a skipped distinct-family leg. The
delivery and orchestration validators share one ladder checker and no longer
count fixed optional-provider attempts.

### Addendum — 2026-10-21, owner decision

The earlier addenda retain the policy at their dates. The owner has adopted a
lighter default ladder in `HARNESS.md`: routine work uses the chair and objective
checks; substantial work adds one passing independent review from another
provider family when available; crucial work needs two passing independent reviews
with distinct lenses and two non-chair families when available. Terminal work
adds an adversarial lens to crucial coverage. Each missing non-chair family needs
its own unavailable or omitted leg with a non-blank reason and a passing
same-family replacement. A blocker for the chair's family or an already passing
family does not cover a missing family. Every passing review needs a non-blank
lens and a distinct reviewer. Failed reviews supply neither coverage nor blocker
records. Receipt roles no longer determine ladder coverage.

Projects may set their own ladders in their contributing guides while retaining
the constitution's minimum independence. Objective checks and the required review
meet acceptance unless the scope requires explicit user acceptance; terminal work
still requires the owner's acceptance before merge. A project may name its
landing gate as acceptance.

## Decision

Certifying review pressure follows the current `HARNESS.md` risk table.
Exceptions and degradations are recorded explicitly. Deterministic oracles stay
primary; findings block on evidence, never votes.

Companion round-7 adoptions: decision packets in `/scope` for broad work
(one-question-at-a-time retained for dependent choices);
the unimplemented review amendment and `architecture-review` skill proposal
are superseded. No replacement skill or specification is created.

Style policy: terse output stays default for inter-agent/mechanical/status
traffic; human-facing explanatory output is domain-appropriate. The `caveman`
overlay loads only on explicit request, avoiding a duplicate skill load.
The `caveman` skill was removed on 2026-10-21 at the owner's request; the terse
inter-agent default stands without it.

Current implementation identifiers name their owners directly: certifying
review owns the four-slot profile, Console acceptance owns the requirement and
evidence catalogues, Agent Fabric owns its system tests, and lifecycle-skill
alignment owns its focused fixtures and deterministic portability evaluation.
The cutover is direct; no old-name aliases or compatibility path exists.

## Consequences

- The current `HARNESS.md` risk-based, cross-family routing remains governing.
  Deterministic oracles remain primary.
- No `architecture-review` skill or replacement framework is created. The
  milestone-derived identifiers are retired without compatibility aliases.
- Reviewer yield/cost tracking informs later calibration.
