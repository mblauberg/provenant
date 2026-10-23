# Supervisor port

Rulebook: `docs/specs/rust-port/PORTING.md` v4 (PR #1297; `origin/docs/porting-v4`).
Author route supplied by the lane contract: `codex/gpt-6.1-sol@high`.
Source and policy-test oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`;
these sources are unchanged in integrated main
`6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`.
Differential interpreter: `PROVENANT_PORT_PYTHON` (default `python3`, >=3.12);
local evidence uses Python 3.14.8, Unicode 16.0.0.
Toolchain: pinned Rust 1.94.1.

The crate is inert. Existing Python, TypeScript and front-door callers retain
command ownership. The utility runner is separate from S2 provider supervision;
its private child handle can signal only the session it created. Provider
custody, receipt publication and registry integration stay with their owners.

## Unit scope

| Unit | Implementation | Remaining integration or partial scope |
| --- | --- | --- |
| `runtime/fabric/src/process-identity.ts#interface` | Typed Serde identity/group/outcome records, observation/effect types and closed stop reasons | Types and API surface only; executable policy belongs to `process-identity.ts#unit-01` below |
| `runtime/fabric/src/process-identity.ts#unit-01` | Pure identity matching, conservative liveness, group attribution, owner deduplication and a group termination reducer with signal-error fencing | Partial: Fabric registry, run control, publication, reaper and S2 effect adapters remain with the source owners |
| `skills/_shared/bounded_process.py#unit-01` | File-backed merged/separated capture, independent stream bounds, timeout and post-exit group cleanup, public `stop_process_group` over `UtilityChild`, strict spawn/PATH/ordered-environment/pass-fd handling, negative signal status and `argument_error` rendering | Library behavior is implemented; `BudgetArgumentParser` stderr writes and exit(2) belong to the CLI edge. Unrestricted process-group conformance remains required |
| `skills/orchestrate/scripts/process_info.py#unit-01` | Process records, Linux stat/task-tree/boot observations, Darwin libproc/sysctl/Mach observations, tty names, OS argv, shell quoting, canonical local dates and Python finite-float duration formatting | Private pure compatibility helpers await the core integration pass; retained JSON encoding of surrogate-containing OS text is deliberately not claimed |
| `workspace:front-door-syscalls` | Safe strict execve, real-ID access and reentrant passwd lookup, with process fixtures | Partial: root binary still contains its original unsafe calls. Source edits are authorised, but the required root Cargo.toml dependency/link is outside this lane's write scope |
| `skills/orchestrate/scripts/process_info.py#unit-02` | Native process/census/start-time observation with explicit clocks; bounded system-ps fallbacks, required identity, process disappearance and census failures | Partial: fallback output has a 1 MiB bound and typed timeout/I/O errors; exact Python fallback refusal/exception rendering needs integration (RP-1302) |
| `skills/orchestrate/scripts/bin/ps#unit-01` | BSD/POSIX option forms and integer parsing, selection/deduplication/sort, layouts/headers, tty filtering, unreadable/sysctl rows, Unicode visibility, locale/padding, stderr/exit values and quiet broken-pipe writing | CLI owns locale input and SIGPIPE disposition; process-census error rendering inherits the partial observation adapter. No executable is added |

`pending` holds only private pure helpers marked `port: belongs in
provenant-core`: Python whitespace, Unicode 16 decimal/printability tables,
checked integer PID tokens and exact finite-float integer/division formatting.
`decimal_digit` and `python_strip` are copied from main's private CLI pending
helpers; supervisor must not import the CLI crate. Source dates use
`time.localtime` (floor seconds), not `datetime.fromtimestamp`; there is no
float-to-datetime conversion in this slice. Any future such conversion must
use core's CPython modf/half-even helper, or its marked pending copy.
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
(37 values), large durations (20 values), Linux stat rows (5 cases), ps parsing
(18 cases), visibility/large PIDs, canonical dates, locale padding and native
identity at the same PID. A missing or older interpreter emits `skip:`;
`PROVENANT_PORT_DIFF_REQUIRED=1` makes either condition fail. Source failures
always fail. Floating-point assertions compare `to_bits()`. UTF-8, unknown identity, syscall errors, inherited fds,
process replacement and bounded child reaping have direct Rust coverage.

The writer sandbox restricts group signalling and executing setuid system ps.
It does not prove descendant cleanup, full fallback execution, unrestricted
platform conformance, J1 mutant coverage, J2 schema coverage, J3 transcripts or
cross-family acceptance. Those remain cut-over gates under rulebook v4; they
are not reasons to stop an inert port. The chair owns cross-family review.
The repair receives a native adversarial review before its existing PR is updated.

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

The utility adapter reaps its direct child before probing the group, matching
Python's wait/poll semantics and avoiding zombie-only SIGKILL escalation. This
is separate from S2's WNOWAIT provider custody. Final reaping has two 250 ms
waits, with a direct kill between them; exhaustion returns
`BoundedError::Wait(ChildWaitError::Timeout { .. })`. Drop also uses a bounded
best-effort wait, and adds no third wait after normal cleanup exhausts its two.

Integer fields retain integer representation. PID selection accepts i64/u64;
outside that domain, and at narrower OS PID/record boundaries, a typed
`NumberOutOfRange` refuses overflow. The v4 improvement is recorded below
RP-1301 in the divergence ledger. Malformed ps numeric fields return None.

Process lookup, start-time and census APIs carry `Result<Option<_>, ProcessError>`
(or `Result<Vec<_>, _>` for census). Python-style unreadable/malformed rows
remain absent, while `NumberOutOfRange` survives every caller instead of
silently omitting a process. The Linux filesystem fixture covers this lookup
boundary on macOS too.

## Batch 2: descriptor-relative filesystem foundation

`fs::Directory` is an owning anchor and `fs::FileHandle` is an owning file
capability; neither exports a raw descriptor. Relative operations accept one
normal OS component, refusing empty names, `.`, `..`, separators and NULs before
any syscall. The initial anchor/source path is trusted and may contain symlink
ancestors (including macOS `/var`); its final component is never followed.

The API supplies no-follow/cloexec opens, exclusive 0600 creation, mkdirat,
file/directory unlinkat, metadata, independent descriptor enumeration, shared-
offset duplication, bounded reads, file/directory fsync, nonblocking locks,
OS-entropy temporary names, symlink creation and atomic replacement. No-replace
and exchange use Linux renameat2 and Darwin renameatx_np with EXCL/SWAP flags;
unsupported filesystems return the OS error, without an unsafe fallback.
Verified directory opens compare before/opened/after identity and the original
anchor device. Explicit final-link following is a separate opt-in directory operation.

These primitives satisfy the pending confinement custody and records storage
contracts without changing their crates. Policy adapters, recursive traversal,
staging/cleanup transactions and application-specific lease/marker formats
remain with their owners. An untrusted directory writer can still change an
entry between syscalls; no atomic conditional-unlink or checked-rename promise
is made. Directory permissions and held handles remain part of custody.

macOS fixtures exercise anchor replacement by a symlink, no-follow refusal,
concurrent no-replace publication, exchange/replace across held directories,
metadata, duplicate offsets, bounded reads, FIFO refusal and lock contention.
Invalid UTF-8 names are passed unchanged to the OS; when macOS refuses one,
the fixture verifies the same refusal from direct standard-library creation.

`fabric_ps` partially ports `runtime/fabric/src/ps.mjs#unit-01`: system ps first, retry
only spawn EPERM/EACCES/ENOENT while the bundled shim exists, and fallback-only
PATH defaulting. It retains the five-second deadline, ignored stdin/stderr,
Node UTF-8 replacement, raw newlines and typed child failures carrying code,
status, signal and captured stdout. Signal/timeout error evidence has a direct
Node differential. The utility loop has an opt-in spool-size stop so overflow
followed by a running process reports output-limit failure instead of waiting
for the timeout. The Python bounded runner keeps its existing defaults.
The caller provides its installed shim path; no runtime caller switches here.
The unit remains partial: Node's precise maxBuffer overflow capture and a hard
pipe-based cap are not ported. The spool is sampled for overflow; it can grow
beyond the threshold between observations, and retained failure stdout uses
head/tail truncation rather than Node's overflow buffer. This is an open library
mismatch, not an accepted divergence. It blocks completion/cut-over, and no
runtime ps unit is added to the completion ledger.

All supervisor Python differentials now use `-I`, an explicit allowlisted
environment and bounded utility waits; Node uses the same shared helper and
requires version 24. Missing/old interpreters print `skip:` and fail when
`PROVENANT_PORT_DIFF_REQUIRED=1`.

`worker_liveness` continues `worker_liveness.py#unit-01` with typed process,
worker, exit and terminal-report records; terminal evidence/live-writer fences;
positive output growth; advisory liveness; explicit-clock deadline rearming;
Python-compatible duration parsing; POSIX command cwd extraction; and ordered,
cycle-safe wrapper collapse. The inspection precondition rejects a missing
observed PID set before any worktree inspection can occur. Differential cases
cover invalid durations/commands, Unicode digits/whitespace, float bit patterns,
terminal error precedence and exact error text.

This unit remains partial: live cwd/lsof observation, canonical path expansion,
recursive session discovery/checked JSON, git status/repository adapters and
filesystem timestamp metadata are not ported here. Core has no public Python
float/shlex/repr compatibility API yet, so the minimal pure helpers remain in
`pending` with their core-owner marks. This does not add a private timestamp or
JSON codec; records already owns checked JSON and core owns future datetime
conversion. Integration must wire those shared owners before these OS adapters
are completed.

`worker_liveness.py#unit-02` is partial: the pure `collect` reducer and exact
Unicode-aware table `render` are ported. Collection scopes observations by
repository before counting shared cwd users, binds output baselines to PID,
keeps missing sizes as null, labels live worktrees `writer-live`, and reports
stalls only as advisories. It consumes resolved census/log/repository values;
OS collection, CLI argument parsing, JSON edge formatting and main remain.
Tests cover missing baselines/logs, short workers, unchanged/growing output,
shared cwd ambiguity, repository filtering/errors, malformed elapsed values,
null fields and exact differential table bytes (including empty tables).

Batch-2 source/oracle baseline: `7475ff01e437ec51c760b852516c21a5281cf9c6`;
merged main `600dec09bcda3b6a86272e391420e89d38777953` adds the batch ledger,
not source changes. Rulebook v4; author route `codex/gpt-6.1-sol@high`.
The root syscall unit remains partial: exec.rs/resolve.rs are authorised, but
linking the existing supervisor wrappers requires a root Cargo.toml dependency
change outside this lane's write scope. No command is switched here.

Native adversarial batch-2 reviews covered the filesystem custody/syscall
contract and source parity. Review repairs made enumeration a lazy owning
iterator, fixed final-symlink refusal for slash-suffixed anchors, and added
explicit identity-replacement/cross-device refusal fixtures. The filesystem
review is clean. Source-parity review keeps the graph units partial and the
Node maxBuffer mismatch open; the ungraphed filesystem foundation is complete.
The chair remains responsible for cross-family assurance and cut-over judges.
