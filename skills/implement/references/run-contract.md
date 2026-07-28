# Software delivery contract

`implement` executes the `software` profile inside the single canonical
`delivery-run` receipt. Create `.agent-run/<id>/RUN.json` from
`skills/deliver/templates/RUN.template.json`, set `profile: software`, then use
the delivery validator. There is no separate implementation receipt format.

The approved scope supplies acceptance criteria, risk and authority. Record
software artifacts with profile types (`source`, `migration`, `configuration`
or `documentation`), map each canonical artifact to its changed security
surfaces, and link deterministic tests and security checks before independent
review. Stochastic or judgement-bearing product behaviour also links an
`evaluate` receipt.

State follows the delivery kernel: `draft -> scoped -> approved -> executing ->
verifying -> reviewing -> awaiting_acceptance`. Repairs return through
verification and review under a repair budget of at most two cycles, which
`validate_delivery.py` enforces against the recorded `repairing` transitions.
The budget is a guardrail against unbounded loops, not a target to spend —
converge as soon as checks and review pass. Exceeding the budget means the run is
stuck: stop and return evidence to the user or `scope`, the same trigger
`docs/runbooks/github-workflow.md` uses for the merge-gate escalation. A new
requirement, authority expansion or one-way-door decision returns to `scope`;
it is not a repair.

Validate from the project root:

```sh
"${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/validate_delivery.py" \
  .agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes
```

## Receipt portability

The exact `.agent-run/<id>/` receipt and its raw operational artifacts stay
local, ignored and validator-readable. Never force-track them, even when a
programme requires per-lane receipts. Project the durable tested-tree facts,
review verdicts, artifact identities and pending gates into tracked project
docs, fixtures and the PR evidence index. That curated projection is public
evidence; it does not replace or weaken validation of the private canonical
receipt.

`awaiting_acceptance` is machine-ready, not complete. User acceptance and any
production promotion remain separate gates.

## Post-merge continuity

For a pull-request delivery, retain the complete ignored run directory until
the merge commit and its `ci-status` check exist. Copy that directory into the
synced primary checkout before pruning the implementation worktree and its
merged branch ([post-merge pruning](../../../docs/worktrees.md#post-merge-pruning)
— retention wins, so copy first, prune second), then run
`scripts/bind_merged_delivery.py` there with the pre-existing typed exact-head
review artifacts. The binder reads PR and `ci-status` truth through the
authenticated GitHub API, holds an exclusive receipt lock, keeps the receipt
at `awaiting_acceptance` and adds:

- a canonical `git_revision` artifact bound directly to the exact merged commit
  and its resolved tree, with no archive or per-file digest;
- readable, hash-bound `github-pull-request-evidence` and
  `github-ci-evidence` JSON; and
- one readable `code-review-evidence` JSON artifact for every passing review
  retained by the receipt.

The merged tree must equal the reviewed PR-head tree, and `ci-status` binds the
merge commit. Validate the updated receipt with `--verify-hashes` before asking
for acceptance. Only explicit acceptance advances that same receipt through
`accepted` to `awaiting_release`; release validation will not reconstruct
missing evidence later. Frozen schema-v1 Git-archive receipts remain readable,
but this binder only emits the digestless commit-and-tree form.

Because binding reads GitHub with the CLI's stored authentication, the approved
Authority V2 scope must already set `network.tool_egress: allowlist`, include
`api.github.com` in `network.allowed_hosts`, set
`secrets_access: use-without-disclosure`, and name the `github-cli-auth` secret
reference. The binder validates the complete receipt and these explicit grants
before creating its lock file or invoking `gh`; denied runs leave no receipt or
artifact mutation.
