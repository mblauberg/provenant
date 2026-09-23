# Codex capability probing

Codex model and effort support can change with CLI version and account. `provenant capabilities codex --out .agent-run/scratch/codex-capabilities.json` captures the current CLI capability snapshot. Use it when a direct CLI fallback needs to choose a model or effort. Fabric's catalogue and `fabric_adapters` already expose the applied route on its normal path.

The probe records available model IDs, effort levels and version. A fresh model entry is runtime evidence, while the dated product catalogue supplies alias order and audit context. A requested model absent from the snapshot is a warning or model substitution when a runnable route exists. A requested effort absent from that model's supported set is substituted to the nearest supported value and recorded. Explicit unknown model IDs pass through with a note when the provider accepts them. Do not claim the model was observed unless the provider session or rollout shows it.

Historical resolver statuses such as `capability_discovery_failed`, `capability_snapshot_stale`, `capability_model_unavailable`, `no_candidate_available`, `effort_unsupported` and `no_effort_available` describe the old direct resolver. Fabric v2 reports typed preflight failures only when it cannot run, and otherwise reports the applied model and effort plus warnings. A stale snapshot calls for a new probe, not an invented model ID.

Never put credentials in the capability file or command line. Keep the file under `.agent-run/scratch/` and let `provenant clean` expire it.
