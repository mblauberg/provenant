## Summary

<!-- Two or three plain sentences: what changed and why, for a reader who
     has not seen the diff. No jargon. Link the issue, approved spec or
     decision record. Do not restate issue or Project status here; GitHub
     issues and Project Status own work state. -->

## Decision requested

<!-- Include only when a user or external decision is outstanding: state the
     exact decision and any gate that stays open after merge. Delete this
     section otherwise; the repository runbook owns merge authority. -->

<!-- Change map: add a Mermaid diagram whenever it makes the change faster to
     review than prose: changed actors or flows, state transitions, before/after
     structure, or cross-component dependencies. GitHub renders it here. Prefer a
     visual wherever it saves reviewer attention; skip only when the diff is
     simple enough that prose is already clear. Keep it small and captioned:
     never decorative. -->

## Risk and rollback

<!-- One or two lines: risk tier and blast radius, then the exact
     rollback or forward-repair step if this change is wrong after
     merge. -->

## Evidence

Base: `main` at `<sha>`. Head under review: `<branch>` at `<sha>`.
Rows are bound to the head they ran on, and the merging head needs green CI.
Review a later commit's delta; a clean base merge needs only CI. Apply the
HARNESS head-move classes in shadow until tooling enforces them.

<!-- Every row must be externally verifiable: an exact command with its
     result, or a linked artifact, bound to the exact head SHA. Never
     leave a cell empty; record a result or an N/A reason. Add rows for
     change-specific gates (migration preflight, live smoke, evaluation,
     load); do not delete rows. -->

| gate | command or artifact | result | head SHA | N/A reason |
| --- | --- | --- | --- | --- |
| Style / lint | | | | |
| Typecheck, tests, build | | | | |
| Production dependency audit | | | | |
| Contract and cutover | | | | |
| Security and operations | | | | |
| Operator documentation | | | | |

## Independent review

- Author route: <!-- adapter/model@effort from the attempt receipt; include receipt path -->
- Reviewer route: <!-- adapter/model@effort from the review receipt; model family derives from the resolved model; native: claude/<model>@<effort> (anthropic; resolved) -->
- Reviewer role:
- Independence from authorship/implementation context:
- Exact head reviewed:
- Unresolved P0-P2 findings:

<details>
<summary>Implementation detail</summary>

<!-- Approach and notable choices, authorised write scope, prohibited or
     external actions, and anything a future maintainer needs to
     understand the diff. -->

</details>
