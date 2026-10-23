# On disk record schemas

These files are language independent JSON Schema Draft 2020-12 contracts.
`test_record_schemas.py` validates the checked-in config examples and records
written by isolated dispatch, batch, watchdog, quota, and telemetry flows.
Dispatch cases cover success, provider failure, timeout, quota exhaustion and
questions. Each subprocess has a fresh home, instance root, state root and
Codex home. Telemetry checks validate every row in primary-key order. Test
cases are the actual JSON values paired with the schema selected by the record path or
its `schema` / `record_type` discriminator. Markdown manifest rows and the
plain text digest sidecar are converted to their documented column values
before validation.

## Schema to writer map

| Schema | Record and writer |
| --- | --- |
| `canonical-attempt.schema.json` | `tasks/<task>/attempt-NNN/attempt.json`; `contract_row` and `publish_contract` in `skills/orchestrate/scripts/dispatch_run.py` (`fabric.attempt.v1`). |
| `dispatch-attempt.schema.json` | `dispatch/tasks/<task>/attempt-NNN/attempt.json`; terminal record construction in `dispatch_run.py`. |
| `adapter-receipt.schema.json` | `dispatch/tasks/<task>/attempt-NNN/adapter-receipt.json`; adapter output consumed by `dispatch_run.py` and `provider_exec.py`. |
| `worker-terminal-envelope.schema.json` | Question envelope stored as `result.md`; `worker_question_envelope_bytes` in `run_records.py`. |
| `attempt-digest-sidecar.schema.json` | `attempt.sha256`, represented by its digest and target filename; `dispatch_run.py` and `run_records.py`. |
| `batch-summary.schema.json` | `dispatch/batches/batch-NNN/summary.json`; summary writer in `batch_run.py`. |
| `manifest-row.schema.json` | One eight-column row in `MANIFEST.md`; `manifest_rows` and `append_manifest_to` in `run_records.py`, plus batch indexing in `batch_run.py`. |
| `dispatch-status.schema.json` | `dispatch-status.json`; initial write in `runtime/fabric/src/execution.ts`, terminal update in `dispatch_run.py`. |
| `dispatch-owner.schema.json` | `dispatch-owner.json`; `record_owner` and `ensure_nested_owner` in `run_lineage.py`. |
| `run-receipt.schema.json` | `RUN_RECEIPT.json`; initial writer `run_dir_init.sh`, review/pair/pane updates in `review_plan.py` and `run_dir_finalize.py`, plus dispatch close updates. |
| `supervisor-state.schema.json` | Future `supervisor.json` state record, as specified in `docs/specs/harness/rust-supervisor.md`. |
| `supervisor-exit.schema.json` | `supervisor.exit.json` terminal record (`provenant.lane.exit.v1`), as specified in `rust-supervisor.md`. |
| `launch-spec.schema.json` | `launch.json` record of the `provenant.lane.launch.v1` launch spec, as specified in `rust-supervisor.md`. |
| `observer-record.schema.json` | Future `observer.json` output described in `rust-supervisor.md`. |
| `watchdog-observation.schema.json` | `watchdog.json`; observation writer in `skills/orchestrate/scripts/stall_watchdog.py` (`fabric.watchdog.v1`). |
| `wip-checkpoint.schema.json` | `wip-checkpoint.json`; checkpoint writer in `stall_watchdog.py` (`fabric.wip.v1`). |
| `quota-snapshot.schema.json` | Per-account quota cache JSON; `blank` / `save_snapshot` in `scripts/provider_quota.py` (`provenant.quota.v1`). |
| `telemetry-usage-row.schema.json` | `usage` ledger rows; table declaration in `scripts/telemetry.py`, populated by `scripts/telemetry_ingest.py`. |
| `telemetry-event-row.schema.json` | `events` ledger rows; `put_event` in `scripts/telemetry.py`. |
| `telemetry-quota-series-row.schema.json` | `quota_series` ledger rows; `put_quota_series` in `scripts/telemetry.py`. |
| `telemetry-quota-event-row.schema.json` | `quota_events` ledger rows; quota transition ingestion in `scripts/telemetry.py`. |
| `model-routing.schema.json` | `config/model-routing.json`; read by `scripts/model_route.py` and `scripts/telemetry_costs.py`. |
| `model-preferences.schema.json` | `config/model-preferences.json`; read and updated by `scripts/model_route_preferences.py`. |
| `graph-reservation.schema.json` | Git worktree reservation; `reserve` in `runtime/fabric/src/graph/store.ts`. |
| `graph-node-receipt.schema.json` | Graph node attempt `receipt.json`; `#receipt` in `runtime/fabric/src/graph/owner.ts`. |
| `route-health.schema.json` | Route health store; `write_route_health` in `skills/orchestrate/scripts/fabric_records.py`. |
| `cooldowns.schema.json` | Cooldown store; `_store_cooldowns` in `fabric_records.py`. |
| `log-reduction.schema.json` | Optional log reduction receipt; `main` in `log_reducer.py`. |
| `delivery-profiles.schema.json` | `config/delivery-profiles.json`; loaded by delivery profile handling in `skills/deliver/scripts/delivery_contract.py`. |

