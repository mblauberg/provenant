# Breaking changes from v1.11.2

## Breaking changes

### CLI and install

The command and option spellings below appeared in the v1.11.2 command surface or its legacy repair hints; their current replacements are listed. Work-facing JSON identity uses format-2 WorkKey, whose CLI target spelling is `GRAPH/NODE#N`. Reading internal run files has no supported replacement.

- `dispatch`: provenant run TEMPLATE BRIEF. (#3078)
- `lanes`: provenant status. (#3078)
- `resume`: provenant retry NAME/NODE. (#3078)
- `cancel`: provenant stop NAME. (#3078)
- `continue`: provenant retry NAME/NODE#N. (#3078)
- `redo`: provenant retry NAME/NODE. (#3078)
- `preset`: provenant templates. (#3078)
- `review`: provenant run review BRIEF. (#3078)
- `stats`: provenant telemetry cost --by node. (#3078)
- `pr land`: provenant land NUMBER --wait. (#3078)
- `fabric graph`: provenant status NAME. (#3078)
- `fabric mcp`: provenant fabric. (#3078)
- `fabric_graph`: provenant run TEMPLATE BRIEF. (#3078)
- `quota usage`: provenant telemetry cost --by account. (#3078)
- `config migrate`: provenant config check. (#3078)
- `telemetry backfill`: nothing; this binary reads no format-1 store. (#3078)
- `dispatch --prompt-file`: provenant run TEMPLATE --brief-file PATH. (#3078)
- `dispatch --prompt-stdin`: provenant run TEMPLATE --brief-file -. (#3078)
- `resume --no-fallback`: provenant retry NAME/NODE#N. (#3078)
- `run --prompt-file`: provenant run TEMPLATE --brief-file PATH. (#3078)
- `run --prompt-stdin`: provenant run TEMPLATE --brief-file -. (#3078)
- `retry --no-fallback`: provenant retry NAME/NODE#N. (#3078)
- `retry --prompt-file`: provenant retry NAME/NODE --brief TEXT. (#3078)
- `retry --prompt-stdin`: provenant retry NAME/NODE --brief TEXT. (#3078)
- `retry --operation-id`: no direct equivalent; use `provenant retry NAME/NODE#N`. (#3078)
- `output --host`: no direct equivalent; use `provenant output NAME`. (#3078)
- `land --batch`: provenant land NUMBER [NUMBER ...]. (#3078)
- `run --use`: provenant run TEMPLATE BRIEF. (#3078)
- `run --tier`: provenant run TEMPLATE BRIEF --route ROUTE. (#3078)
- `run --operation-id`: provenant run TEMPLATE BRIEF --name NAME. (#3078)
- `run --wait`: provenant wait NAME. (#3078)
- `status --wait`: provenant wait NAME. (#3078)
- `status --target`: provenant status NAME or provenant wait --pr NUMBER. (#3078)
- `status --until`: provenant wait NAME --any. (#3078)
- `status --follow`: provenant wait NAME --follow. (#3078)
- `status --stats`: provenant telemetry cost NAME --by node. (#3078)
- `status --by`: provenant telemetry cost NAME --by node. (#3078)
- `status --id`: provenant status NAME. (#3078)
- `stop --operation-id`: provenant stop NAME. (#3078)
- `stop --wait`: provenant wait NAME. (#3078)
- `retry --node`: provenant retry NAME/NODE. (#3078)
- `retry --wait`: provenant wait NAME. (#3078)
- `output --trace`: provenant trace NAME. (#3078)
- `reconcile --id`: provenant reconcile NAME. (#3078)
- `telemetry stats --run-id`: provenant telemetry cost NAME. (#3078)
- `telemetry stats --by`: provenant telemetry cost --by route. (#3078)
- `telemetry stats --detail`: provenant telemetry cost NAME. (#3078)
- `telemetry trace --run`: provenant telemetry trace NAME. (#3078)
- `pr status --wait`: provenant pr watch NUMBER. (#3078)
- `steer --text-stdin`: provenant steer TARGET --brief-file -. (#3078)
- `PROVENANT_BUILD_WAIT_SECS` and `PROVENANT_BUILD_WAIT_MARKER`: removed; busy build slots now proceed without waiting. (#3078)
- `run_id`, `task_id` and `operation_id` work selectors and JSON identity fields: use the format-2 WorkKey; its CLI target spelling is `GRAPH/NODE#N`. (#3078)
- Direct reads of internal run files: no supported replacement; use the CLI output interfaces. (#3078)

### Lanes and settlement

- `domains.fabric.limits.lease_seconds`: remove this obsolete key; custody records replace the old lease mechanism. (#3078)
- `.provenant/verify.json`: move verification declarations to the versioned `.provenant/ports.json` table; each tier declares its selection and host-load policy. (#3078)
- `verify-tiers.yml` dispatch `tier=lane`: use `task=lane`; configured tier names, including `lane`, use `task=tier`. (#3078)
- `domains.fabric.lanes.build_wait_minutes`: build slots try once and proceed; Cargo manages its own locks. (#3078)
- `lanes.slot_wait_seconds`: build-slot waits and their wall-clock extension have been removed. (#3078)
- `lane.max_continuations`: no direct equivalent; use graph `limits.continuations` to bound restarts. (#3078)
- `lane.resume_continuations`: no direct equivalent; automatic restarts use fresh sessions; choose the node `on_stop` policy. (#3078)

### Accounts, quota and routing

- `knobs.domains.routing.capacity_cooldown_seconds`: set `capacity_cooldown_seconds` in `[domains.routing]`. (#3078)
- `knobs.domains.routing.settlement_retries`: set `settlement_retries` in `[domains.routing]`. (#3078)
- `knobs.domains.routing.capacity_retry_seconds`: set `capacity_retry_seconds` in `[domains.routing]`. (#3078)
- `knobs.domains.routing.capacity_retry_max_seconds`: set `capacity_retry_max_seconds` in `[domains.routing]`. (#3078)
- `knobs.domains.routing.retry_delay_ceiling_seconds`: set `retry_delay_ceiling_seconds` in `[domains.routing]`. (#3078)

### Pipelines and graphs

- `domains.fabric.pipelines`: no direct equivalent; delete this stale pipeline table. (#3078)
- `domains.fabric.graph.writer_launch_seconds`: use `domains.fabric.graph.launch_wait_seconds` for launch and wait, and `domains.fabric.graph.writer_provision_seconds` for writer provisioning. (#3078)

### Telemetry and providers

- Usage attribution now carries verified identity, epoch and evidence source; unresolved identities report `unattributed`. Account groups use `provider/account_key` rather than directory-derived labels. (#3078)
