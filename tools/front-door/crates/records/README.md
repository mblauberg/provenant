# Records port

Inert library under Rust port rulebook v4 (#1297). No command uses it. Source oracle:
`9797088841644372af432f64b6dd3918bbdd5c19`; J2 fixtures originate in
`d9fcebd4`.

| Graph unit | Contract in this crate |
|---|---|
| `skills/orchestrate/scripts/attempt_evidence.py#unit-01` | Success alias, digest, process/result/adapter agreement and stable refusal ordering |
| `skills/orchestrate/scripts/attempt_storage.py#unit-01` (partial) | Bounded accounting and tmp policy; descriptor-relative deletion has a proposed anchor-escape refusal, awaiting a boundary decision |
| `skills/deliver/scripts/delivery_run_shape.py#unit-01` | Flat receipt field/type validation, gate predicates and insertion-ordered timestamp discovery |
| `skills/orchestrate/scripts/fabric_records.py#interface` | Serde projections, absent/null/value fields, scoped cooldown keys, index identity and store transaction contract |
| `skills/orchestrate/scripts/layout.py#interface` | Resolved discovery inputs, shared paths, explicit-run workspace containment, run kinds, names and typed I/O contract |

`#interface` claims cover typed contracts only. Existing pure helpers are
preparatory implementations; neither file's implementation units are complete.

Delivery receipt values retain JSON object insertion order so timestamp
discovery matches Python's recursive walk. The three gate predicates copied
into this crate are marked pending for `provenant-workflow`; the records crate
cannot depend on workflow under the crate dependency map.

| Later unit | Public implementation work still owned there |
|---|---|
| `layout.py#unit-01` | `run_root` Git/environment discovery; `run_workspace` and `contains_run` resolution/filesystem I/O; `new_run_dir` clock, entropy and `mkdir(parents=True, exist_ok=False)` plus optional `_owner`; `reap_orphans`; `__main__` argument parsing/output |
| `fabric_records.py#unit-01` | `timestamp(datetime) -> str`, `parse_time(str) -> datetime`, `cooldown_path() -> Path`, `route_health_path() -> Path`, `active_cooldowns(values, at=None, *, scopes=None) -> dict`, `render_digest(row) -> str`, `pacing_policy() -> dict`; store primitives, identity and cooldown migration/policy |
| `fabric_records.py#unit-02` | `read_cooldowns`, `read_route_health`, `write_cooldown`, `write_route_health`, `clear_cooldowns`, `append_index` transactions and effects |

`LayoutIo`, `FabricRecordPolicy` and `FabricRecordStore` declare those contracts.
Optional path/time/scope arguments retain Python defaults. Datetime contracts
retain naive versus offset-aware shapes; optional layout kind/slug/owner inputs
select Python defaults. Policy cooldown rows and joined recent history retain
sparse dictionaries. Cooldown backoff uses `serde_json::Number` (i64/u64/f64)
so default `30` remains integer and configured `30.5` remains float.
Route-health reads return the joined
`recent_failures`, `rate_limits` and `cooling_until` fields. Index append accepts
row/run directory/optional root, and cooldown clearing has no timestamp argument.
No trait implementation or command-line entry point is claimed here.

## Checked JSON codec

`checked_json::{from_str, from_slice}` is the workspace codec owner, exported
by `provenant-records`. It returns `serde_json::Value`; schema deserialisation is a separate caller step.
All integer tokens, including ignored/retained fields,
must fit i64/u64; overflow returns typed `NumberOutOfRange`, never an f64
projection. Integer `-0` decodes as zero. Floating tokens retain f64 semantics.
Use `serde_json::Number` for mixed numeric fields so later schema conversion
does not coerce integers into f64. Call this boundary before typed
deserialisation; serde derives alone do not
check unknown fields. Other crates can replace their pending codecs with this
API in the integration pass. No arbitrary_precision feature is enabled.

## Pending integration

Python whitespace uses `provenant_core::py`. `pending::resolve` and its
component queue remain for CPython 3.13+ non-strict symlink-loop handling: the
current core resolver returns an error for loops, while this source returns
the unresolved path. This is a named owner API parity gap, not a path-based
filesystem fallback.

`pending::{EntryInfo, StorageFs}` belong in `provenant-supervisor`. The safe OS
binding must provide owned directory handles, lazy descriptor-relative entries,
no-follow child opens, descriptor metadata, chmod, unlink and rmdir. Storage
policy checks device and inode before deletion, never descends through links,
and leaves foreign-device directories. Anchor opens have the original trusted
path semantics. Directory handles must close on errors. Same-device bind
mounts remain unsupported, as in Python. There is deliberately no path-based
fallback. Storage effect functions stay crate-private until this binding is
available; `keep_private_tmp` is public and pure.

`RunLayout::from_resolved` and explicit-run methods accept paths already
resolved by the discovery owner. The Git probe must strip `GIT_*` environment
variables and retain its five-second timeout. Time and three bytes of entropy
are inputs; records never changes environment variables or generates IDs.

Serde projections retain unowned fields for their later owners. Codec parsing
is separate from schema validation and store migration; it does not substitute
for J2. Scope fields preserve missing versus explicit unknown null, including
when flattened into cooldowns. Opaque cooldown keys retain Python's compact
ASCII JSON bytes, including surrogate pairs for supplementary characters.

## Evidence and remaining gates

Rust unit tests translate retained-attempt policy, storage accounting/removal
policy, Fabric wire/index identity and layout fixtures. Python
differentials cover the success refusal matrix, actual allocated block
metadata/pruning, scoped keys/statuses and slug/non-Git workspace selection and non-strict symlink resolution.
The raw-record validation API preserves Python integer `-0` semantics without
returning a lossy projection for persistence. Decode errors retain typed UTF-8
or JSON causes behind stable policy messages. Differentials use
`PROVENANT_PORT_PYTHON` (default `python3`, >=3.12), print `skip:` when
unavailable/old, and fail instead when `PROVENANT_PORT_DIFF_REQUIRED=1`.
The non-strict loop oracle requires >=3.13. Clocks and entropy are explicit;
there are no sleep assertions.

Before cut-over, J1 still needs direct-command help/output/exit coverage; J2
needs full old/new writer/schema and byte-level sidecar/index publication
coverage; J3 needs Fabric transcript coverage. The pending OS binding also
needs real Linux/macOS descriptor-race, permissions, mount, error and sudden
owner-death conformance. Current storage tests run policy over an owned-handle
filesystem model and a Python metadata snapshot; they do not prove that future
OS binding. Existing Python and TypeScript owners remain in place.

Storage remains partial: review exposed Python's acceptance of `anchor/../victim`
for removal. This inert implementation refuses non-normal relative components;
that Python-bug correction needs the user's approval before cut-over and does not block this inert PR.
See RP-records-003 in the divergence log. Keep storage off the fully ported
unit list until that decision and the pending OS binding are settled.

Lone-surrogate JSON string escapes are refused with a typed JSON cause, including
in ignored validation fields; CPython accepts them. RP-records-006 records this
library divergence for resolution before cut-over. RP-records-005 records the
rulebook v4 improvement refusal for integers outside i64/u64.

## Lane rrc-3 storage boundary

The lane starts at `7021182d535f6c730c29713e77d5bca76893fd06`, including the
safe filesystem foundation from #1328. The storage unit remains partial.
R2 forbids a records-to-supervisor dependency, so the supervisor's `SafeFs`
traits cannot be imported into this crate. A legal shared storage capability
contract/binding is still missing on main. Do not invert that dependency or
add path-based deletion to make the crate callable. The chair must settle the
contract's owner; RP-records-003 also remains a pending boundary decision.
Other units and generic codec ownership can proceed independently.

## Generic owner APIs added by lane rrc-3

Implementation route: `codex/gpt-6.1-sol@high` via Fabric. The copy inventory was
compared with its Python owners; consumer crates remain unchanged in this lane.

| Copy family | Shared owner / disposition |
| --- | --- |
| Checked decode, integer token validation, schema-one, bounded JSON reads | `checked_json::{from_str_as, from_slice_as, check_integer_tokens, schema_one, read_json, ReadJsonError}`; original Value decode API stays stable |
| Python `json.dumps`, ASCII/UTF-8, sort/indent/compact variants | `python_json::{dumps, DumpsOptions, EncodeError}` |
| OS path JSON and telemetry identity | `python_json::{path_json, PathEncodingError, identity}` |
| Ordered dynamic values and Python object diagnostics | `ordered_json::{OrderedValue, OrderedObject, from_str}` |
| Exact mixed numeric comparisons and checked arithmetic | `numeric::{Numeric, NumericError}` |
| Hex and SHA-256 encoding | crate-root `hex`, `sha256_hex`; existing `digest_bytes` delegates to them |
| Missing/null/value generic field | crate-root `Field` re-export, `Field::as_ref`, `From<Option<T>>` |
| Quota reports, telemetry events/usage, run leases/receipts | Stay in their domain crates, as required by the graph |

The JavaScript non-finite preprocessing adapter remains Fabric-runtime policy;
it is not silently folded into the strict Python checked decoder. Domain
numeric caps and domain error-message mapping also stay at their boundaries.
Object order is explicit: use `OrderedValue` when observable, rather than
passing through `serde_json::Value`. The public numeric representation exposes
only i64/u64/f64; checked arithmetic returns a typed range error. Encoder integer
overflow is typed too. Python-compatible floating decode uses lexical parsing
so serde's default decimal rounding does not change exact comparisons.

The new public APIs have translated and differential fixtures covering encoder
variants, ordering, escaping, numeric boundaries, negative zero, token failures,
unknown-field overflow, typed and bounded decoding, and retained diagnostics.
The shared Python interpreter gate uses isolated mode and an explicit
environment. Source boundary limitations for lone surrogates/non-finite decode
remain documented above; no consumer cut-over is performed here.

Sorted NaN object keys are explicitly refused with `EncodeError::NaNKey`.
CPython sorts such keys using a non-total order that Rust's standard sort does
not reproduce. Unsorted NaN keys and non-finite values retain Python spellings.
No inventoried string-keyed consumer depends on NaN-key sorting; RP-RRC3-003
keeps this library boundary visible before any migration requires it.
