# Layering & context discipline

The reason to use multiple agents is **context isolation**. Each worker runs in a clean window on one
subtask; you aggregate the results without any single context carrying the whole load. Depth is a cost.
Add a layer only when it buys real isolation.

## Context rot (why this matters)

Model accuracy declines as a context fills, not only near the window limit but well inside it, and
even when the relevant fact is present (irrelevant *volume* alone degrades reasoning). So the failure
to avoid is **letting raw sub-outputs accumulate in the orchestrator**. Keep your own context small and
high-signal; push detail to files.

## The worker output contract (use for every dispatch)

Give each worker:
- a **tight, single-purpose brief** (scope it so it can't drift);
- the **minimum context** it needs (not your whole state);
- an explicit **output contract**:
  > Write your full output to `<run-dir>/findings/<name>.md`. Reply to me with ONLY: 3–6 headline
  > points, any surprises/contradictions, and the file path. Do **not** paste the full output back.

Cap returned summaries at roughly 1–2k tokens. The file is the record; your context holds pointers.

## Handoff schema (every layer is a checkpoint)

When one layer's output feeds the next, pass a structured record, not prose, so errors get **filtered**
rather than propagated:

```
claim:              <the assertion / result>
source:             <path or URL that backs it>     # always keep the pointer
confidence:         high | medium | low
unresolved:         <open question, or none>
prohibited-action:  <anything the next layer must NOT do, or none>
validation:         <checked? how? by whom?>
```

Validate a worker's output before a downstream worker consumes it; one agent's hallucination must not
become another's "fact".

## Reversible compression

When you summarise or drop content, **keep the pointer** (file path / URL / line range) so a detail
needed ten steps later is recoverable. Irreversible compaction that silently loses an observation is
the main long-run failure mode.

## Depth & breadth limits

- Cap **inline** workers per orchestrator at ~**3–5**; beyond that, use a native workflow or add a
  **tier** (a synthesiser over groups of workers) rather than shrinking a task that really needs wider
  fan-out.
- **Read-heavy parallel = safe.** **Concurrent shared-state writes = forbidden**: partition work so
  no two workers write the same file/state at once (append-only, namespaced).
- Prefer **fewer, structured handoffs** over elaborate manager hierarchies. More hierarchy is not more
  reliable.
- Over-decomposition is a real cost: a 10-step pipeline can spend more on handoffs than on work.
  Decompose only where a subtask genuinely benefits from its own context.
