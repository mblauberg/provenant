## Fixes

### Graphs and settlement

- Inline implement writers no longer settle `cause=other_failure` when their attempt succeeded, their handoff is `done` and their commit is on the remote. The graph now uses the verified pushed delivery head as commit evidence even when the sealed writer head is absent or still names the input commit, so the node succeeds and its dependants unblock. A provisional `failed` settlement reason no longer denies a finished, pushed brief. When a real precondition does fail on a successful-looking attempt, the cause is now named with its settlement reason instead of bare `other_failure`. Missing findings are not a writer failure: a writer without a findings contract is not failed for reporting no findings. (#4065)
