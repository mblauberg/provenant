# Fabric runtime port status

Current batch: 3. Rulebook: `docs/specs/rust-port/PORTING.md` version 4 and
ADR 0028. Source/oracle baseline: `a05bcb461f8a599a61637dc97153e65bc315ee81`;
main integrations through `b527d675` retain the same source units. Previous
batch history remains in Git. This status supersedes the batch-1/2 completion
claim for the run-registry interface. The crate is inert: no command has been
switched over and no Python or TypeScript source has been removed.

## Complete in this batch

- `runtime/fabric/src/ps.d.mts#unit-01`: declaration-only process-output
  contract, with caller-supplied output/error types and environment.
- `runtime/fabric/src/run-root.ts#unit-01`: real-path fallback, product root,
  executable-owner checks through supervisor, fixture temp/Git/run/scratch
  fences, exact diagnostics and optional-field presence. Node differentials
  cover successful and refused roots, configured fallback order, executable
  files, directories, links and missing owners. Temporary path assertions
  canonicalise the root for macOS `/var`.

## Review repairs from PR 1318

| Item | Result |
|---|---|
| 1 | Owner read accepts missing/mistyped `run_dir`, overwrites it, validates PID/PGID > 1 including integral floats, retains null/mistyped extras, and preserves top-level property order. Interface remains withdrawn for nested ordered JSON, lone surrogates and unsafe-integer semantics (RP-FR3-003). |
| 2 | Node fixtures cover missing directory, null IDs, rejected PID 1, integral floats, schema failures, extra-field rewriting and raw wire order. A separate mismatch fixture documents 9007199254740993. |
| 3 | Policy and dangling-link tests compare canonical temporary roots. |
| 4 | Policy readers validate grammar while leaving unread values opaque; unread large integers, non-finite numbers and lone surrogates do not poison independent fields. Selected exotic values remain partial. |
| 5 | Uses core `python_value_repr`; quote/control/exponent fixes are shared. Ordered dictionaries and exotic selected values still require a lossless decoder; no repr copy remains. |
| 6 | Removes invented resolve diagnostic, omits absent fixture fields, and tests fixture success/failure, fallback, product/executable root and scratch behaviour against Node. |
| 7 | Deletes local repr, path-resolution and checked-JSON copies in favour of core/records owners. ECMAScript trim remains pending because core has no equivalent. |

Deliberate ledger removal: `runtime/fabric/src/run-registry.ts#interface`.
Its source contract is broader than the available generic JSON value owner;
passing ordinary record fixtures is insufficient to restore the marker.

## Partial units

Each row names implemented policy and what still prevents the whole unit or
interface from being claimed. These IDs are absent from this batch's Units line.

