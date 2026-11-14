# Runbooks
Owner: repository maintainers
Anchor: [`check-doc-truth`](../../scripts/check-doc-truth)

Procedures the tool cannot make obvious. Command syntax is `provenant help --all`.

**Set up and configure**

- [Install, upgrade and remove](install.md)
- [Harness configuration](configuration.md): precedence, lane sandbox, worktree setup, pre-push hook
- [Providers and accounts](providers.md), [fabric hosts](fabric-hosts.md), [cloud launch](cloud-launch.md)
- [Routing and templates](routing-and-templates.md)

**Do the work**

- [GitHub workflow](github-workflow.md): issues, branches, pull requests, landing
- [Lane lifecycle](lane-lifecycle.md): launch, delivery, recovery
- [Human inbox](human-inbox.md), [session checkpoints](session-checkpoints.md), [Claude hooks](claude-hooks.md)

**Keep it healthy**

- [CI](ci.md), [test tiers](test-tiers.md), [bench](bench.md)
- [GC](gc.md), [telemetry](telemetry.md)
