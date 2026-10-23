# Supervisor port

Rulebook: `docs/specs/rust-port/PORTING.md` v4 (PR #1297; `origin/docs/porting-v4`).
Author route supplied by the lane contract: `codex/gpt-6.1-sol@high`.
Source and policy-test oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`;
these sources are unchanged in integrated main
`6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`.
Differential interpreter: `PROVENANT_PORT_PYTHON` (default `python3`, >=3.12);
local evidence uses Python 3.14.8, Unicode 16.0.0.
Toolchain: pinned Rust 1.99.1.

The crate is inert. Existing Python, TypeScript and front-door callers retain
command ownership. The utility runner is separate from S2 provider supervision;
its private child handle can signal only the session it created. Provider
custody, receipt publication and registry integration stay with their owners.

## Unit scope

| Unit | Implementation | Remaining integration or partial scope |
| --- | --- | --- |
| `runtime/fabric/src/process-identity.ts#interface` | Typed Serde identity/group/outcome records, observation/effect types and closed stop reasons | Types and API surface only; executable policy belongs to `process-identity.ts#unit-01` below |
| `runtime/fabric/src/process-identity.ts#unit-01` | Pure identity matching, conservative liveness, group attribution, owner deduplication and a group termination reducer with signal-error fencing | Partial: Fabric registry, run control, publication, reaper and S2 effect adapters remain with the source owners |
| `skills/_shared/bounded_process.py#unit-01` | File-backed merged/separated capture, independent stream bounds, timeout and post-exit group cleanup, public `stop_process_group` over `UtilityChild`, strict spawn/PATH/ordered-environment/pass-fd handling, negative signal status and `argument_error` rendering | Complete; fallible parser stderr writes return exit status 2. Native descendant cleanup fixtures cover timeout and successful parent exit |
| `skills/orchestrate/scripts/process_info.py#unit-01` | Process records, Linux stat/task-tree/boot observations, Darwin libproc/sysctl/Mach observations, tty names, OS argv, shell quoting, canonical local dates and Python finite-float duration formatting | Private pure compatibility helpers await the core integration pass; retained JSON encoding of surrogate-containing OS text is deliberately not claimed |
| `workspace:front-door-syscalls` | Safe strict execve, real-ID access and reentrant passwd lookup, with process fixtures | Complete in batch 3: root binary links and calls the safe wrappers |
| `skills/orchestrate/scripts/process_info.py#unit-02` | Native process/census/start-time observation with explicit clocks; bounded system-ps fallbacks, required identity, process disappearance and census failures | Partial: fallback output has a 1 MiB bound and typed timeout/I/O errors; exact Python fallback refusal/exception rendering needs integration (RP-1302) |
| `skills/orchestrate/scripts/bin/ps#unit-01` | BSD/POSIX option forms and integer parsing, selection/deduplication/sort, layouts/headers, tty filtering, unreadable/sysctl rows, Unicode visibility, locale/padding, stderr/exit values and quiet broken-pipe writing | CLI owns locale input and SIGPIPE disposition; process-census error rendering inherits the partial observation adapter. No executable is added |

`pending` holds only private pure helpers marked `port: belongs in
provenant-core`: Python whitespace, Unicode 16 decimal/printability tables,
checked integer PID tokens and exact finite-float integer/division formatting.
`decimal_digit` and `python_strip` originated in the CLI pending helpers;
supervisor must not import CLI. Core now exports whitespace/strip/repr helpers,
but their migration awaits the workspace dependency declaration described below. Source dates use
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
runtime ps unit is added to the completion ledger. Platform-specific signal
name extensions also remain outside the current common-POSIX mapping.

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
filesystem timestamp metadata are not ported here. Core now supplies public
whitespace/strip/repr APIs, but their integration
requires a root workspace dependency entry outside this lane's write scope.
The marked copies remain pending that declaration. Float/shlex helpers still
have no public owner API. This does not add a private timestamp or
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

Main integration `d0027d7cd8de987b15c2f7c855691ca3ddf488aa` adds public core
Python compatibility helpers. The root workspace has no `provenant-core`
dependency entry; this lane permits only inherited dependencies and cannot
edit that manifest. Replacing the corresponding marked `pending` copies is
blocked on that declaration. No path dependency bypass is introduced.

Merged main again at `d8be7244ada4b5e7a9dadc565a7cab292e54890f`; its changes
do not alter the supervisor source/oracle fixtures.

## Filesystem consumer adapters (PR #1328 repair)

Author route: `codex/gpt-6.1-sol@high`. `fs::{SafeFs, SafeDir, SafeFile}`
are public, object-safe, `io::Result` contracts; `LocalFs`, `Directory` and
`FileHandle` implement them. Consumers may inject `&dyn SafeFs` and use
`Box<dyn SafeDir>` / `Box<dyn SafeFile>` as associated handle types, or use
concrete handles. Tests can implement these traits without descriptors.
`SafeFile` includes `Read + Write + Seek`; its duplicate shares the file offset.
The trait publication methods operate within one held directory. Concrete
`Directory` also supports rename/replace/exchange between held directories.

The pending-contract inventory comes from
`rg -n 'belongs in provenant-supervisor' tools/front-door/crates`.
No consumer crate is changed in this repair.

| Pending owner / contract | Supervisor adapter mapping |
| --- | --- |
| confinement `FileKind`, `Metadata` | Convert `fs::FileKind` variants and copy matching metadata fields; supervisor additionally supplies `allocated`. |
| confinement `CustodyFs` initial access | `root`, `open_source`, `temporary_name` delegate to `SafeFs`; `LocalFs` supplies the OS implementation. |
| confinement `CustodyFs` directory operations | `directory_metadata` → `SafeDir::metadata`; `metadata_at` → `metadata_at`; `open_directory(dir, name, follow)` → `open_directory(name, follow)`; `open_read`, `create_new`, `sync_directory`, `replace`, `unlink` → corresponding directory methods (`sync_directory` uses `sync`). |
| confinement `CustodyFs` file operations | `duplicate`, `file_metadata`, `sync_file` → `SafeFile::{duplicate, metadata, sync}`. Boxed file handles implement the required read/write/seek traits. |
| records `EntryInfo` | Copy device/inode/links/allocated from `fs::Metadata`; derive `directory` and `symlink` from `kind`. |
| records `StorageFs` | `open_anchor` → `SafeFs::open_anchor`; `info` → `SafeDir::metadata`; `stat` → `metadata_at(name, false)`; `open_child` → `open_verified_directory`; `entries`, `make_writable`, `unlink`, `rmdir` → corresponding directory methods. `Entries` can be `Box<dyn Iterator<Item = io::Result<OsString>>>`. |
| config/core `atomic_write` | Open the trusted parent once, then call `Directory::atomic_write(name, prefix, bytes, PublishMode::Replace)` (or its `SafeDir` method). Core's argument order differs; the adapter reorders it. Config retains its parent-creation policy; supervisor takes a held directory and does not create ancestors. |
| config `replace_link` | Open the trusted destination parent, then call `replace_link(destination_name, source_path)`. Target bytes are stored verbatim. |
| run `read_bounded`, `publish_lease`, `lock` | `Directory::read_bounded`; `atomic_write` with replace policy; held `FileHandle::{try_lock, unlock}` with caller-owned retry/deadline and create/open policy. This does not implement run's blocking append/read lock helper verbatim. |
| run `OwnerCustody`, `ProcessProbe`, `WorkerExit`, `SessionWorker`, `SessionHost`, `publish_marker` | Still pending. Existing identity reducers, process observations and bounded utility runner are not drop-in S2/session custody adapters. Marker naming/permissions and no-fsync semantics require their own adapter; durable `atomic_write` is not a parity claim for markers. |
| fabric-store `IdSource` | UUID formatting remains pending. `temporary_name` supplies 96 bits of entropy for staging names, not a UUID implementation. |

`FsError` exposes `is_not_found`, `is_exists`, `is_symlink` / `is_loop`,
`is_cross_device`, `is_unsupported` and `is_not_directory`. Unsupported errno
classification includes EINVAL, ENOTSUP/EOPNOTSUPP and ENOSYS, including flagged
rename refusals. No check-then-rename fallback is introduced. A no-follow open
that returns ENOTDIR/ELOOP is classified with `fstatat(AT_SYMLINK_NOFOLLOW)`;
a positively observed final symlink yields `FsError::Symlink` and canonical
ELOOP on both Linux and macOS. A regular non-directory keeps ENOTDIR.
`From<FsError> for io::Error` returns the underlying OS error, preserving errno
and kind. Inspect the typed error before conversion for operation/path context;
standard `io::Error` cannot retain both a raw errno and a custom context payload.
Nested handle errors carry the full anchor-relative path, recorded only for
diagnostics and never used to reopen a held capability.

`atomic_write` exclusively creates a mode-0600 staging file in the held
parent, writes and fsyncs it, performs no-replace or replacement rename, then
fsyncs the parent. `replace_link` stages a fresh link, replaces the destination
and fsyncs the parent. Both require trusted directory writers; failures before
publication attempt staging cleanup while preserving the primary error. A
post-rename directory-sync failure means publication is visible but durability
is unconfirmed. The caller chooses the publication policy explicitly.

`open_verified_directory` refuses observed stat/open/stat identity differences
and crossing the original anchor device (typed `CrossDevice`, EXDEV). It cannot
detect ABA replacement or prevent mutation after the last stat. Regular-file
verification precedes clearing O_NONBLOCK; FIFO opens still cannot block waiting
for a writer. Open/mkdir/rename/unlink retry EINTR in the syscall layer.
`sys::errno` supplies named platform constants to callers, including fabric ps.
Directory-stream ownership transfers with `into_raw_fd` after fdopendir succeeds.

Local repair evidence: `cargo fmt --all`, supervisor clippy with warnings denied,
and the required supervisor differential test command. The installed Rust
targets omit `x86_64-unknown-linux-gnu`; Linux compilation and execution remain
CI gates. No graph unit or consumer cut-over is newly claimed.

## Batch 3: shared owners, front-door syscalls and observation adapters

Rulebook **v4**, author route **`codex/gpt-6.1-sol@high`**. Source/oracle and
judge checkout: `7021182d535f6c730c29713e77d5bca76893fd06` (all Python/TS
sources remain unchanged). Pinned Rust 1.94.1; local Python 3.14.8 and Node
24.21.0. This section supersedes the earlier workspace-link blockers.

Fully ported in this batch:

- `skills/_shared/bounded_process.py#unit-01`: the parser error effect now
  writes stderr fallibly and returns status 2 for the embedding CLI to exit.
  New handshake/EOF fixtures prove same-group, SIGTERM-ignoring descendants
  are cleaned after both timeout and normal parent exit. No sleep is an
  assertion; the native parent intentionally abandons its child for this test.
- `workspace:front-door-syscalls`: root `exec.rs`/`resolve.rs` now call the
  supervisor's safe wrappers. Strict ENOEXEC refusal, OS byte strings,
  environment and argv forwarding, root precedence and passwd fallback retain
  their existing behavior. Root-package tests cover the actual linked binary.

The crate now depends on the shared `provenant_core::py` and
`provenant_records::checked_json` owners. Duplicate whitespace, strip and repr
implementations were deleted. Pending core helpers still needed are decimal
classification, printable classification, checked PID tokens, finite-float
integer/division formatting, float parsing and POSIX shlex splitting; main has
no equivalent public APIs. The new fixed-record JSON edge needs two small
pending records helpers for Python ASCII escaping and float spellings because
records has no public Python-compatible JSON encoder. They use core for
finite-float spelling and keep integer fields exact.

Partial/skipped scope, with no completion-ledger claim:

| Unit | This batch | Remaining scope |
| --- | --- | --- |
| `process_info.py#unit-02` | Typed command seam; exact spawn/timeout census diagnostics and failure differentials | RP-1302: 1 MiB fallback capture refusal still differs from unbounded Python output |
| `scripts/bin/ps#unit-01` | Source/policy audit; existing parser/render/OS tests retained | Skipped implementation changes; its census errors inherit RP-1302, so it remains partial |
| `worker_liveness.py#unit-01` | Filesystem/command/census traits; canonical cwd, lsof, git identity/status, recursive session logs, explicit-clock metadata and file size | Main lacks Python surrogateescape string/path support; typed command refusal preserves non-UTF-8 argv. Extended JSON (NaN, lone surrogates), bounded input/command domains and non-strict symlink-loop resolution remain open parity work |
| `worker_liveness.py#unit-02` | OS collection adapter, typed main requests, terminal-report/error branches, exact ordered worker JSON and table output | Raw argparse/default/help edge belongs to CLI and is not implemented; underlying unit-01 observation limits remain |
| `runtime/fabric/src/ps.mjs#unit-01` | Node differential for platform-specific signal names | RP-SUP-PS-BUFFER: hard pipe cap and exact Node overflow capture still missing |

No command switches to a new owner, and no source is removed. Diagnostic
subprocesses have a 15-second deadline and 16 MiB stream bound; session first
lines have a 64 MiB bound. These source-domain differences are recorded as
open library mismatches, not approved behavior changes. Integer-range refusal
uses the already accepted v4 improvement and the records owner. Native reviews
check syscall effects/custody and liveness/source parity; cross-family review,
J1/J2/J3 and unrestricted Linux/macOS conformance remain chair-owned gates.

Filesystem review repairs additionally retain both rename paths in typed
errors and require `Send + Sync` for boxed safe filesystem capabilities.

Batch-3 final local evidence: 130 supervisor tests passed with
`PROVENANT_PORT_DIFF_REQUIRED=1`; all 102 root-package tests passed; supervisor
clippy with warnings denied and workspace fmt passed. Both layering policy
functions passed under isolated Python (pytest is absent locally). Two native
read-only reviews covered syscall/custody and source/record parity. Repairs
preserve a rotated first session-log path, root-before-nested glob ordering,
directory-link exclusion, refusal of truncated diagnostic paths, and the
source's missing/empty-home behavior. The missing-home/rotation regressions
were observed failing before their repairs and passing afterwards.

The syscall review found no introduced defects. It noted the existing
source-compatible utility policy can reap its leader before group cleanup,
leaving a theoretical process-group ID reuse race; this batch does not change
that implementation or S2 provider custody. Treat it as a cut-over design
question, not proof of provider custody. The chair still owns cross-family
review and exact-head CI/acceptance; no auto-merge is armed.

Batch 4 adds reusable raw-exec, strict-text, owned-PID, native-CWD, append/flock,
marker and following-read owners. See [PORT.md](PORT.md#owner-api) for the public
API inventory, current partial-unit accounting and remaining blockers. The old
batch descriptions above are historical; PORT.md owns batch-4 status.
