# Steward mode

The coordinator delegates bounded tracker work through Fabric tasks, with the
repository, issue/PR links, authorised actions and a stop condition. The worker
uses the repository's declared tracker command for writes and follows
[filing](filing.md), [linking and board](linking-and-board.md) and
[hygiene](hygiene.md).

Before retrying any authorised write, reconcile current issues, PRs and Fabric
tasks with `gh` reads. A local outcome record cannot prove that a remote write
happened exactly once. If the remote outcome remains uncertain, report the
blocker before another write.

Return at most 15 lines with completed actions, links, no-ops and decisions
still needed. Full detail stays in the tracker and Fabric task.

Create, edit, link, close and set Status inside the repo's rules. Never
delete an issue, transfer it, change priority or horizon except by the repo's
logged promotion rule, close a Decision, or write code.
