# Route resolution portability: independent review

> **Historical pre-ADR-0020 evaluation.** The removed daemon paths and review
> findings below are retained evidence, not current implementation guidance.
> See [ADR 0020](../../adr/0020-retire-the-daemon-fabric.md) and
> [`runtime/fabric/README.md`](../../../runtime/fabric/README.md).

**Verdict: MERGE-AFTER-FIX.** The CI defect is genuinely fixed and the new regression test is
real, but the change quietly swaps a dependency on provider binaries for a dependency on a clean
Git checkout of the adapter wrapper, and the split-root test was amended in a way that conceals
exactly that. Three of the four MAJOR items are small edits; MAJOR-1 needs a decision.

- Target: worktree `route-resolution-portability`, head `3c2e1387`, commits `e55c4306` and
  `3c2e1387` on top of `a8c9ce7f`.
- Reviewer did not author the change. Every claim in the author's summary was re-checked against
  the code or by execution.

## Test counts

| Run | Code | Tests | Result |
| --- | --- | --- | --- |
| Worktree, normal PATH | HEAD | HEAD | **198 passed** (95.2 s) |
| Worktree, restricted PATH (no provider CLI) | HEAD | HEAD | **198 passed** (61.1 s) |
| Scratch copy, fix reverted | `a8c9ce7f` | HEAD | **2 failed, 1 passed** (195 deselected) |
| Scratch copy, tests reverted | HEAD | `a8c9ce7f` | **2 failed** (195 deselected) |

Restricted PATH was
`$SCRATCH/nobin:/usr/bin:/bin`, containing node, npm, npx, git, uv, python3, sh and bash and no
provider CLI. Python was the pinned venv interpreter, never a system pytest. Scratch copies live
outside the worktree; the worktree was not modified (`git status --porcelain` empty at exit).

## Findings

### MAJOR-1: resolution now refuses whenever a wrapper file has uncommitted edits

`scripts/model_route.py:205` switched the validator to `--declaration-only`, which routes into
`verifyAdapterCompatibility` instead of `validateEnabledAdapterExecutables`
(`runtime/agent-fabric/scripts/validate-adapter-executables.ts:27-46`). Those two functions are
not the same check minus the binary. `verifyAdapterCompatibility` derives wrapper provenance at
`runtime/agent-fabric/src/adapters/compatibility.ts:348-352`, which calls `deriveWrapperProvenance`
(`compatibility.ts:173-200`) and `verifyWrapperTrackedAndClean` (`compatibility.ts:139-165`). The
wrapper entrypoint must live in a Git repository, be tracked at HEAD, and be byte-identical to its
committed content (`git diff --quiet HEAD -- <file>` at `compatibility.ts:156`).
`validateEnabledAdapterExecutables` (`compatibility.ts:362+`) does none of this.

Concrete failure: an agent edits `runtime/agent-fabric/src/adapters/providers/claude-agent-sdk.ts`
in a worktree, as happens routinely in this harness. Every route through the Claude adapter then
refuses with `adapter_compatibility_invalid` and exit 2 until the edit is committed, including
`cf_dispatch.sh`, which resolves a route per dispatch leg. Demonstrated by execution on a scratch
copy: appending one comment line to that wrapper turned `status: ok` into
`status: adapter_compatibility_invalid`; reverting the line restored `ok`; the identical edit
against pre-diff code returned `ok`. So this is introduced by the diff, not pre-existing.

The same requirement means any product root without Git history, or an exported or vendored tree,
cannot resolve a route at all. That is the same class of defect the lane set out to fix, moved
from "no provider binary" to "no Git provenance".

Fix options, in order of preference: pass a flag that skips wrapper provenance during resolution
(resolution is planning, and provenance is already re-verified immediately before spawn at
`compatibility.ts:225-244`, `verifySpawnWrapperProvenance`); or keep provenance but downgrade a
failure to a recorded fact rather than a refusal, consistent with the stated design of this change.

### MAJOR-2: the split-root test was amended to hide MAJOR-1

`tests/test_model_route.py:2738-2748` deletes the copied provider directory in the synthetic
product root and symlinks it back to the real repository, with the commit message stating the
purpose plainly: "preserves Git history so wrapper provenance derivation succeeds". The related
fixtures at `tests/test_model_route.py:42-56` and `:2947-2950` rewrite `wrapper_entrypoint` to
absolute paths inside the real checkout.

The test exists to prove a split product root and instance root work. After the amendment it only
proves that works when the product root borrows the developer's own repository directory. Proven
by execution: HEAD production code with the pre-change test fixtures fails at
`tests/test_model_route.py:2777` with `adapter_compatibility_invalid`, and the pre-change
`test_explicitly_selected_disabled_pi_requires_a_usable_executable` fails with
`adapter_compatibility_invalid` rather than the executable status it asserted. Both failures are
MAJOR-1 surfacing. The test change is not a rename or a tidy-up; it is accommodation of an
undeclared behaviour regression. If MAJOR-1 is fixed, the symlink and the absolute-path rewrites
should all come back out.

