# Supervisor port, batch 4

Rulebook: `docs/specs/rust-port/PORTING.md` version 4.
Author route: `codex/gpt-6.1-sol@high` via Fabric, as supplied by the lane.
Source/oracle revision at intake: `ad64bd688ad6517ae883b43cc6bcde3c700d0796`.
The crate remains inert. No command is switched, no Python/TS source is removed,
and the chair owns cross-family review, exact-head CI and merge authority.
The root lane change only relocates its existing unsafe file operations.

## Owner API

Import these from `provenant_supervisor`; consumers adopt them in a later lane.
All first-party unsafe implementation stays below `sys`.

| Contract | Public surface and injection seam | Semantics |
| --- | --- | --- |
| Raw utility exec | `exec::{Executor, SystemExecutor, ExecOptions, run, ExecOutcome, CapturedStream, Termination, ExecError}`; `Executor::execute` | OS argv/environment/cwd, optional stdin bytes, separate stdout/stderr byte limits and live `Write` sinks, explicit stream discard, full environment replacement when supplied. Retains prefixes, drains overflow, records total bytes/truncation, no decoding/newline conversion. One nonblocking pipe pump, no detached threads or disk spool. Optional overflow stop. TERM at deadline, KILL after grace, bounded final reap; exit code and signal remain distinct. |
| Strict text | `exec::{run_text, TextOutcome, TextError}` over `&dyn Executor` | Strict UTF-8 on each retained stream; typed error names the invalid stream. No replacement or newline conversion. Python diagnostic adapters apply universal newline conversion separately. |
| Per-PID authority | `pid::{OwnedPid, PidAuthority, PidResult, PidError}`; `OwnedPid::{from_child, pid, try_wait}` | Consume an owned `std::process::Child`, never adopt a numeric PID. Mutex serializes wait/signal; an unreaped child reserves its PID, and cached terminal status fences later signals. TERM/KILL only, refuses PID <=1; ESRCH and EPERM remain typed results. Fakes implement `PidAuthority::{signal, probe}`. Drop does not signal a process. |
| CWD observation | `pid::{CwdObserver, SystemCwdObserver, CwdObservation}`; `CwdObserver::cwd` | Darwin `proc_pidinfo(PROC_PIDVNODEPATHINFO)` or Linux `/proc/PID/cwd`, preserving path bytes. Typed gone/permission-denied/unsupported results; no signalling grant. Worker diagnostics preserve Darwin's source `lsof` route and use this observer on Linux. |
| Append and locks | `fs::{AppendOptions, LockMode, LockOptions, AppendFile, PathIo}`, `FileHandle::{open_append, lock}` | Python `a+`: read/write O_APPEND + create, initial read position EOF, mode 0666 filtered by umask by default, existing mode unchanged. Shared/exclusive flock, blocking when timeout is None, bounded retry otherwise; typed `FsError::LockTimeout`. Normal file handles expose no descriptors. |
| Durable marker | `fs::publish_marker`, `PathIo::publish_marker` | Reuses `Directory::atomic_write`: exclusive 0600 temporary, fsync file, replacement rename, fsync directory. Final destination link is replaced, not followed. A post-rename fsync error leaves publication visible but durability unconfirmed. |
| Following-path read | `fs::{ReadOptions, read_following}`, `PathIo::read_following` | Explicitly follows links, refuses non-regular files before blocking reads, reads at most limit bytes, checks held-file length before/after, returns typed `FsError::TooLarge` (or `RaceRefused` for inconsistent length/read counts). This trusted-path diagnostic API is distinct from no-follow custody opens. |
| Initial path seam | `PathIo` implemented by `fs::LocalFs` | Fake append handles implement `AppendFile::{lock, unlock, sync}` and Read/Write/Seek; no OS handles are needed in consumer tests. |
| Existing lane syscall relocation | `sys::lane_files::{LaneDirectory, timestamp_epoch, canonical_start, touch}`; `LaneDirectory::{open, create, remove, exists, publish_file, sync}` | Retains the lane's no-follow anchor/parent traversal, nonblocking writer lease, exclusive capture creation, hard-link no-replace terminal publication, progress rename, timestamps and file touch. Fault injection, ENOSPC retry and post-link durability diagnostics remain in the lane. This is a legacy adapter, not a new custody policy. |

Raw exec is for utility children, not S2 providers. It signals only its direct
child; it never infers descendant or process-group authority. A descendant
holding a pipe cannot make its wait unbounded: the owner closes those pipes at
the deadline once its direct child has exited. The subprocess spawn syscall
itself and caller-supplied sinks cannot be preempted safely; sinks must return
promptly. Successful timeout cleanup reports `timed_out` even if the child
ultimately exits normally. A final reap timeout is a typed error; Drop performs
only a bounded best-effort kill/reap. Abort/SIGKILL cannot promise Drop cleanup.
`OwnedPid` is a utility capability, not an S2 WNOWAIT/provider lifecycle adapter;
callers retain explicit cleanup responsibility and must not abandon a live child.