| Unit | Implemented | Remaining owner/API or boundary |
|---|---|---|
| `runtime/fabric/src/hosts.ts#unit-01` | eligibility, placement rewrites including arrays/BMP strings/scalars, cancellation response and bounded stderr tail | supervisor has no public pipe/stdin/live-child custody transport; UUID allocation and active/cancel/reap integration remain; UTF-16 lone-surrogate strings need a records value owner |
| `runtime/fabric/src/json-subprocess.ts#unit-01` | capture bounds, failure precedence, pre-abort, explicit deadlines and first-failure/cancel effects | same missing pipe/stdin/live-child custody API; arbitrary non-JSON AbortSignal reasons need an edge representation |
| `runtime/fabric/src/lane-watch.ts#unit-01` | file filter and explicit-clock debounce/wake decisions | supervisor has no recursive filesystem event watcher, registration/recovery/cleanup or bounded watch wait API |
| `scripts/fabric_git.py#interface` | transport request/result records, metadata validation, verification receipt extension fields and transport trait | agreed run-owner writer-lease, repository proof and complete dynamic verification receipt contract; current run lease API is policy, not the federation owner |
| `scripts/fabric_hosts.py#interface` | peer/config/placement records, exact boolean response envelopes, protocol decision and peer transport/client traits | full config/default/doctor/client shapes, Python lossless strict JSON and supervisor SSH pipe execution; this is a partial contract, not an invented full interface |
| `scripts/fabric_lanes.py#interface` | verb/write table, transient/path fields, operation-ID policy and federation trait | fabric-store federation pending-operation/lock/history transaction contracts plus Git writer proof; no store implementation or second store schema is copied |
| `skills/orchestrate/scripts/fabric_policy.py#unit-01` | defaults, instruction policy, exclusion containment and warnings using shared paths/JSON/repr | records has no ordered lossless Python value decoder; selected exotic tools/exclusions and nested dictionary repr remain unresolved (RP-FR3-002) |
| `runtime/fabric/src/run-registry.ts#unit-01` | owner/provider codecs, retention policy with explicit observations/clocks, supervisor safe bounded regular-file read adapter | full durable publication/list/prune/signal/process-start-identity integration; generic JS JSON wire semantics remain unresolved; bounded read refusal is explicit (RP-FR3-004) |
| `runtime/fabric/src/catalogue.ts#unit-01` | typed snapshot/adapter/probe records, provider-listing table/trait and bounded live-model digest | run/config has no replacement catalogue snapshot/cache/YAML/cooldown owner; listing invocation needs pipe transport; split-surrogate truncation needs lossless JS strings |
| `runtime/fabric/src/execution-input.ts#unit-01` | adapter/mode/tier tables, UTF-16 edit distance, alias/explicit-route selectors and canonical mode policy | full `correctSelector`/`normaliseRoute`, prompt-file/read-root/worktree authority and preflight depend on the missing catalogue and run preflight owner; no full route-normalisation claim |
| `runtime/fabric/src/execution-input.ts#unit-02` | prompt exclusivity, timeout defaults, field-local ordered dispatch defaults, default application and ordered argv expansion | working-identity/project/read-root resolution and preflight integration; nested ordered JSON/unsafe numbers/lone surrogate argv values need records codec (RP-FR3-003) |
| `runtime/fabric/src/pools.ts#unit-01` | native-first and knob policy, pool/council expansion, precedence, error/warning propagation, ordered owner requests and picker trait | actual routing requests await run's replacement `model_route.py` owner and supervisor pipe transport; fake owner only exercises the policy protocol |
| `runtime/fabric/src/graph/document.ts#unit-01` | graph/node IDs, canonical sorted JSON, attempt hash and fault rendering | complete shape/authority/route validation needs execution-input/run owners; entropy and lossless JS number/string codec remain missing (RP-FR3-003) |
| `runtime/fabric/src/graph/document.ts#unit-02` | FIFO topological order, cycle faults, heights, confidentiality propagation/downgrade and writer ancestry order | complete guarded-prompt/path/read-root/worktree validation needs run's graph preflight/linked-worktree proof contracts; graph-analysis tests filter only implemented faults |

The existing graph-gates unit remains partial: argv authorisation/hash and
receipt policy are present, but gate pipe handshake, process custody, atomic
exit publication and full receipt-number semantics remain unported. The landed
supervisor filesystem API is used by run-root and the record read adapter; it
does not supply these process/watch/authority APIs.

## Dropped unit

`runtime/fabric/src/python-owner.ts#unit-01` has graph disposition `dropped`.
It is not ported and has no completion marker. Python helper invocations have
not been promoted into a second runtime owner.

## Pending and extension seams

Private `pending` contains only items whose owner has no equivalent public API:

- `trim_ecmascript` belongs in core; Python strip is not the same operation.
- `OrderedMap<T>` belongs in records; preserves insertion/replacement order.
- `JsNumberBoundary`, `js_json_number` and `js_json_value` belong in records;
  finite-number spelling is shared locally, but this is not a lossless JS codec.

Each is marked with its intended owner in source. There are no pending copies
of Python repr, lexical path resolution or checked JSON. Generic extension
fields still use JSON values and are expressly incomplete when key order or
JavaScript/Python exotic values are observable. No pending fabric-store
implementation is needed. Hosts, Git transport, federation, catalogue listing,
pool routing and process-output seams use traits. Variant behaviour is held in
single policy tables where the source varies.

## Verification and limits

All oracle tests share bounded capture/deadlines, an explicit environment and
`PROVENANT_PORT_NODE` / `PROVENANT_PORT_PYTHON` selection. Python uses `-I`.
Missing interpreters print `skip:` and fail in required-differential mode.
Tests import actual sources; fixture generators do not exec/eval source text.
Policy tests use fixtures and explicit clocks, with no sleeping assertions.

Final checks and exact counts are recorded in the PR. Required local checks:
`cargo fmt --all`, crate clippy with `--all-targets -- -D warnings`, and
`PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-fabric-runtime` on
pinned Rust 1.94.1. Full harness checks belong in GitHub Actions. Native
adversarial review is source-parity evidence, not a cross-family review or
J1/J2/J3 cut-over acceptance. The chair controls cross-family review and merge.

Verified after the main integration: 81 unit tests passed, zero failed, and
zero doc tests. Node and Python oracles ran in required-differential mode.
Fmt, clippy and both R2 layering checks passed. The retained unsafe-integer
fixture deliberately proves the documented mismatch; it is not parity evidence.
