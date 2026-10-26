---
name: code-review
description: "Use for read-only source review of PRs, diffs, commits, migrations and refactors through dependencies. Not for root cause (diagnose), fixes (implement) or browser/UX review (ui-ux-design)."
---

# Code review

Review is source-read-only unless fixes are separately authorised. Write
findings and traces only to the assigned artifact directory. Trace beyond the
diff and judge the resulting system.

## Review

1. Read project instructions and preserve the requested scope and output
   contract. Briefs carry the target and rubric, never the hypothesis or
   verdict; see [review outcome](references/finding-contract.md#review-outcome).
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
5. Follow the `HARNESS.md` Review ladder: routine needs chair and checks;
   substantial adds one independent review; crucial needs two distinct lenses;
   terminal adds an adversarial lens. `orchestrate` owns families/scheduling.
   Record unavailable cross-family coverage. Councils challenge anonymised
   claims before fresh reduction; never rank prose or majority-vote.
   `provenant config show` sets `review.*` above that floor.
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
   defect exists and required review coverage is complete.

Load [review-lenses.md](references/review-lenses.md) for the detailed inspection
questions, [multi-agent-review.md](references/multi-agent-review.md) for lens
fan-out/council reduction, and [finding-contract.md](references/finding-contract.md)
before writing findings.

## Boundaries

- `orchestrate` chooses reviewer topology, families, and waves. This skill
  defines what each reviewer inspects and the independence/reduction contract.
- `diagnose` owns reproduction and root cause for known broken behaviour.
- `implement` owns authorised fixes and bounded re-review; `tdd` or `refactor`
  may supply the method. Do not mutate source or Git state during review: no
  Git write verbs (`orchestrate` lists them); read other refs with `git show`.
- Artifact-only authority permits assigned run outputs, not repo-root scratch.
  Without a run directory, use the session scratchpad. Bound captures; never
  redirect over a wildcard/list that could include its own output.
- Domain/project skills supply specialised lenses. For rendered-UI changes,
  inspect light and dark desktop captures of changed surfaces before the
  verdict (`ui-ux-design`).
- Before aligning a looser validator with a stricter one, search tests for the
  permissive values; they may be a designed route into a rejection path.
- A structural alternative blocks only when tied to a defect or
  material regression with a safer, validated design. Attractive redesign
  alone is not a finding.

Reference eval cases (not run by CI): [discipline cases](evals/discipline_cases.yaml).
