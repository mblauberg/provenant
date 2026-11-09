---
name: refactor
description: "Use for authorised behaviour-preserving refactors and large structural changes proved by a mechanical oracle. Not for new behaviour or read-only review; use test-design or code-review."
---

# Refactor

Record the cost to remove, observable invariants and the before/after ownership and dependency shape. Choose an oracle before changing structure: the compiler for type-driven moves, a reviewed codemod for uniform changes across about 20 or more sites, a differential run, or characterisation tests. Record the baseline and commands so another agent can repeat the comparison.

Land the shared seam first when independent modules depend on it. Then make the large change under the same oracle, with disjoint write scopes and one writer per worktree. Delete the old path in the same change and rewrite tests coupled to its structure at the public boundary. For a codemod, review the transform and a sample of sites. Structural search can locate candidates, for example `sg -p '$A.unwrap()'`.

For dependent steps, stack the seam first and its consumers above it; follow the repository's runbook for stacked pull requests.

Do not add compatibility shims, legacy readers or fallback paths. Use a temporary strangler only while live traffic or persisted data moves, or behind a weak oracle; give it an expiry and delete it when the move finishes. Bound retries per unit and route the long tail to a senior. Stop on an unexplained oracle failure or discovered behaviour change; use diagnose or scope as appropriate. Report the ownership and dependency delta, evidence and residual uncertainty for code-review.

## How it runs

Use `implement` for a shared seam and `batch` for disjoint modules into one integration branch. Inspect `provenant run --use batch --file work.toml --explain`, with worktree and write_scope bindings; submit without `--explain`.
