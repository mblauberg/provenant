## Additions
### Pipelines and graphs

- A graph writer can set `continues` to repair an earlier writer: it reuses that worktree and branch, and the host pushes its commits to the same pull request without force, so a moved branch is refused (#2233; see [writer continuation](../../specs/fabric.md#writer-continuation)).
