# Claude-only Workflow adapter

This is the Claude-only layer for native Workflow execution. Feature details
and limits drift; verify them in current Claude Code documentation before use.
Codex uses `codex-subagents.md` instead.

## Workflow contract binding

This adapter realises the substrate-neutral stage, gate, recovery, and
escalation graph in [orchestration-contract.md](orchestration-contract.md).
External consumers bind to that contract, not this runtime-specific adapter.
A Workflow may coordinate adaptive waves and isolated agents, but objective
checks, author/reviewer separation, user gates, and one accountable chair still
apply.

The runtime cannot collect mid-run user approval. End at the gate-adjacent
stage, record `awaiting-user`, and continue only through a fresh user-approved
Workflow or session. Never model a one-way-door decision as pause/resume.

## Saved-workflow conventions

Saved workflows live in `.claude/workflows/` for a project or
`~/.claude/workflows/` for personal use and run as `/<name>`. Keep each script
small, inspectable, and bound to the portable contract. These saved-workflow conventions apply:

- `export const meta = { name, description, ... }` is the first statement and
  a pure literal. Do not use variables, calls, spreads, or interpolation in it.
- Read structured input from the `args` global as values, not stringified JSON.
  Treat absent input as `undefined` and validate it before dispatch.
- The Workflow script coordinates only. Agents perform filesystem and shell
  work under their assigned authority.
- Derive timestamps and run identifiers from `args` or a bootstrap worker; do
  not use `Date.now()`, argless `new Date()`, or `Math.random()`.
- Follow [routing-and-tiers.md](routing-and-tiers.md) for all stage routing;
  retain the resolved route and effort receipts.
- Put durable state and full worker output in the run directory described by
  [memory-scratchpad.md](memory-scratchpad.md); return only a digest and path.
- Use a serial applier for shared writes. Leave high-risk patches for a separate
  approve-then-apply stage with validation evidence.
- Apply [verification.md](verification.md) and [direct-cli-fallback.md](direct-cli-fallback.md)
  for clean-context, safe cross-family pressure. Record certified, advisory,
  and not-run outcomes distinctly.

## Static check before saving

- The file parses as plain JavaScript.
- `meta` is first and remains a pure literal.
- Loops have explicit bounds and consume the declared budget.
- Inputs are validated before they reach prompts or tool arguments.
- Writes are partitioned or serialised and stay inside assigned authority.
- Every required gate has an objective check or an explicit failed/not-run
  receipt.
- Gate-adjacent user decisions end the Workflow.

## Pointers

- Portable stages, gates, worker contract, and recovery:
  [orchestration-contract.md](orchestration-contract.md)
- Run-directory schema, resume, and retention:
  [memory-scratchpad.md](memory-scratchpad.md)
- Claude/Codex routing policy: [routing-and-tiers.md](routing-and-tiers.md)
- Cross-family verification: [verification.md](verification.md) and
  [direct-cli-fallback.md](direct-cli-fallback.md)
- Codex-native realisation: [codex-subagents.md](codex-subagents.md)