`SafeFs`, `SafeDir` and `SafeFile` already require Send + Sync on main. Rename
errors already retain source and destination. Those review carries are verified,
not duplicated. `sys::account` now retries EINTR and retains a valid home when
pw_name is null (empty name), aligning the old root's home-only refusal rule.

## Unit accounting

No new graph unit is claimed complete in this batch; `ported.txt` is unchanged.
Each listed unit received its owner integration or coherent continuation:

| Unit | Batch-4 work | Remaining scope / blocker |
| --- | --- | --- |
| `skills/orchestrate/scripts/process_info.py#unit-02` | Bounded ps fallback now uses the raw owner; strict UTF-8 and Python newlines, typed spawn/census errors retained; core exact numeric helpers replace pending copies. | Existing RP-1302: Python has no 1 MiB fallback cap. Its complete refusal/exception domain is not parity. Resolving that source-domain boundary remains open; no silent acceptance. |
| `skills/orchestrate/scripts/bin/ps#unit-01` | Uses core's exact integer and Unicode printability owners; existing parsing, unreadable rows, layouts and output policy tests remain. | Inherits RP-1302 census boundary. Locale/SIGPIPE setup remains with the executable CLI owner; no cut-over. |
| `skills/orchestrate/scripts/worker_liveness.py#unit-01` | Raw strict diagnostic runner; native Linux CWD observation with lsof fallback and unchanged Darwin lsof routing; shared splitlines, float, shell and Unicode helpers replace private copies. | Main has no Python surrogateescape/lone-surrogate path/string owner or extended JSON NaN/Infinity decoder. Existing diagnostic-command/first-line bounds and symlink-loop parity remain open. Non-UTF-8 native paths are explicitly refused instead of replaced. |
| `runtime/fabric/src/ps.mjs#unit-01` | Pipe-based hard retained cap replaces sampled disk-spool capture; ignored stderr stays discarded; existing spawn-only fallback, PATH, Node replacement decoding and timeout/signal evidence retained. | RP-SUP-PS-BUFFER remains: Node's synchronous maxBuffer preserves an overflow read; the raw owner retains an exact bounded prefix. TERM-ignoring Node timeout waits also differ from a bounded owner. Full Node synchronous overflow semantics have no owner API on main. |
| `skills/orchestrate/scripts/worker_liveness.py#unit-02` | Inert typed argv parser, explicit home/cwd defaults, repeat-last options, unique long-option abbreviations, terminal choices and exact signed/unsigned i64/u64 JSON values; main request/effect pipeline retained. | Underlying unit-01 blockers remain. CLI integration must supply observed defaults, structural help/error rendering and process stdio/exit effects; argparse diagnostic-byte parity is not claimed. |

`worker_liveness::edge::{MainDefaults, ArgumentError, CommandInteger,
parse_request}` are the new inert command-edge items.
`worker_liveness::ReportInteger` seals terminal record fields to exact supported
integer types; a compile-fail fixture rejects string-valued terminal records. Existing terminal-report
and exit records are generic over the sealed `ReportInteger` domain (i64, u64,
or `CommandInteger`); serde wire fields
are unchanged. `ObservationFs::pid_cwd` defaults to unsupported for injectable
filesystems; `ObserveError::NonUtf8Path` is an explicit refusal.

Only `pending::{json_ascii, json_float}` remain, both marked
`port: belongs in provenant-records`. They implement fixed-record Python ASCII
escaping/float spelling; main's records crate has no equivalent encoder. All
core pending copies have been deleted. Checked session JSON still uses
`provenant_records::checked_json`, and float timestamp conversion still uses
`provenant_core::py::from_timestamp`.

## Verification

Targeted checks: workspace fmt, supervisor clippy with warnings denied,
supervisor tests with `PROVENANT_PORT_DIFF_REQUIRED=1`, and the touched root lane
file tests. Python/Node differential helpers retain `-I` for Python, an explicit
environment and version gates; absence prints `skip:` and required mode fails.
Fixtures compare raw stdin/output bytes, signal/nonzero results, worker terminal
CLI JSON (including Unicode and unsigned-64 integers), and existing policy cases.
Clocks remain explicit in the liveness reducers; no sleep is used as an assertion.
A removed capture append was observed failing its byte-policy test before being
restored. Review regressions cover append read positioning, cached-exit signal
fencing, ignored stderr, unsigned integers and abbreviated flags.

Native adversarial reviews use separate syscall/custody and source-parity
lenses. Their findings and final exact-head counts are retained in the PR;
cross-family review, Linux/macOS CI matrix and J1/J2/J3 cut-over judges remain
with the chair. Auto-merge is not armed.

Final local evidence after merging `origin/main` at
`114e3331ebf6b767baadac1069d4b900ae7d91d2`: 150 supervisor unit tests and
1 compile-fail doctest passed with required differentials; 2 touched root lane
file tests and both layering policy checks passed. Workspace fmt and supervisor
clippy with warnings denied passed. The five Python/TS oracle source files are
unchanged from the intake revision. Both native reviewers confirmed no unresolved
introduced defect after repairs; they do not replace the chair's cross-family
review or PR CI gate. The containment fixture was observed red before repair.
