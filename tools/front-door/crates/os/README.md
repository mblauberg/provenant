# Operating-system primitives

provenant-os owns safe OS operations below supervisor. Unsafe platform
operations live in sys; custody-specific native operations remain in
supervisor. The crate depends only on core among workspace members.

`exec::SystemExecutor` is constructed once at the process edge and shared with
consumers through `Executor`. Its clones share fixed-capacity cleanup admission,
reserved before spawn. Every child is reaped explicitly or transferred to that
reaper. Error paths send SIGKILL before transfer; dropping an owner never waits.
All executor instances share one reaper worker, which parks when idle. Utilities
have direct-child authority only. Providers use supervisor's dedicated
same-binary guardian, persistent writer reservation and explicit census/cleanup.

Each stream selects `Capture`, `Discard` or `Tee`. Capture defaults to 8 MiB and
refuses limits over 64 MiB. Discard creates no pipe. Tee uses a bounded
nonblocking queue and records full/failed sink loss. Optional bounded frame
consumers run before retention and filtering; oversized frames and decoder
queue loss remain observations. Capture/filter failures never request a stop.
Only a delivered deadline TERM sets `timed_out`; drain expiry and overflow have
separate outcomes. `run_text` strictly decodes retained UTF-8 bytes.

`fs::Directory` retains a directory descriptor and validates every relative
component. Directory and FileHandle provide the single concrete filesystem
API with typed FsError results. Directory::read_stable bounds reads and refuses
observed metadata or identity changes. Publication::require_durable converts
an unconfirmed publication into its original sync error.
Authoritative parents must be private and trusted. Owner locks additionally
validate directory ownership, leaf type, owner, permissions, link count and
identity. `Directory::lock` uses nonblocking flock polling with core's
`WaitBudget`, `CancelToken` and the `Clock` that defines its monotonic coordinate.
Stable names end in `.lock`; generic mutation refuses these leaves.
An expired budget permits one nonblocking attempt. The non-cloneable `LockGuard`
has no explicit unlock; its deadline never revokes it. Hidden descriptor handoff
operations support supervisor custody across the crate boundary.

`atomic_write` and `publish` sync exclusive 0600 staging files before atomic
`Replace` or `NoReplace` publication, then sync the parent. No-replace conflict
preserves existing bytes. A successful rename followed by failed sync returns
`Publication::VisibleDurabilityUnconfirmed`; callers retain their custody and
retry durability rather than overwrite the visible record.

Confinement owns the closed Git/gh utility policy, repository verification and
sanitised environment. It executes through the OS crate's bounded `Executor`.

The fixture feature exposes filesystem fault operations and temporary-root
helpers for focused tests. Execution and filesystem tests live beside their
implementation.
