---
name: scope
description: "Use when unsettled requirements or decisions need a spec, options, acceptance criteria, authority bounds, or decision record. Not for implementing approved scope or writing files during read-only advice; use implement."
---

# Scope

Turn an idea into decided, testable scope under project constraints.

## Frame

Define the decision, users and done. Search specs and registers. Emit the minimum
`config/risk-policy.json` tier plus allowed
source/artifact paths, prohibited actions, external effects, expiry and approver.
Ordinary authorised workspace content may cross configured provider families
without a family-separation gate. This is execution permission, not assurance:
credentials and authentication stores, secrets, unrelated paths, explicit
denials, platform rules and narrower task/project bans still control. Path and
write scopes, resource limits and external-action gates remain explicit.
Only a user may downgrade risk. Judgement-bearing AI,
ranking or heuristics require `evaluate`.

Preserve decision context: intake/revision, goals, constraints, alternatives, evidence and
decided/parked branches. Revise it; never fork competing scope. Record
disclosure in the authority envelope, never private memory.

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

Research surviving questions; use `orchestrate` for fan-out and source retained
claims. Use `prototype` when a timeboxed
throwaway answers feasibility; harvest its result and delete/quarantine only
manifest-owned scratch under its authority. Neither lane exists for curiosity.

For viable options, compare cost, reversibility, risk and prior
decision fit through correctness/cost/operations lenses. Use independent
reviewers when available; recommend one. Put costly-to-reverse choices and
rejected alternatives in an ADR, reversible detail in the spec/story.
Paired-primary mode has one chair ask while the peer audits evidence; record
authorship for later independence.

## Land outputs

First resolve artifact authority and canonical owners. In advisory/read-only
mode, return proposed scope and named open decisions in chat; do not change
project files. In project-write mode, land only approved artifacts:

| Output | Owner |
|---|---|
| Spec, stories, acceptance criteria | project docs via `engineering-docs` |
| One-way decisions | project ADR process |
| User gates | existing register or `docs/OPEN_DECISIONS.md` |
| Work items | project tracker |
| Durable context | project context/state owners |

Write clear, observable, verifiable acceptance criteria. Given/When/Then helps
behavioural cases but is not mandatory for research, documents or operations.
Preserve project Markdown/YAML/JSON schema. Pin only decision-critical external
interfaces through project-native locks/constraints. Link or cache permitted
authoritative material with source, version/date and digest; never vendor
without licence and redistribution authority.

Before handoff: branches decided/parked; exclusions/failure modes;
machine-readable authority/risk; anchored evidence; user approval of spec and
one-way doors. Execution handoff is digest-bound to approved scope,
decisions and authority; change creates a new revision and gate.

## Adapter-absent path

Without optional Console, Herdr or GitHub, use canonical project artifacts and
emit the skill-owned kind in
[portable-workflow.v1.json](portable-workflow.v1.json). The runner validates
declared fields, including `accepted_artifact_identity`. Output proves context
only, not scope evidence or approval.
