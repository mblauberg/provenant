---
name: deliver
description: "Use to verify and accept approved non-software outcomes (research, analysis, documents, agent work) and to write the outcome report every skill uses. Not for software changes (implement), unsettled scope (scope) or promotion (release)."
---

# Deliver

Non-software work has no CI to say it is done. This skill supplies the
evidence standard and the report that lets the owner accept or reject the
result without redoing it. Software goes through `implement`, which ends with
the same report.

## Entry

Require approved intent, acceptance criteria with how each is verified, a risk
tier and bounded authority. Unsettled intent goes to `scope`. A run that
needs a delivery record saves one with `provenant workflow receipt save ID
--expected-revision N --request FILE` and reads it with `provenant workflow
receipt show ID`.

## How it runs

- **Research or analysis:** `provenant run --preset scout-swarm --prompt-file
  Q.md --set 'parameters.external=1' --explain` to gather sourced claims, then
  the `plan.research` role to reconcile them. Every claim carries a source, a
  date and whether it was observed or inferred.
- **Documents:** a read-only fidelity check through `--preset single`,
  preferably on a different model family, that compares the document with its
  sources and criteria and lists unsupported claims, not stylistic edits.
- **Agent work products:** check the output against the criteria directly;
  produce deterministic evidence (a diff, a recomputation, a test) before
  judgement evidence. For stochastic behaviour use `evaluate`.

## Outcome report

The owner can reject it, so write it for a reader who will not open the
artefact. Lead with the decision needed, then:

1. What changed or was produced, and where it lives.
2. Each acceptance criterion with its evidence, or `unverified` and why.
3. Review coverage: who or what reviewed, from which model family, and what
   was not covered.
4. What was not verified, and the assumptions made on the owner's behalf.
5. Follow-ups, each as a tracker issue or a named owner.

Never fabricate approval from passing checks. A check that did not run is
reported as not run. Evidence outranks confidence.

## Acceptance

Objective checks plus the review the tier requires meet acceptance; the chair
accepts and reports, and the owner may reject. Explicit owner acceptance is
needed only for `terminal` work or when the approved scope asks for it.
Delegates may only narrow authority. Sending, publishing and deploying are
`release`. Keep the criteria, evidence and outcome in the existing issue or PR,
and move settled rules to their durable home (`engineering-docs`).