### MAJOR-3: `executable_available` is hardcoded false, so it is not a fact

`runtime/agent-fabric/scripts/validate-adapter-executables.ts:63-65` emits
`executable_available: declarationOnly ? false : ...`, and `scripts/model_route.py:205` always
passes `--declaration-only`. The field is therefore always `false` in every route this code can
produce. Demonstrated: on a machine with `claude` and `codex` both installed on PATH, a successful
`--adapter-gate fabric` resolution returns `"status": "ok", "executable_available": false`.

It is also write-only. A repository-wide grep finds the producer, `scripts/model_route.py:220-223`
and `:576/595-598`, and two test assertions at `tests/test_model_route.py:2961` and `:2984`.
Nothing reads it: not `cf_dispatch.sh`, whose route field extraction at
`skills/orchestrate/scripts/cf_dispatch.sh:580` lists seventeen keys and not this one; not the
TypeScript router at `runtime/agent-fabric/src/routing/model-route.ts`.

A constant-false field that a receipt reader would reasonably interpret as "we checked and the
binary is absent" is worse than no field. Either compute it honestly (resolve the executable
without letting failure refuse the route) or drop it, and drop the two assertions with it.

### MAJOR-4: the adapter.ts comment asserts a safety property that is not implemented there

`runtime/agent-fabric/src/adapters/providers/adapter.ts:601-602` is the entire change to that
file: "Route resolution records executable availability only. Dispatch validates it at this
boundary before starting adapter work." No validation was added. `effect()` at `adapter.ts:604`
calls `options.boundary.spawn(payload)` directly. The author's own commit message for `e55c4306`
says "Executable validation at dispatch time remains for future work (marked with comment)", which
contradicts the summary claim that refusal "moves to dispatch".

Validation does exist, but not at that boundary and not uniformly. Verified downstream:
`claude-agent-sdk.ts:638-643`, `codex-app-server.ts:457-458` via `codex-json-rpc.ts:294-301`,
`command-boundaries.ts:200` for agy and cursor, and `kiro-acp.ts:195` / `opencode-acp.ts:67` all
call `verifyProviderConformance`, which yields a typed
`FabricError("ADAPTER_ARTIFACT_MISSING", ...)` at `provider-identity.ts:124`. `pi-rpc.ts:120-124`
has no such hook, so a missing Pi binary surfaces only as
`ProviderAdapterError("PROVIDER_SPAWN_FAILED", "Pi RPC failed to start: spawn ... ENOENT")` at
`pi-jsonl-client.ts:113-115`. Separately, daemon composition still refuses to admit an adapter with
an unresolvable executable at `daemon/composition.ts:131-163`.

So the observable diagnostic after resolution succeeds with no binary is acceptable for six of the
seven adapters and merely generic for Pi. The comment is the problem, not the diagnostics. Delete
it, or reword it to name the boundary that actually checks. Do not merge a comment that promises a
check the reader will then not look for.

### MINOR-1: the precedence fix is partial

Order as implemented in `resolve()`: unknown adapter (`scripts/model_route.py:528`), catalogue
compatibility unknown (`:555`), declaration load failure (`:562-570`, via `:92-131`), Fabric
activation config unreadable or malformed (`:578-590`, the hoisted check), declaration validation
(`:594-606`), account-default conflict (`:608-618`), then much later `adapter_disabled` (`:746`)
and `adapter_inactive` (`:759`).

Hoisting `fabric_activation_invalid` above the declaration check is correct and fixes the named
test. But `adapter_inactive` still sits after the declaration check and is still masked.
Demonstrated: with an empty `activeAdapters` list the route returns `adapter_inactive`; add one
uncommitted line to the wrapper and the same request returns `adapter_compatibility_invalid`.
`adapter_disabled` escapes masking only because the declaration check is skipped for a
Fabric-disabled adapter at `:593`. The right order is activation state before declaration
validity, for the same reason the author moved the activation config check.

### MINOR-2: the receipt schema was not updated

`runtime/agent-fabric/schemas/fabric-receipt.schema.json:105` defines `modelRouteReceipt` with
`additionalProperties: false` and no `executable_available` property. `model-route.ts:211` writes
the parsed route JSON verbatim to the receipt path, and
`fabric-receipt.schema.json:60` embeds that object as `receipt` inside `modelRoutingReceipt`
evidence. Any consumer validating a v1 receipt would now reject the route. Latent today only
because `exports/projector.ts:113` always emits an empty `modelRoutingReceipts` array. Note
`adapter_active` is missing from the same schema and predates this change, so the schema is
already drifting.

### MINOR-3: resolution now shells out to Git on a hot path

