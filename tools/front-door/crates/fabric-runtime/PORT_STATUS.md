# Fabric runtime port batch 1

Rulebook: PORTING.md version 2. Source and oracle revision:
`e897b10d58b27a46544969594eed08a5ed64c72c` (also the policy-test source).
This crate is inert. No command, provider, live store or installed runtime uses it.

## Complete units

- `runtime/fabric/src/batch-execution.ts#interface`: batch/task and cancellation
  envelopes, with a lossless route-field envelope pending execution-input's port.
- `runtime/fabric/src/execution.ts#interface`: dispatch, launch and owner records,
  fallible launch observation and an instance-owned execution facade contract.
- `runtime/fabric/src/identity.ts#unit-01`: redirect removal, project and worktree
  identity, copied/stale metadata fencing, state/run paths and commit-prefix policy.

The interface units define contracts only. Dispatch, batch execution and
cancellation implementations belong to subsequent graph units.

## Partial units and integration blockers

| Unit | Implemented | Remaining |
|---|---|---|
| `runtime/fabric/src/graph/gates.ts#unit-01` | argv hash, go-line authorisation, order/exit records and receipt decoder | command composition, recorded process-group launch, pipe handshake transport, bounded receipt file reading, reaping and atomic exit publication |
| `runtime/fabric/src/hosts.ts#unit-01` | federation eligibility, operation-ID eligibility, object-shaped owner placement rewrites, cancellation response, bounded stderr byte tail | host transport, UUID allocation, bounded stdout, active checks, group termination and reaping; non-object owner rewrites (reported as a typed incomplete boundary without mutating input) |
| `runtime/fabric/src/json-subprocess.ts#unit-01` | bounded capture, failure precedence, cancellation reason, explicit kill/completion effects, typed response parsing | launch/stdin transport, explicit deadlines, pre-spawn abort, reaping, arbitrary/non-JSON AbortSignal reasons and nullish abort defaults, and response-type integration |
| `runtime/fabric/src/lane-watch.ts#unit-01` | relevant-file filter, wake and debounce decisions with explicit clocks | recursive watch registration/error recovery/cleanup, root refresh and bounded wait transport |

At the source revision, `provenant-supervisor` has no safe OS API, and core,
records, run and fabric-store are empty scaffolds. This batch does not invent a
second provider owner, use unsafe outside supervisor, add an unapproved watch
crate or substitute timer polling for recursive event delivery. It exposes only
the coherent completed policy portions. The private `pending::trim_ecmascript`
helper belongs in `provenant-core` and awaits integration there. No other
cross-crate items were copied. Supervisor-backed integration must complete the
four partial units before they can appear on a PR's fully ported Units line.

No intentional behaviour change is proposed. Generic JSON parsing is not a
JavaScript-number compatibility layer. Successful helper output stays opaque
until the response owner supplies its type. The gate receipt decoder's generic
extension fields also need the numeric compatibility coverage before cut-over.

## Verification boundary

Rust unit tests translate policy from project-identity, fabric, graph-gates,
hosts, json-subprocess and lane-watch tests. They use isolated Git repositories,
fixed collision fixtures and explicit clocks. They do not launch providers.
The identity and hash differential tests invoke actual TypeScript using Node's
type-transform support, skipping if that interpreter is unavailable. The lane
filter additionally compares against the reference JavaScript regular expression.

Crate fmt, clippy and unit-test results are recorded in the PR. J1/J2/J3 and
lane conformance are not claimed by these tests. The corresponding judge owners
must supply boundary coverage before integration or cut-over. CI remains the
exact-head merge gate. The requested native adversarial review covers source
parity; no cross-family review is claimed by this headless lane.

Final local verification: 40 unit tests passed, zero failed, with zero doc tests.
The Node differentials ran on this host. `cargo fmt --all`, crate clippy with
`-D warnings` and the crate test suite passed using pinned Rust 1.94.1 and
`-j 2` for cargo builds. An isolated `/tmp` Cargo cache was needed because the
shared cache is read-only.

The native adversarial reviewer passed the repair delta. Repairs preserve
null-versus-missing input fields, project-root lexical resolution, ECMAScript
trimming and explicit unfinished host rewrite boundaries. Gate file I/O remains
unported, so the decoder introduces no unbounded receipt read or file-size
refusal policy. No full gate, host, helper or watch execution parity is claimed.
