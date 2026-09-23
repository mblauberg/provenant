# Direct CLI fallback

Use this path only when Fabric cannot express the task or is unavailable. Record `FABRIC-ROUNDTRIP-UNAVAILABLE`, the reason, exact route, command class and a named result artifact. Return to Fabric when it becomes available. For current CLI flags, run the provider's help and capability probe; old command examples are not authority.

## Preflight

1. Confirm the requested workspace, read or write scope, disclosure boundary and provider authentication without exposing credentials in argv or logs.
2. Create a unique run directory under the primary checkout's `.agent-run/runs/` using `run_dir_init.sh` when available. Put prompts, full output and diagnostics there. Place disposable one-shot files under `.agent-run/scratch/`, never a system temporary directory.
3. Use the same model and effort that the task requested. If the CLI substitutes either, record requested and applied values. [codex-capabilities.md](codex-capabilities.md) describes Codex probes.
4. For a writer, require an owned registered worktree and one writer. A read-only run needs an enforced boundary before it can certify an assurance claim; label `best_effort` or `prompt_only` honestly.

## Execution

Prefer file-backed prompt and output transport. Run noninteractively with a bounded wall clock and idle watchdog. Keep stdin policy and process-group supervision adapter-specific; do not assume that `/dev/null` is safe for every provider. Capture exit code, structured error, observed model, provider session id and the full output path. An exit code of zero alone does not prove completion.

For direct Agy calls, the prompt is passed as one argument. Keep the instructions
and question concise, and give Agy access to workspace files for large source
material instead of embedding that material in the prompt.

Classify usage limit, rate limit, authentication, unavailable model, permission denial, stall, timeout, cancellation and partial result separately. Do not silently retry a write or relaunch beside a detached live owner. A `QUESTION:` response requires chair input; resume the saved provider session when supported. See [worker-liveness.md](worker-liveness.md) for a detached direct process.

## Reporting

Report the result status, path, route and guarantee. The route should be `adapter/model@effort` from the observed or resolved invocation, with provider and identity stated. A native Claude subagent route is `claude/<model>@<effort> (anthropic; resolved)` from the Agent tool model parameter. Keep the exact receipt beside the output. The chair verifies the live tree and uses the [HARNESS.md](../../../HARNESS.md) review ladder.

`cf_dispatch.sh` remains the low-level direct adapter when needed. It is a degraded execution path, not a Fabric request/reply. Global provider config edits require explicit authority; prefer per-run flags.
