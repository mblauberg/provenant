# Supervisor port slice

Rulebook: `docs/specs/rust-port/PORTING.md`, version 2.
Author route supplied by the lane contract: `codex/gpt-6.1-sol@high`.
Oracle and original policy-test revision:
`5c48d4e5567c4801b072bbf88203e88822079ab1`.
Main integrated after PR #1289:
`e897b10d58b27a46544969594eed08a5ed64c72c`.
The six source units and their policy tests are unchanged between those revisions.

This is an inert partial port. No command imports this crate yet. No lifecycle
owner, signal sender, process observer, retained-state writer or compatibility
fallback is installed here. Existing Python, TypeScript and front-door callers
remain the owners. The graph is unchanged.

## Delivered and remaining

| Unit | Delivered | Remaining before full-unit acceptance |
| --- | --- | --- |
| `runtime/fabric/src/process-identity.ts#interface` | Serde identity/group/outcome types; explicit observation values; strict match, conservative liveness and group attribution policies | S2 observer/cyclic owner interface agreement; registry/owner identities and termination effects; J2 and lane acceptance |
| `skills/_shared/bounded_process.py#unit-01` | Bounded file-backed capture; result fields and signal-status convention | S2-owned spawn/session/pass-fd/cleanup loop, merged/separated spool setup, CLI argument errors; J1/J2/lane judges |
| `skills/orchestrate/scripts/process_info.py#unit-01` | Process records, Linux stat decoder, lossless Darwin argv decoder, shell quoting, CPU/elapsed formatting | Linux tree traversal, native Darwin libproc/sysctl/task observations, tty names, boot/timebase/local clock adapters; numeric range completion |
| `workspace:front-door-syscalls` | Safe strict execve, real-ID access and reentrant passwd lookup | Replace root binary's unsafe calls, remove its copies and prove root/exec parity; outside this lane's write scope |
| `skills/orchestrate/scripts/process_info.py#unit-02` | Eight-column fallback ps decoder and complete ProcessInfo field representation | Native process/census/start-time observation, bounded fallback commands, census errors and explicit-clock construction |
| `skills/orchestrate/scripts/bin/ps#unit-01` | BSD/POSIX option forms, selection, layouts, columns, header suppression and syntax error values | Unbounded/Unicode PID integers; process selection/deduplication/sorting, unreadable/sysctl rows, visibility, locale/padding, rendering, stderr/exit/SIGPIPE edge |

No unit is claimed fully ported. No `pending` items were needed: the delivered
modules depend only on the standard library and allowlisted workspace crates.
No intentional behavior changes are proposed. The bounded numeric portions
are incomplete, tracked as open mismatches in `divergences.md`.

## Evidence and gates

Rust tests compare Python capture (36 fixtures), duration/quoting (37 values)
Linux stat rows (4 fixtures), and ps parsing (18 argument cases) against the source oracle. They skip with a
diagnostic only when `python3` is absent; oracle failures fail the test. No
volatile fields are stripped. Fixture cases use explicit time values and no
sleep assertions. OS tests cover access, NUL refusal, ENOEXEC without shell
fallback, successful process replacement, exact environment and account lookup.
The exec success fixture runs in a child test process and is reaped by its
parent; no provider is run or signalled.

J1's `tests/golden` runner was absent at integration. J2's schemas arrived with
main; `schemas/README.md` does not map these transient data values to a schema.
The existing S2 implementation still lives in the binary's `src/lane` modules;
it has not exposed the observer/custody library contract this slice needs.
This PR does not claim black-box, mutant, lane-conformance, Linux execution or
cross-family acceptance. The chair owns the missing independent judge coverage
and cross-family review before full-unit completion or command cut-over.

Locally use the pinned toolchain and two build jobs:

```sh
cd tools/front-door
cargo fmt --all
cargo clippy -j 2 -p provenant-supervisor --all-targets -- -D warnings
cargo test -j 2 -p provenant-supervisor
```

The sandbox cannot write the default Cargo registry cache. This lane used an
isolated `CARGO_HOME=/private/tmp/provenant-supervisor-cargo`; dependencies and
the lockfile still come from the workspace allowlist. Full checks run in CI.
The installed Provenant root was observed at `/Users/mikey/Repos/provenant`;
this mutable checkout was not used as an immutable oracle or modified by the lane.
