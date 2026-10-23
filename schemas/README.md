# On disk record schemas

These files are language independent JSON Schema Draft 2020-12 contracts.
`test_record_schemas.py` validates the checked-in config examples and records
written by hermetic dispatch, batch, quota, and telemetry flows. Test cases are
the actual JSON values paired with the schema selected by the record path or
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
| `run-receipt.schema.json` | `RUN_RECEIPT.json`; initial writer `run_dir_init.sh`, close/update writers in `dispatch_run.py` and `run_dir_finalize.py`. |
| `supervisor-state.schema.json` | Future `supervisor.json` state record, as specified in `docs/specs/harness/rust-supervisor.md`. |
| `supervisor-exit.schema.json` | Future `supervisor.exit.json` terminal record (`provenant.lane.exit.v1`), as specified in `rust-supervisor.md`. |
| `launch-spec.schema.json` | Future `launch.json` record of the `provenant.lane.launch.v1` launch spec, as specified in `rust-supervisor.md`. |
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
| `delivery-profiles.schema.json` | `config/delivery-profiles.json`; loaded by delivery profile handling in `skills/deliver/scripts/delivery_contract.py`. |

## Deliberately open records and gaps

Dynamic records keep `additionalProperties: true`, including adapter output,
attempts, routing configuration, and telemetry rows whose values may be null.
The manifest row's eight columns and the digest-sidecar representation have
`additionalProperties: false`; those shapes are fixed today. Config maps remain
open because route names, roles, profiles, and profile extensions are data.

The two `attempt.json` files intentionally have different contracts: the
canonical live record uses `schema: fabric.attempt.v1`; the compatibility
envelope uses `schema_version` and `record_type: dispatch-attempt`. The
TypeScript launcher creates `dispatch-status.json` before spawn and Python
adds terminal fields when it closes, so its schema accepts the union. Adapter
receipts also vary by provider and execution result, so only their shared
writer fields are required. `dispatch-owner.json` has two current forms:
`record_owner` emits the minimal custody identity, while a launcher-stamped
owner can also carry workspace, start, and log-path fields; those additions
remain optional.

The supervisor state, exit, launch, and observer formats are future records
from the Rust supervisor specification; no current writer exists to produce
them. The spec does not name fields for `supervisor.json`, so its schema stays
an open object; it does enumerate the exit and launch fields. `supervisor.lock`
is an operating-system lock file, not JSON, and has no JSON Schema. Telemetry
rows live in SQLite rather than JSON files; the test validates ingested and
quota-produced rows read back from the ledger. The attempt, status,
adapter-receipt, and owner writer differences above are the writer
disagreements found in this inventory.
