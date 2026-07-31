---
name: code-review
description: "Use for source-read-only review of a PR, diff, commit, migration, refactor, or implementation through its dependency cone. Not for root-cause diagnosis, authorised fixes, or browser/UX review; use diagnose, implement, or ui-ux-design."
---

# Code review

Review is source-read-only unless the user separately authorises fixes. A
reviewer may write compressed findings and traces only to an assigned
artifact directory. The diff is the entry point, not the boundary. Judge the
resulting system, not the changed lines alone.

## Review

1. Read project instructions and preserve the requested scope and output
   contract.
2. Establish the change intent from the spec, issue, tests, commit message, or
   user request. Mark unclear intent instead of inventing it.
3. Trace the affected dependency cone: full touched files, live callers/
   consumers, exports, canonical owners, tests, configuration, migrations,
   persistence boundaries, dependency/lock changes, and generated artefacts.
   Record inspected/excluded surfaces; unavailable context stays `unknown`.
4. Select review lenses from the task/risk profile. Correctness/spec alignment
   is mandatory; add security/privacy, data/concurrency,
   performance/reliability, tests, architecture, readability, UX/accessibility
   or operations only when activated by the dependency cone.
5. For substantial+ or wide-surface work, use independent targeted agents with
   distinct primary lenses and deliberate overlap on the riskiest invariant.
   `orchestrate` adds the required other-primary and distinct-family review
   pressure from `HARNESS.md`. Then
   run an anonymised claim challenge and a fresh reduction.
   Never rank prose or majority-vote findings.
6. Review the design delta. Look for duplicated ownership, scattered flags,
   nullable modes, casts hiding invariants, thin wrappers, parallel flows, and
   abstractions adding concepts without reducing complexity.
7. Ask whether a proven reframe could delete branches, state, layers, or
   duplicated flows. More than 1,000 lines warrants inspection, not a finding.
   Report a concrete cohesion, ownership, reviewability or change-risk defect.
8. Check tests and verification against the acceptance criteria. Confirm the
   trajectory: relevant deterministic checks ran and their results are
   available. Never infer coverage from a green summary alone.
9. Report only high-confidence, actionable findings. Return `clean` when no
   genuine defect exists.

Load [review-lenses.md](references/review-lenses.md) for the detailed inspection
questions, [multi-agent-review.md](references/multi-agent-review.md) for lens
fan-out/council reduction, and [finding-contract.md](references/finding-contract.md)
before writing findings.

## Boundaries

- `orchestrate` chooses reviewer topology, families, and waves. This skill
  defines what each reviewer inspects and the independence/reduction contract.
- `diagnose` owns reproduction and root cause for known broken behaviour.
- `implement` owns authorised fixes and bounded re-review; `tdd` or `refactor`
  may supply the method. Do not mutate source during review.
- Artifact-only authority permits named outputs under the assigned run
  directory; it does not permit arbitrary repo-root scratch. Use the system
  temporary directory when no run directory exists. Never redirect a command
  over a wildcard/list that can include its own growing output; keep
  captures bounded.
- Language, framework, UI, security, and project skills add specialised lenses;
  do not repeat their doctrine here.
- A structural alternative blocks only when tied to a present defect or
  material regression with a safer, validated design. Attractive redesign
  alone is not a finding.
