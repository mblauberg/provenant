# Software delivery contract

Delivery acceptance is met by objective checks and tier-required review; the
chair reports the outcome and the user may still reject it. Explicit user
acceptance is required only for `terminal` work or when the approved scope
asks for it. A project may name its landing gate as acceptance. The current
receipt code still enforces explicit human approval; the field descriptions
below document that compatibility constraint, not a universal user gate.
Do not fabricate approval. See the
[acceptance sweep](../../../docs/acceptance-sweep-2026-10-21.md) for code sites.

`implement` executes the `software` profile inside the single canonical
`delivery-run` receipt. Use the complete `init` command in `deliver` with
profile `software` to create `.agent-run/runs/<run-dir>/RUN.json`, then use the delivery
validator. There is no separate implementation receipt format.

The approved scope supplies acceptance criteria, risk and authority. Record
software artifacts with profile types (`source`, `migration`, `configuration`
or `documentation`), map each canonical artifact to its changed security
surfaces, and link deterministic tests and security checks before independent
review. Stochastic or judgement-bearing product behaviour also links an
`evaluate` receipt.

The receipt is flat: it records approvals, artifacts, evidence, reviews and
gates, not a transition history. The `deliver` receipt contract owns repair
recording, its shared risk-tier budget and the back-to-`scope` boundary.

Validate from the project root:

```sh
"$(provenant root)/skills/deliver/scripts/validate_delivery.py" \
  .agent-run/runs/<run-dir>/RUN.json --workspace-root "$PWD" --verify-hashes \
  --product-root "<product-root>"
```

Installed skill callers may export `AGENT_FABRIC_PRODUCT_ROOT` instead. An
explicit `--product-root` takes precedence.

## Receipt portability

The exact `.agent-run/runs/<run-dir>/` receipt and its raw operational artifacts stay
local, ignored and validator-readable. Never force-track them, even when a
programme requires per-lane receipts. Project the durable tested-tree facts,
review verdicts, artifact identities and pending gates into tracked project
docs, fixtures and the PR evidence index. That curated projection is public
evidence; it does not replace or weaken validation of the private canonical
receipt.

Report delivery acceptance when its checks and required review pass. The
current receipt may remain blocked on its legacy human field; report that
separately. Production promotion requires separate user authority.

## Terminalisation

When a run directory exists, terminalise it after acceptance, failure or
cancellation with
`"$(provenant root)/skills/orchestrate/scripts/run_dir_finalize.py"`.
An outer orchestrator's `awaiting-user` transport remains active across that
call and does not rename the canonical receipt state, so a still-open outer
run is not a reason to skip terminalising the inner one.

## Post-merge continuity

For a pull-request delivery, retain the complete ignored run directory until
the merge commit and its `ci-status` check exist. New runs are anchored at the
primary checkout, so the directory survives implementation-worktree pruning
([post-merge pruning](../../../docs/worktrees.md#post-merge-pruning)). Legacy
worktree-local runs require a deliberate one-time copy before pruning. Run
`scripts/bind_merged_delivery.py` from the primary checkout with the pre-existing typed exact-head
review artifacts. The binder reads PR and `ci-status` truth through the
authenticated GitHub API, holds an exclusive receipt lock, refuses to run once
human acceptance is recorded, and adds:

- a canonical `git_revision` artifact bound directly to the exact merged commit
  and its resolved tree, with no archive or per-file digest;
- readable, hash-bound `github-pull-request-evidence` and
  `github-ci-evidence` JSON; and
- one readable `code-review-evidence` JSON artifact for every passing review
  retained by the receipt.

The merged tree must equal the reviewed PR-head tree, and `ci-status` binds the
merge commit. Validate the updated receipt with `--verify-hashes` before
reporting acceptance or requesting any required user decision. In the current
schema, only explicit user acceptance records the
`human_gates.acceptance` approval on that same receipt, and release approval is
a separate gate; release validation will not reconstruct missing evidence
later. Frozen schema-v1 Git-archive receipts remain readable,
but this binder only emits the digestless commit-and-tree form.

Because binding reads GitHub with the CLI's stored authentication, the approved
Authority V2 scope must already set `network.tool_egress: allowlist`, include
`api.github.com` in `network.allowed_hosts`, set
`secrets_access: use-without-disclosure`, and name the `github-cli-auth` secret
reference. The binder validates the complete receipt and these explicit grants
before creating its lock file or invoking `gh`; denied runs leave no receipt or
artifact mutation.
