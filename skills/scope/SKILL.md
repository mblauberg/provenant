---
name: scope
description: "Use when unsettled requirements or decisions need a spec, options, acceptance criteria, authority bounds, or decision record. Not for implementing approved scope or writing files during read-only advice; use implement."
---

# Scope

Produce testable scope.

## Frame

Read the repository's `Repository process` declaration. If absent or ambiguous,
use an unambiguous existing owner or return proposed content in chat; never
invent an owner. Define the decision, users and done there. Emit the minimum
tier from `HARNESS.md`, path/write/resource limits, prohibited actions,
external effects, disclosure, expiry and approver. Credentials, secrets,
unrelated paths, denials and project bans stay outside authority. Execution
permission is not assurance. Ordinary authorised workspace content proceeds
without a family-separation gate. Only a user may downgrade risk.
Judgement-bearing AI requires `evaluate`.

Writing a tracker needs explicit external-write authority; project-write
authority alone does not grant it.

Scope owns accepted content and the decision register. Record disclosure in
the authority envelope, never private memory.

## Grill

Load `grill-me` when requested or dependent owner decisions remain materially
unresolved. Otherwise present two or three choices, a recommendation and parked
calls.

Agents decide engineering calls. Business, legal or financial owner calls stay
parked as open-decision rows, never guessed, unless the owner explicitly authorises assume-and-record for reversible
calls; record those for confirmation. A blocking one-way door may use an
owner-authorised decision process only with a recorded answer, cheap reversal
until confirmed and owner confirmation before activation. Irreversible actions,
promotion and live activation stay gated. Record every unresolved branch.

## Resolve uncertainty

Research surviving questions through `orchestrate`; use `prototype` only for a
timeboxed feasibility answer, then harvest it and clean only manifest-owned
scratch.

Compare correctness, cost and operations. Put architectural one-way
decisions and rejected alternatives in an ADR; product calls and deferrals go to
the owning issue; reversible detail stays in the spec. In
paired-primary mode one chair asks; the peer audits. Record authorship.

## Land outputs

In read-only mode, return proposals in chat without changing project files.
Otherwise land only approved content with its declared owner: change scope,
stories, criteria and user gates in the issue body or project-docs home,
architectural one-way decisions in ADRs, product calls and deferrals in the
owning issue, work items in the tracker, surviving invariants and
context in project docs via `engineering-docs`. Run state stays run-local.

For an issue-tracker repository, the change spec lives in the issue body and
expires with the delivering PR. Keep no delta archive. Size it to the change:
a small change needs a goal, two to five observable criteria and non-goals.
Tag each criterion `[test]`, `[command output]`, `[review]` or `[observation]`.
Name must-not-change behaviour. For risky work, a reviewer retains one held-back
check outside the writer's context. State the post-delivery fate: nothing, or a
named invariant and the durable document it moves to.

Preserve project schemas; pin only decision-critical interfaces with native
locks/constraints. Link or cache authority with its source and date or
version; vendor only with redistribution permission. Name the objective
checks, tier-required review and any project landing gate.
Spec drift is a reviewer check at acceptance, never an automatic gate. Explicit
user acceptance is required only for `terminal` work or when scoped; the chair
reports the outcome and the user may still reject it.

Tracked artifacts, including issues, PRs, docs and code comments, never cite
session scratch: run files, plan codes or local scratch paths. Put the relevant
evidence in the artifact or cite a resolvable project source instead.

Never leave a "Spec gap:" line. Give the gap a scope or research lane that
decides and records it in the body ("Decided (date): rationale"). Only an
owner-only question becomes a Decision issue with options and a recommendation.
For new scoping, later epics hold a `## Backlog` checklist and write decisions,
not leaf briefs; a leaf gets a full brief when leased or Ready. Keep existing
well-scoped leaves, even under Next or Later epics.

Before approval, consider an advisory fresh-reader check; skip trivial items.
A zero-context sub-agent reads only the spec and returns the goal, likely files,
test plan and every open decision. Answer its substantive questions in the spec
before approval; dismiss noise explicitly. This advice cannot automatically fail
readiness. Use the [dispatch recipe](references/fresh-reader.md).

Before handoff, record decided/parked branches, exclusions, risk/authority and
anchored evidence, with user approval of the spec and one-way doors. The
execution handoff is digest-bound; any change creates a new revision and gate.

## Portable artifact

Keep criteria, evidence and outcome in the declared issue body or project-docs
home; a PR links that owner. Delivery ends the change spec's authority. Promote
only its named surviving invariant; do not copy the spec into a durable archive.

Reference eval cases (not run by CI): [lifecycle cases](evals/lifecycle_cases.yaml).
Behaviour fixtures: [spec cases](evals/spec_cases.yaml) and
[fresh-reader evaluation](evals/fresh-reader/README.md).
