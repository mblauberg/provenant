# Steward and closing

## Sweep checks

- **Stale:** a referenced PR merged, a dependency closed or cited evidence moved.
  Re-verify against current state, then correct or close with the reason.
- **Orphaned:** the parent closed or the sub-issue link no longer resolves. Re-parent
  or make it standalone.
- **Mis-closed:** closed with no merge evidence or stated reason. Reopen, or add the
  missing evidence.
- **Superseded:** close as duplicate-of or not planned, but only after comparing its
  acceptance to the merged code.
- **Thin:** a body a stranger cannot act on, or one citing scratch paths. Fix or flag.

## Delegation

The chair gives each steward the repository, issue and PR links, the authorised
actions and a stop condition. Before retrying any write, read current issues, PRs
and Fabric tasks with `gh`: a local record cannot prove a remote write happened
exactly once. If the outcome stays uncertain, report it before writing again. A
steward returns at most 15 lines: actions taken, links, no-ops, decisions needed.
