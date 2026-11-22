## Summary

<!-- Two or three plain sentences: what changed and why. Link the issue,
     spec or decision record. -->

## Decision requested

<!-- The exact decision asked of the user, e.g. "Approve merge of <head SHA>
     to main", and any gate that stays open after merge. -->

<!-- Add a small, captioned Mermaid diagram when it is faster to review than
     prose. -->

## Risk and rollback

<!-- Risk tier and blast radius, then the rollback or forward-repair step. -->

## Evidence

Base: `main` at `<sha>`. Head under review: `<branch>` at `<sha>`.
Rows are bound to the head they ran on, and the merging head needs green CI.
A later commit needs review only of its delta (a repair or a conflict
resolution); a clean merge of the base needs none.

| gate | command or artifact | result | head SHA | N/A reason |
| --- | --- | --- | --- | --- |
| Harness style | `scripts/check-harness` | | | |
| CI | | | | |
| Credentials or authority touched (yes/no) | | | | |

## Independent review

- Author route: <!-- adapter/model@effort from the attempt receipt; include receipt path -->
- Reviewer route: <!-- adapter/model@effort from the review receipt; model family derives from the resolved model; native: claude/<model>@<effort> (anthropic; resolved) -->
- Reviewer role:
- Independence from authorship/implementation context:
- Exact head reviewed:
- Unresolved blocking findings:

<details>
<summary>Implementation detail</summary>

<!-- Approach, notable choices and anything a future maintainer needs. -->

</details>