`compatibility.ts:134-137` invokes the bare name `git`, so route resolution now requires Git on
PATH in addition to Node. The new test at `tests/test_model_route.py:2969-2971` silently encodes
this by symlinking `git` into its minimal bin directory. `cf_dispatch.sh` resolves a route per leg,
so this adds two Git subprocesses per dispatch.

### MINOR-4: declaration-only mode re-implements adapter selection in the CLI script

`validate-adapter-executables.ts:28-46` parses the compatibility YAML a second time to compute
enabled adapter IDs, duplicating logic the library already owns, and `--mandatory-primary` is
silently ignored in that branch. It also sets `requireEnabled: adapter === undefined`, so with the
`--adapter` flag that `model_route.py` always passes, the enabled check, provider identity policy
check and Cursor install root check at `compatibility.ts:283-305` are all skipped. The Python side
covers "enabled" separately, so this is a maintainability point rather than a hole, but the
duplication will drift.

### MINOR-5: an unparseable declaration reports "unavailable", not "invalid"

Verified by execution: a truncated compatibility YAML returns `adapter_compatibility_unavailable`
with exit 2, from `scripts/model_route.py:103`. It refuses, which is what matters, but the status
is less specific than the one a reader would expect. Pre-existing, not introduced here.

### NIT-1

`tests/test_model_route.py:2729` adds `import shutil as _shutil` inside the test body although
`shutil` is already imported at module scope and used four lines later.

### NIT-2

The fixtures at `tests/test_model_route.py:42-56` and `:2947-2950` interpolate absolute paths into
the real checkout, coupling temporary fixtures to the developer's working copy.

## Question-by-question answers

1. **Did refusal move to dispatch?** No. The adapter.ts change is a comment only; nothing was
   added at that boundary. A dispatch-time check does exist, but it pre-dates this diff and lives
   elsewhere (per-boundary `verifyProviderConformance`, plus daemon composition), and it does not
   cover pi-rpc. See MAJOR-4 for the concrete diagnostics.
2. **Was a test weakened?** Yes for the split-root test, no for the new PATH test. See MAJOR-2.
   `test_resolve_succeeds_without_adapter_binaries` genuinely restricts PATH and asserts the
   provider CLIs are unresolvable; with the fix reverted it fails with
   `adapter_executable_unavailable`, so it is a real regression test and mocks nothing.
3. **Is the precedence fix real and complete?** Real but incomplete. See MINOR-1.
4. **Stub binaries?** None. The only new PATH manipulation narrows PATH rather than seeding fakes,
   and the symlinks it creates are node, npx and git, with explicit assertions that `claude` and
   `codex` do not resolve. The pre-existing `executable: "sh"` fixture substitution is unchanged
   context in the diff. No chmod, no shim, no fixture named after a provider, and the worktree has
   no untracked files.
5. **Receipt field correctness.** Not schema-validated, not read by anything, and constant false.
   See MAJOR-3 and MINOR-2.
6. **Does the Python side still fail closed?** Yes, verified by execution. Unparseable declaration
   returns `adapter_compatibility_unavailable` exit 2; schema-invalid declaration returns
   `adapter_compatibility_invalid` exit 2; a Fabric-disabled adapter under the Fabric gate returns
   `adapter_disabled` exit 1.

## Verified by execution versus by reading

**By execution:**

- Both full test runs of `tests/test_model_route.py`, normal and restricted PATH, 198 passed each.
- Both scratch revert experiments and their four failures, including which status each produced.
- The dirty-wrapper regression, on both HEAD and pre-diff code, with a control run in between.
- `executable_available` returning false on a machine where both provider CLIs resolve on PATH.
- `adapter_inactive` being masked by `adapter_compatibility_invalid`.
- All three fail-closed probes in question 6.
- Confirmation that the scratch test file, not the worktree file, was the one pytest executed.

**By reading:**

- That `deriveWrapperProvenance` is reached only through `verifyAdapterCompatibility` and not
  through `validateEnabledAdapterExecutables`, which is the mechanism behind MAJOR-1 and MAJOR-2.
- The absence of any executable check at `adapter.ts:604`, and the presence of
  `verifyProviderConformance` in six boundaries but not pi-rpc.
- The receipt schema gap and the empty `modelRoutingReceipts` projection.
- The grep establishing that nothing consumes `executable_available`.
- The full precedence ordering inside `resolve()`.

## Required before merge

1. Resolve MAJOR-1: stop wrapper provenance from refusing a route, or accept the coupling
   explicitly and document it as a routing precondition.
2. Revert the split-root symlink and the absolute-path fixture rewrites once MAJOR-1 is fixed
   (MAJOR-2).
3. Compute `executable_available` honestly or delete it (MAJOR-3).
4. Delete or correct the adapter.ts comment (MAJOR-4).
