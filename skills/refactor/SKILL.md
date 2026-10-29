---
name: refactor
description: "Use for authorised behaviour-preserving refactors and large structural changes proved by a mechanical oracle. Not for new behaviour or read-only review; use test-design or code-review."
---

# Refactor

Record the cost to remove, observable invariants and the before/after ownership and dependency shape. Choose an oracle before changing structure: the compiler for type-driven moves, a reviewed codemod for uniform changes across about 20 or more sites, a differential run, or characterisation tests. Record the baseline and commands so another agent can repeat the comparison.

Land the shared seam first when independent modules depend on it. Then make the large change under the same oracle, with disjoint write scopes and one writer per worktree. Delete the old path in the same change and rewrite tests coupled to its structure at the public boundary. For a codemod, review the transform and a sample of sites. Structural search can locate candidates, for example `sg -p '$A.unwrap()'`.

Use a temporary strangler only for production traffic, persisted data, published APIs or a weak oracle; give its adapter an expiry. Do not add compatibility shims for unpublished code. Bound retries per unit and route the long tail to a senior. Stop on an unexplained oracle failure or discovered behaviour change; use diagnose or scope as appropriate. Report the ownership and dependency delta, evidence and residual uncertainty for code-review.

## How it runs

Use `generator-evaluator` to land the shared seam when needed. For multiple modules, prepare registered worktrees and a `big-batch` params file with one `lanes` entry per module and an `integration_branch`.
Run `provenant run --preset big-batch --prompt-file BRIEF --params-file LANES --explain` before submission, then run without `--explain`.
