# Records port

Inert library under Rust port rulebook v3. No command uses it. Source oracle:
`9797088841644372af432f64b6dd3918bbdd5c19`; J2 fixtures originate in
`d9fcebd4`.

| Graph unit | Contract in this crate |
|---|---|
| `skills/orchestrate/scripts/attempt_evidence.py#unit-01` | Success alias, digest, process/result/adapter agreement and stable refusal ordering |
| `skills/orchestrate/scripts/attempt_storage.py#unit-01` (partial) | Bounded accounting and tmp policy; descriptor-relative deletion has a proposed anchor-escape refusal, awaiting a boundary decision |
| `skills/orchestrate/scripts/fabric_records.py#interface` | Serde projections, absent/null/value fields, scoped cooldown keys, index identity and store transaction contract |
| `skills/orchestrate/scripts/layout.py#interface` | Resolved discovery inputs, shared paths, explicit-run workspace containment, run kinds, names and no-replace directory creation |

The Fabric interface does not implement the separate `fabric_records.py`
unit-01/unit-02 graph units: digest rendering, identity, pacing, cooldown and
health policy, or transactional filesystem stores. The layout interface does
not implement the separate `layout.py#unit-01` Git probe or orphan recovery.
Those require their owning core, routing, quota, run and supervisor APIs.

## Pending integration

`pending::resolve` and its component queue belong in `provenant-core`. They
supply Python non-strict path resolution until core exports it.

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
policy, Fabric wire/index identity and layout fixtures. Six Python
differentials cover the success refusal matrix, actual allocated block
metadata/pruning, scoped keys/statuses and slug/non-Git workspace selection and non-strict symlink resolution.
The raw-record validation API preserves Python integer `-0` semantics without
returning a lossy projection for persistence. Decode errors retain typed UTF-8
or JSON causes behind stable policy messages. Differentials skip with a diagnostic when `python3` is unavailable. Clocks and entropy
are explicit; there are no sleep assertions.

Before cut-over, J1 still needs direct-command help/output/exit coverage; J2
needs full old/new writer/schema and byte-level sidecar/index publication
coverage; J3 needs Fabric transcript coverage. The pending OS binding also
needs real Linux/macOS descriptor-race, permissions, mount, error and sudden
owner-death conformance. Current storage tests run policy over an owned-handle
filesystem model and a Python metadata snapshot; they do not prove that future
OS binding. Existing Python and TypeScript owners remain in place.

Storage remains partial: review exposed Python's acceptance of `anchor/../victim`
for removal. This inert implementation refuses non-normal relative components;
that Python-bug correction has not been approved as a compatibility change.
See RP-records-003 in the divergence log. Keep storage off the fully ported
unit list until that decision and the pending OS binding are settled.
