# Safe OS owner

`provenant-supervisor` owns the operating system boundary under PORTING.md v7,
steps S1 and S2 of `design/supervisor.md`. All first-party unsafe code lives in `sys`.

`exec::SystemExecutor` is constructed once at the process edge and shared with
consumers through `Executor`. Its clones share fixed-capacity cleanup admission,
reserved before spawn. Every child is reaped explicitly or transferred to that
reaper. Error paths send SIGKILL before transfer; dropping an owner never waits.
All executor instances share one reaper worker, which parks when idle. Utilities
have direct-child authority only. Providers use the dedicated same-binary guardian, persistent writer reservation and explicit census/cleanup described in `PORT.md`.

Each stream selects `Capture`, `Discard` or `Tee`. Capture defaults to 8 MiB and
refuses limits over 64 MiB. Discard creates no pipe. Tee uses a bounded
nonblocking queue and records full/failed sink loss. Optional bounded frame
consumers run before retention and filtering; oversized frames and decoder
queue loss remain observations. Capture/filter failures never request a stop.
Only a delivered deadline TERM sets `timed_out`; drain expiry and overflow have
separate outcomes. `run_text` strictly decodes retained UTF-8 bytes.

`fs::Directory` retains a directory descriptor and validates every relative
component. `SafeFs`, `SafeDir`, `SafeFile` and `PathIo` are the filesystem seams;
their filesystem methods preserve typed `FsError` results through trait objects.
Authoritative parents must be private and trusted. Owner locks additionally
validate directory ownership, leaf type, owner, permissions, link count and
identity. `Directory::lock` uses nonblocking flock polling with core's
`WaitBudget`, `CancelToken` and the `Clock` that defines its monotonic coordinate.
Stable names end in `.lock`; generic mutation refuses these leaves.
An expired budget permits one nonblocking attempt. The non-cloneable `LockGuard`
has no explicit unlock or raw descriptor escape; its deadline never revokes it.

`atomic_write` and `publish` sync exclusive 0600 staging files before atomic
`Replace` or `NoReplace` publication, then sync the parent. No-replace conflict
preserves existing bytes. A successful rename followed by failed sync returns
`Publication::VisibleDurabilityUnconfirmed`; callers retain their custody and
retry durability rather than overwrite the visible record.

Confinement owns the closed Git/gh utility policy, repository verification and
sanitised environment. It executes through this crate's bounded `Executor`.

The dev-only `fixture` feature supplies self-re-exec scenarios for the
`harness = false` process target. A self dev-dependency enables it for crate
tests; installation builds do not enable it. No additional executable is built.
Process children use temporary HOME, state and harness configuration paths.