## Writer disagreements

- Canonical `attempt.json` uses `schema: fabric.attempt.v1`, integer `attempt`,
  `state` and a question prompt string. The compatibility record uses
  `schema_version`, `record_type: dispatch-attempt`, `attempt_id` and a structured
  question. Their status/outcome vocabularies also differ. Canonical `digest`
  is the rendered lane status/provenance text; file and sidecar digests are
  `sha256:` values.
- TypeScript writes the initial `dispatch-status.json`; Python adds terminal
  fields. Both stages share one schema with optional terminal fields.
- `record_owner` writes the minimal `dispatch-owner.json` custody identity;
  TypeScript and nested launchers can add workspace, wall-clock start and log paths.
- Process identity fields such as `owner_started_at`, `host_started_at` and
  `pgid_started_at` contain platform `ps lstart` text. Wall-clock timestamps use
  RFC3339; process identity strings cannot use `format: date-time`.
- Normal adapter receipts and error receipts with `status`/`fix` have different
  required fields. Failed outputs may have an empty output digest. The dispatch
  exception fallback can write `{}`; the contract deliberately rejects it as
  missing evidence.
- `fabric.route-health.v1` identifies both the persisted `routes` map and a CLI
  report with `task_class_routes`. The persisted contract is checked here; the
  report remains a presentation surface.
- SQLite nullable columns permit null, including token counts and percentages.
  JSON records with fixed writer shapes are closed; genuinely extensible maps
  use typed values.

## Future records and fixtures

`launch-spec`, `supervisor-exit`, `observer-record` and `supervisor-state` have
valid and invalid examples under `fixtures/`, derived from
[`rust-supervisor.md`](../docs/specs/harness/rust-supervisor.md). They describe
future Rust records, not records emitted by the current Python flows. The
launch fixture models the draft persisted form: `env` contains variable names
only, rather than the values carried by the stdin launch protocol.
`supervisor-state` is a draft: the spec names an atomically replaced running
state file but defines no fields, so the contract is a minimal typed object.
`observer-record` is also a minimal typed draft because the spec describes
its concepts without fixing field names. The exit contract remains provisional
where the spec does not freeze the capture representation. `supervisor.lock` is
an OS lock file and has no JSON Schema.

Writer-derived fixtures cover persisted inventory additions and the launcher
status shape. Tests pair each `NAME.valid.json` with `NAME.invalid.json`.
The inventory scans Python, TypeScript and shell source throughout `scripts/`,
`skills/` and `runtime/fabric/src` in both directions. CLI and report identifiers
have individual reasons in `NON_RECORD_SCHEMAS` in the test, including the
transient execution plan; future version IDs are explicitly separated from
current writers.

## Updating

Run from the repository root:

```sh
uv run --group test pytest tests/test_record_schemas.py
```

When a writer changes, edit its schema in `schemas/`, the matching writer map
and disagreements above, and its flow or valid/invalid fixtures in
`tests/test_record_schemas.py` and `schemas/fixtures/`. Add new persisted version
identifiers to a schema and exercise them with a writer flow or paired fixtures.
For a new CLI/report identifier, add an explicit reason to
`NON_RECORD_SCHEMAS`. Keep both inventory directions passing. Date and date-time
formats are enforced by `FormatChecker`; the test dependency group includes
`rfc3339-validator` for date-time checks.
