---
name: diagnose
description: "Use to reproduce and find the root cause of broken behaviour, crashes, test failures, flakes, or performance regressions. Not for a known-cause repair or change review; use implement or code-review."
---

# Diagnose

Build a mental model and check recorded constraints before probing. Work in
order; record why any phase is skipped.

## Iron law

```text
NO FIX WITHOUT A REPRODUCTION AND A ROOT CAUSE FIRST
```

Diagnosis is read-only except temporary instrumentation. Permanent repair
requires an explicit user request or an authorised `implement` scope.

## Workflow

1. **Feedback loop.** Create the fastest sharp signal that reaches the reported
   failure: failing test, CLI/HTTP/browser fixture, replay, property loop,
   bisection, differential harness or structured user loop. Pin time/randomness
   and isolate mutable dependencies. For flakes, raise the reproduction rate.
   If no useful loop is possible, stop and request the missing environment or
   artifact; do not guess.
2. **Reproduce.** Confirm repeated runs show the user's exact symptom, not a
   nearby failure. Capture the observation needed to prove later repair.
3. **Hypothesise.** Rank 3–5 falsifiable causes before testing. Each needs a
   distinct prediction; inspect recent diffs, deploys and dependency changes.
4. **Instrument.** Map one probe to one prediction and vary one thing at a
   time. Prefer debugger/breakpoints; tag targeted temporary logs uniquely.
   Performance work starts with baseline, profile and bisect.
5. **Handoff or fix.** Without repair authority, stop with root cause,
   confidence, affected paths and the proposed regression seam. With authority,
   first turn the repro into a failing test at the real call-site seam, then
   make one root-cause fix and rerun the regression, original loop and suite.
   An absent correct seam is an architectural finding, not permission for a
   shallow test.
6. **Clean up.** Remove only instrumentation/harnesses proven run-owned and
   covered by authority; preserve unknown, pre-existing and user-authored work.
   Record the winning hypothesis and prevention opportunity. Never bundle
   unrelated refactoring.

After three failed fix attempts, stop and question the architecture with the
user. An enclosing lifecycle's stricter repair-cycle cap always wins.
Detailed loop choices and cleanup checks: [references/method.md](references/method.md).
For a guided user reproduction, adapt the
[human-in-the-loop script template](scripts/hitl-loop.template.sh).
