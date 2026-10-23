# Supervisor port

Rulebook: `docs/specs/rust-port/PORTING.md` v3 (PR #1294).
Author route supplied by the lane contract: `codex/gpt-6.1-sol@high`.
Source and policy-test oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`;
these six sources are unchanged in integrated main
`50972eb5c1d8e208a28c0ef6d11ab0dd5e522814`.
Differential interpreter: Python 3.14.8, Unicode 16.0.0.
Toolchain: pinned Rust 1.94.1.

The crate is inert. Existing Python, TypeScript and front-door callers retain
command ownership. The utility runner is separate from S2 provider supervision;
its private child handle can signal only the session it created. Provider
custody, receipt publication and registry integration stay with their owners.

## Unit scope

| Unit | Implementation | Remaining integration or partial scope |
| --- | --- | --- |
| `runtime/fabric/src/process-identity.ts#interface` | Serde identity/group/outcome types; strict identity match, conservative liveness, attribution, owner-stamp deduplication; explicit stop reasons and a sans-IO group termination interface | The cyclic Fabric registry and S2 effect adapters use these values in later units; this interface does no retained-state I/O |
| `skills/_shared/bounded_process.py#unit-01` | File-backed merged/separated capture, independent stream bounds, timeout and post-exit group cleanup, strict spawn/PATH/environment/pass-fd handling, negative signal status, one-line argument errors | Unrestricted process-group conformance on Linux/macOS remains required; no provider owner is installed |
| `skills/orchestrate/scripts/process_info.py#unit-01` | Process records, Linux stat/task-tree/boot observations, Darwin libproc/sysctl/Mach observations, tty names, OS argv, shell quoting, canonical local dates and Python finite-float duration formatting | Private pure compatibility helpers await the core integration pass; retained JSON encoding of surrogate-containing OS text is deliberately not claimed |
| `workspace:front-door-syscalls` | Safe strict execve, real-ID access and reentrant passwd lookup, with process fixtures | Partial: root binary still contains its original unsafe calls. Replacing/removing those calls is outside this lane's write scope and would be integration/cut-over |
| `skills/orchestrate/scripts/process_info.py#unit-02` | Native process/census/start-time observation with explicit clocks; bounded system-ps fallbacks, required identity, process disappearance and census failures | Partial: fallback output has a 1 MiB bound and typed timeout/I/O errors; exact Python fallback refusal/exception rendering needs integration (RP-1302) |
| `skills/orchestrate/scripts/bin/ps#unit-01` | BSD/POSIX option forms and integer parsing, selection/deduplication/sort, layouts/headers, tty filtering, unreadable/sysctl rows, Unicode visibility, locale/padding, stderr/exit values and quiet broken-pipe writing | CLI owns locale input and SIGPIPE disposition; process-census error rendering inherits the partial observation adapter. No executable is added |

`pending` holds only private pure helpers marked `port: belongs in
provenant-core`: Python whitespace, Unicode 16 decimal/printability tables,
integer PID tokens and exact finite-float integer/division formatting. Main's
similar CLI helpers are private and supervisor must not import the CLI crate.
No additional direct dependencies or workspace features were added.

No intentional command behavior change is accepted here. RP-1301 records the
fixed numeric mismatch; RP-1302 records the remaining fallback refusal domain.
OS argv remains lossless until the JSON edge; ps visibility renders invalid
bytes as Python surrogate escapes. Generic serde emission of non-UTF-8 process
text returns an error rather than silently replacing it.

## Evidence and cut-over gates

Rust policy tests use explicit time values and fixtures; no sleep is an
assertion. Differential fixtures compare capture (36 inputs), runner records
(5 cases, with only measured elapsed seconds excluded), duration/quoting
(37 values), large durations (20 values), Linux stat rows (4 cases), ps parsing
(18 cases), visibility/large PIDs, canonical dates, locale padding and native
identity at the same PID. Python absence skips a differential with a diagnostic;
source failures fail it. UTF-8, unknown identity, syscall errors, inherited fds,
process replacement and bounded child reaping have direct Rust coverage.

The writer sandbox denies group signalling and executing setuid system ps.
It does not prove descendant cleanup, full fallback execution, unrestricted
platform conformance, J1 mutant coverage, J2 schema coverage, J3 transcripts or
cross-family acceptance. Those remain cut-over gates under rulebook v3; they
are not reasons to stop an inert port. The chair owns cross-family review.
A native source-parity review is required before this lane opens its PR.

Run the targeted checks with the pinned toolchain and two jobs:

```sh
cd tools/front-door
cargo fmt --all
cargo clippy -j 2 -p provenant-supervisor --all-targets -- -D warnings
cargo test -j 2 -p provenant-supervisor
```

A sandbox that cannot write Cargo's registry can use an isolated `CARGO_HOME`.
Full checks and the remaining platform matrix run in CI; no live provider is
needed. Consult the PR for exact-head counts and review evidence.
