# Provider wire recordings — build step 0

These are sanitised projections of existing recordings for the five retained
providers in [execution-route.md](../../../../../docs/specs/rust-port/design/execution-route.md).
They support future decoder tests. **Every execution mode and quota source in
[activation.json](activation.json) is disabled.** No capture proves the complete
invocation and confinement contract. This file is evidence for the later static
registry, not runtime configuration or permission to execute a provider.

[manifest.json](manifest.json) records each fixture's source, SHA-256, known
CLI version, session/access mode, process exit, substitutions and limitations.
`null` means unrecorded, never an inferred default. Exact argv and platform are
unrecorded in every promoted source. Local receipt exits and access modes are
reported as receipt evidence, not independent proof of confinement. Claude's
version is from its stream; OpenCode's major version remains unverified, so its
streams cannot establish v2 acceptance.

## Coverage and activation

| Adapter | Recorded decoder evidence | Read-only fresh/resume | Writer fresh/resume | Quota |
| --- | --- | --- | --- | --- |
| Claude | Success, init/answering model mismatch, matching model, continuation pair, authentication error with success subtype | Disabled | Disabled | Disabled |
| Codex | Success/usage, structured failure, interrupted prefix without terminal | Disabled | Disabled | Disabled |
| Cursor | Success, thinking deltas, continuation pair | Disabled | Disabled | Disabled |
| Agy | Init/step updates/success and error result | Disabled | Disabled | Disabled |
| OpenCode (version unknown) | Text/stop, error, intermediate tool-step finish followed by error and nonzero exit | Disabled | Disabled | Disabled |

The tracked fresh/resume labels follow the existing continuation-test pairs;
matching session placeholders preserve that relationship. They do not prove
accepted resume argv. OpenCode `step_finish` with `reason: tool-calls` is an
intermediate step, not terminal success. A zero receipt exit alone proves
neither an answering identity nor a safe invocation.

Enable a mode only after a recording proves its exact selector, effort,
default service controls where applicable, CLI version/platform, accepted
fresh/resume invocation and access/confinement form. Independent disclosure and
confinement readiness remain required. Read-only evidence never enables a
writer. A missing adapter/mode defaults to disabled. Quota also needs recorded
windows/resets, missing data, 401/429 and Retry-After plus runtime authority;
these fixtures grant no credential access.

Recovered retry, nested tool/subagent identity, malformed protocol and quota
cases remain gaps. The later decoder tests must add explicitly synthetic
negative inputs for gaps without calling them CLI acceptance recordings. The
partial Codex fixture keeps only the interrupted stream's initial protocol
records; tool payloads are omitted. No malformed bytes or user content from a
local stream are copied.

## Public-data boundary

Only the named tracked JSONL streams and bounded local `events.jsonl` streams
with companion `adapter-receipt.json` were read. Local event streams over 1 MiB
were skipped. No live provider, network probe, prompt/result file, credential
file, auth store or real harness configuration was used. Source files were not
modified. Tracked stream provenance comes from commits `38b234f2` and
`b9b824fc`; listing seeds and synthetic `fabric-v1` records are not execution
acceptance evidence. Kiro is outside this design.

All non-allowlisted strings are replaced in memory. The closed allowlist keeps
only protocol enum values, public model IDs and recorded CLI versions. Empty
strings, booleans and nulls retain their wire meaning. Measurements, costs,
durations and numeric timestamps are replaced by zero with their JSON number
form retained: these fixtures test shape, not measured amounts or date logic.
Decimal numbers here are external JSON wire forms, not Rust numeric models.

Stable `FIXTURE_<CLASS>_<NUMBER>` substitutions preserve equal string values
across the corpus. Classes cover session, request/message/event identifiers,
filesystem paths/snapshots, timestamps, arbitrary text, unclassified strings
and dynamic keys. All answer, prompt, error-description and thinking text is
synthetic placeholder text. Local source locators also replace run, task and
attempt identifiers. The manifest stores substitution classes, never original
values or a reversal map. Local Claude init inventories and all local tool
events/payloads are omitted; other per-capture projections are documented.

Step 0 adds no Rust implementation or fixture-loading helper. The design's
snapshot helper belongs with the actual decoders. No Python/TS owner is replaced
here; the deletion gates in steps 1–4 still apply.

Author route: `codex/gpt-6.1-sol@high` via Fabric.
