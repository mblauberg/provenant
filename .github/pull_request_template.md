## Summary

<!-- Two or three plain sentences: what changed and why, for a reader who
     has not seen the diff. No jargon. Link the issue, approved spec or
     decision record. Do not restate issue or Project status here; GitHub
     issues and Project Status own work state. -->

## Decision requested

<!-- The exact decision the reviewing user is asked to make, e.g.
     "Approve merge of <head SHA> to main." Name any user or external
     gate that stays open after merge. -->

<!-- Change map: add a Mermaid diagram whenever it makes the change faster to
     review than prose — changed actors or flows, state transitions, before/after
     structure, or cross-component dependencies. GitHub renders it here. Prefer a
     visual wherever it saves reviewer attention (docs/runbooks/github-workflow.md);
     skip only when the diff is simple enough that prose is already clear. Keep it
     small and captioned — never decorative. -->

## Risk and rollback

<!-- One or two lines: risk tier and blast radius, then the exact
     rollback or forward-repair step if this change is wrong after
     merge. -->

## Evidence

Base: `main` at `<sha>`. Head under review: `<branch>` at `<sha>`.
A later commit invalidates every row below and the independent review;
rerun both against the new head.

<!-- Every row must be externally verifiable: an exact command with its
     result, or a linked artifact, bound to the exact head SHA. Never
     leave a cell empty — record a result or an N/A reason. Add rows for
     change-specific gates (migration preflight, live smoke, evaluation,
     load); do not delete rows.

     Compound gates cover:
     - Contract and cutover: current schema, protocol and configuration
       owners checked; any pre-release direct cutover, regeneration or
       reset path recorded; no legacy reader, compatibility bridge or
       historical-format promise added; persistence or schema changes
       carry migration preflight, rollback or forward-repair, and
       trigger or query-plan evidence.
     - Security and operations: authority, secret-disclosure and
       least-privilege boundaries; resource limits, failure behaviour
       and recovery; live provider or daemon smoke when runtime
       behaviour changed.
     - Operator documentation: docs describe expected behaviour, not
       workstation state. -->

| gate | command or artifact | result | head SHA | N/A reason |
| --- | --- | --- | --- | --- |
| Harness style | `scripts/check-harness` | | | |
| Fabric typecheck, tests, build | `npm run check` | | | |
| Fabric evaluation | `npm run test:evaluation` | | | |
| Fabric load | `npm run test:load` | | | |
| Production dependency audit | `npm run audit` | | | |
| Contract and cutover | | | | |
| Security and operations | | | | |
| Operator documentation | | | | |

## Independent review

- Reviewer role:
- Model family:
- Independence from authorship/implementation context:
- Exact head reviewed:
- Unresolved P0-P2 findings:

<details>
<summary>Implementation detail</summary>

<!-- Approach and notable choices, authorised write scope, prohibited or
     external actions, and anything a future maintainer needs to
     understand the diff. -->

</details>
