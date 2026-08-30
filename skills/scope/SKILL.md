---
name: scope
description: "Use when unsettled requirements or decisions need a spec, options, acceptance criteria, authority bounds, or decision record. Not for implementing approved scope or writing files during read-only advice; use implement."
---

# Scope

Turn an idea into decided, testable scope under project constraints.

## Frame

Read the target repository's `Repository process` declaration. Define the
decision, users and done; put change scope and story in its declared home: the
parent tracker issue for `issue-tracker`, or named project docs for
`project-docs`. Emit the minimum `config/risk-policy.json` tier, path bounds,
prohibited actions, external effects, expiry and approver. Credentials,
secrets, unrelated paths, denials and project bans remain outside authority;
path, write and resource limits stay explicit. Ordinary authorised workspace content
is ordinary execution without a family-separation gate; this is execution
permission, not assurance.
Only a user may downgrade risk. Judgement-bearing AI,
ranking or heuristics require `evaluate`.

Scope owns accepted content and the decision register: retain intake, goals,
constraints, alternatives and decided/parked branches without forking scope.
Record disclosure in the authority envelope, never private memory.

## Grill

Load `grill-me` only when the user explicitly asks to be grilled or dependent
owner decisions remain materially unresolved. Then work purpose -> users ->
constraints -> edge cases -> failure modes -> success -> exclusions, one
question per round. Otherwise present a compact decision packet with 2–3
choices, a recommendation and parked owner calls.

Agents decide engineering calls. Business, legal or financial owner calls stay
parked as named open-decision rows; never guess. Put every unresolved branch in
the spec.

## Resolve uncertainty

Research surviving questions through `orchestrate`; use `prototype` only for a
timeboxed feasibility answer, then harvest it and clean only manifest-owned
scratch. Neither lane exists for curiosity.

For viable options, compare cost, reversibility, risk and prior fit through
correctness, cost and operations lenses. Use independent reviewers when
available. Put costly-to-reverse choices and rejected alternatives in an ADR;
keep reversible detail in the spec/story.
Paired-primary mode has one chair ask while the peer audits evidence; record
authorship for later independence.

## Land outputs

First resolve artifact authority and canonical owners. In advisory/read-only
mode, return proposed scope and named open decisions in chat; do not change
project files. In project-write mode, land only approved artifacts:

| Output | Owner |
|---|---|
| Durable specifications and acceptance criteria | project docs via `engineering-docs` |
| One-way decisions | project ADR process |
| User gates | existing register or `docs/OPEN_DECISIONS.md` |
| Change scope and stories | declared issue tracker or project-docs home |
| Work items | declared project tracker |
| Durable context | declared project-docs owner; run state stays run-local |

Write clear, observable acceptance criteria. Preserve project schemas; pin only
decision-critical interfaces with native locks/constraints. Link or cache
authority with source, version/date and digest; never vendor without a licence.

Before handoff, record decided/parked branches, exclusions, machine-readable
risk/authority and anchored evidence, with user approval of the spec and
one-way doors. The execution handoff is digest-bound; any change creates a new
revision and gate.

## Adapter-absent path

Without optional Console, Herdr or GitHub, use canonical project artifacts and
emit the skill-owned kind in
[portable-workflow.v1.json](portable-workflow.v1.json). The runner validates
declared fields, including `accepted_artifact_identity`. Output proves context
only, not scope evidence or approval.
