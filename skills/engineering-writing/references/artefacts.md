# Artefact shapes

Match the shape to the reader's job. Link the canonical owner instead of
restating it; duplicated detail drifts. Placement of durable docs is
`engineering-docs`.

## README

A landing page for a capable reader who does not know the project. Answer in
this order: what is it (one plain sentence); why use it (concrete outcomes);
what do I need and how do I start (prerequisites before the first command);
what are the limits; where is the detail. Quick start states audience,
prerequisites, essential steps, a separate verify step and the expected result.
Give exact supported versions (`Node.js >=24.15.0 <25`, not `Node.js 24`). Keep
status, roadmap and changelog material out unless the README is the
current-state authority. A shorter README must keep every load-bearing fact
(versions, commands, security-reporting path, generated blocks) verbatim.

## Pull request

Problem, change, impact, tests run (with results), what was not verified,
follow-ups. Report net tests added and removed. Tie implementation detail to
review value; the reader may be an agent with a clean context and no memory of
your session.

## Commit message

Follow the repository convention. Subject specific, imperative, normally under
50 characters; a body only when the reason is not obvious or the change is
breaking, security-sensitive, a migration or a revert. Preserve required issue
and attribution trailers exactly.

## Error message

What failed, why if known, what to do next. No blame, no vague apology.

## Bug report

One line with symptom and condition. Then environment, minimal numbered
reproduction from a clean state, expected, actual (exact error text), impact.
Keep observation separate from diagnosis; a suspected cause goes under a marked
hypothesis, and the summary never proposes a fix. Search for duplicates first.

## Runbook

For the on-call reader with least context. Trigger, preconditions, numbered
steps with copy-pasteable commands and expected output, verification, rollback,
escalation. No unexplained judgement: `if depth > 10,000 for 5 minutes`, not
`if the queue looks bad`. Test it by having someone else follow it verbatim.

## Postmortem

Blameless: systems and decisions, not culprits. Summary, measured impact,
forward timeline with timezone from logs (not memory), trigger versus root
cause, what went well and poorly, action items each with owner and link. Do not
soften impact numbers.

## Migration or deprecation note

Who must act and who can ignore it; before and after per breaking change;
ordered steps; how to verify; rollback. A deprecation names the exact
identifier and version, the replacement and the removal condition.

## Comments and docstrings

Comments carry intent, invariants, side effects and traps, not syntax.
Docstrings carry contract, errors and gotchas. Keep the repository's density.

## Reports

Lead with the finding, then evidence, impact, uncertainty and next action. Use
exact dates, versions and sample sizes. Findings come before summary; if there
are none, say so and name the residual risk.
