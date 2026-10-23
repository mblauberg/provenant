# S1–S2: native OS and provider custody

Rulebook: `PORTING.md` version 7. Design: `docs/specs/rust-port/design/supervisor.md`.
Author route: `codex/gpt-6.1-sol@high` via Fabric. S2 is stacked on #1370.

S2 removes the old Rust lane, its compatibility JSON parser, the public raw
`lane run` launch specification and the Python lane conformance subjects.
Python leaves with live Run/CLI/Fabric importers remain until R4+R5 and the
design's last-importer gates. There is no adapter for the removed command.

## Native boundaries

| Owner under `sys` | Operations and proof |
| --- | --- |
| `child`, `raw_exec` | Preallocated argv/environment, strict exec, signal reset, CLOEXEC sweep, owned utility child and bounded pipe polling. |
| `reaper` | Fixed admission reserved before spawn, non-signalling transfer, bounded fair `try_wait` turns. |
| `pid`, `census` | Linux procfs and Darwin libproc identity/CPU/CWD, native boot identity, complete/partial/unknown census. Observation never grants arbitrary PID signalling. |
| `provider` | Guardian-local signal reactor and Linux subreaping, WNOWAIT leader reservation, controlled group delivery, exact adopted-child checks and explicit cleanup before reap. |
| `handoff` | Bounded private same-binary channel; only the guardian inherits writer/attempt locks. Provider environment values and prompt remain ephemeral. |
| `fs` | Held-directory open/stat/mutation, atomic publication, no-follow locks, descriptor ownership, directory sync and bounded flock. |

`machine` is pure; `capture` decodes before bounded retention and never treats
capture failure or silence as a reason to stop. `custody` owns generation-scoped
records, reservation fencing, recovery and release. `internal supervise` is
private descriptor handoff, not a raw launch API. Run composes provider decoding
and its children journal through `GuardianHooks` in its same-binary entry point.

## Clarifications for the next owners

The state reservation directory and verified worktree are different anchors.
`WriterBinding` can only be constructed from confinement-supplied held worktree
and Git handles, a branch and pinned base. Confinement owns their association to
the selected state slot. A private storage DTO reads those facts during recovery.
A durable ownership journal precedes reservation publication; missing reservation
with unresolved journal evidence refuses a new writer. Retries update the
reservation under the same writer lease; the original ownership journal remains
until settlement, avoiding a two-record generation transaction.

Run's trusted permissions service implements `ReconcileAuthority`. This is an
in-process composition seam, not authentication against arbitrary Rust code.
Supervisor checks the scoped decision and durably records actor, evidence and
disposition before accepting a release. Uncertainty alone does not release a
writer, and reconcile never grants numeric-PID signalling.

`SettlementCommit` requires the original attempt inode, its custody lock and
Run-private `release.json`, `terminal-commit.json` and `quarantine/manifest.json`
attestations. All are version 1 and scoped; the release contains both raw-file
digests. Run must write these only after its canonical terminal transaction and
quarantine commit. This explicit release-attestation contract fills a design gap;
S2 neither reads SQLite nor manufactures canonical Run truth. Run must protect
these files from providers. Scratch admission likewise requires a private generated
directory outside the provider sandbox and Run/writer/attempt exclusion against
all legitimate mutators. Mode 0700 alone does not exclude same-UID peers; unknown
or externally writable trees must not be admitted. Only classified unchanged
leaves are removed; captures, history and unknown data remain.

The guardian requires a native boot identity and fails closed if the host denies
it. Dev fixtures inject a recorded boot key explicitly while retaining real
native process observations; production has no environment override. Darwin's
public proc APIs do not prove a particular open file description holds `flock`;
nested custody therefore remains unknown and blocks unsafe group delivery there.
A future native proof needs an explicit reviewed mechanism, not a guessed flag.

The binary pointer reader now uses `serde_json` directly, allowing the last shared
Python/V8 compatibility parser to be deleted. No legacy wire emulation remains
in supervisor. S1's `WaitBudget`/`Clock`, streams, executor, filesystem and reaper
APIs are reused. No dependency on Run, providers or confinement is introduced.
