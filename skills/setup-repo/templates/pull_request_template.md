## Summary

<!-- Two or three plain sentences: what changed and why. Link the issue,
     spec or decision record. -->

## Decision requested

<!-- Include only when a user or external decision is outstanding: state the
     exact decision and any gate that stays open after merge. Delete this
     section otherwise; the repository runbook owns merge authority. -->

<!-- Add a small, captioned Mermaid diagram when it is faster to review than
     prose. -->

## Risk and rollback

<!-- Risk tier and blast radius, then the rollback or forward-repair step. -->

## Evidence

Base: `main` at `<sha>`. Head under review: `<branch>` at `<sha>`.
Rows are bound to the head they ran on, and the merging head needs green CI.
Review a later commit's delta; a clean base merge needs only CI.

| gate | command or artifact | result | head SHA | N/A reason |
| --- | --- | --- | --- | --- |
| Style / lint | | | | |
| Typecheck, tests, build | | | | |
| Production dependency audit | | | | |
| Credentials or authority touched (yes/no) | | | | |

## Independent review

- Author route: <!-- adapter/model@effort from the attempt receipt; include receipt path -->
- Reviewer route: <!-- adapter/model@effort from the review receipt; model family derives from the resolved model; native: claude/<model>@<effort> (anthropic; resolved) -->
- Reviewer role:
- Independence from authorship/implementation context:
- Exact head reviewed:
- Unresolved P0-P2 findings:

<details>
<summary>Implementation detail</summary>

<!-- Approach, notable choices and anything a future maintainer needs. -->

</details>
