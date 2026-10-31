# Supervisor and confinement

Owner: `crates/supervisor` (custody), `crates/confinement` (permissions,
evidence, worktrees), `crates/os` (bounded I/O and execution)
Anchor: `crates/supervisor/tests/custody.rs`, `crates/supervisor/src/custody_tests.rs`,
`crates/confinement/src/permissions.rs`, `src/worktree_tests.rs`

Supervisor holds processes, output and live custody through provider exit and
durable exit publication. Run then drives resumable settlement while retaining
the worktree writer flock through import and publication. Confinement turns an authorised plan into
filesystem, Git, network and provider permissions, checks evidence and
quarantines instruction changes. Together they prevent a failed write, a lost client or a mistaken process identity from killing a healthy lane, freeing a busy worktree or publishing an incomplete result. Run owns admission and result
semantics ([run](run.md)). Wiring and platform limits are in the [crate
notes](../../tools/front-door/crates/supervisor/README.md). Paths are
relative to `tools/front-door/`.

## Custody

### Closed stop reasons; natural exit wins

Only `ProviderGroup::stop(StopReason)` signals a provider: explicit cancellation, wall deadline, protocol rejection, supervisor signal or post-terminal
cleanup. Silence, CPU inactivity, capture failure, ENOSPC and failed
publication cannot produce a stop reason, and there is no startup deadline. A
natural exit observed in a tick wins over a late cancel or deadline.
- Why: a bookkeeping fault or slow start must never kill a healthy provider.
- Test: `crates/supervisor/src/machine.rs::silence_capture_and_observation_failures_are_not_stop_reasons`;
  `::a_wnowait_exit_wins_over_late_cancel_and_deadline`
- Owner: `crates/supervisor/src/machine.rs`

### The runtime deadline is wall-clock time

An attempt's allowance is its dispatch `--provider-timeout`, tightened by the
provider's `providers.<id>.runtime_deadline_seconds` cap when one is set.
The guardian stops the provider (SIGTERM, then SIGKILL after the cleanup
grace) when either the monotonic clock or the wall clock has run the whole
allowance since launch. The monotonic clock stands still while a host sleeps, so
it alone would extend a deadline by the length of every sleep. Run records the
absolute `runtime_deadline` when the attempt starts, `lanes` shows it for
running lanes, and `reconcile` asks a live guardian that is still running
two minutes past it to stop at the deadline. The attempt settles as
`timed_out` with the reason `runtime deadline exceeded: provider stopped after
Ns`, custody is released, and `provenant resume` continues its session.
- Why: stuck attempts hold worktrees, locks and quota for hours.
- Test: `crates/supervisor/src/machine.rs::a_host_sleep_cannot_extend_the_wall_deadline`;
  `crates/supervisor/tests/custody.rs::a_host_sleep_cannot_extend_the_wall_deadline`;
  `crates/run/src/dispatch_tests.rs::a_stop_at_the_runtime_deadline_settles_as_a_typed_timeout`
- Owner: `crates/supervisor/src/machine.rs`, `crates/supervisor/src/guardian.rs`,
  `crates/run/src/holder.rs`, `crates/run/src/controls.rs`

### Signals need proven identity and scope

Supervisor registers child observation before spawn, observes exit with
`WNOWAIT` and reaps the handle last, so the PID stays reserved through
cleanup. Only the unreaped provider, its group while the leader is unreaped,
and proven attempt-sandbox escapees may be signalled. Reused identities,
members that left the group, shared hosts and sibling groups are never signalled.
Census and working-directory (CWD) observations never grant signal authority.
- Why: signalling a recycled PID or a sibling's group kills unrelated work.
- Test: `crates/supervisor/tests/process.rs::inherited_wait_policy_preserves_pid_reservation`;
  `crates/supervisor/tests/custody.rs::stale_complete_census_does_not_signal_a_late_shared_group_owner`
- Owner: `crates/supervisor/src/process_os/`, `crates/supervisor/src/custody.rs`

### Nested owners are spared unless disproven

A registered nested owner is spared from signals only with matching native identity,
observed ancestry and proof of its held custody; an unlocked descriptor,
environment token or process name is not proof. When macOS cannot prove an
owner's custody, that owner and its descendants stay protected.
- Why: a nested lane's owner must survive its parent's cleanup.
- Test: `crates/supervisor/tests/custody.rs::nested_owner_stays_protected_when_native_custody_proof_is_uncertain`;
  `crates/supervisor/src/census.rs::ancestry_requires_exact_identities_and_strictly_older_parent_stamps`
- Owner: `crates/supervisor/src/census.rs`

### Escapees

Each confined launch may look up one Mach name unique to its attempt. After
exit, a live process outside the provider that the sandbox reports as
permitted to look up that name is an escapee: supervisor re-identifies it,
then sends SIGTERM and SIGKILL. If the nested child store is unreadable or
holds a nested run, supervisor signals nothing and the escapee stays
unresolved.
- Why: a detached descendant keeps the sandbox but leaves the group.
- Test: `crates/supervisor/tests/custody.rs::detached_descendant_in_the_attempt_sandbox_is_found_and_ended`;
  `::escapee_is_ended_only_when_the_host_reports_no_nested_runs`
- Owner: `crates/supervisor/src/guardian/`

### Guardian holds custody past its owner

The attempt guardian runs in its own session and receives the plan and a
duplicated writer lease over a bounded private channel and acknowledges them
before the parent disconnects. Secrets travel in memory and pipes, never argv or
disk. The guardian keeps the provider, attempt lock and lease after the run
owner or MCP host dies. Provider exec needs both leases and the durable
reservation and closes every custody descriptor. A missing acknowledgement
is an unknown launch, never permission to launch again.
- Why: losing the caller must not lose custody or double-launch a writer.
- Test: `crates/supervisor/tests/custody.rs::owner_death_keeps_guardian_and_writer_live`;
  `crates/os/src/exec/tests.rs::secret_input_arrives_by_descriptor_with_eof_and_no_token_environment`.
  The unknown-launch rule has no test (gap).
- Owner: `crates/supervisor/src/guardian/`, `crates/run/src/launch.rs`

### Quiescence before release

A writer stays fenced until three things hold: the provider is reaped; two
complete census snapshots at least 20 ms apart show no other session or group
member and no tracked survivor; and a fresh non-blocking lock on the original
lease inode succeeds. Failed, racing or incomplete reads keep the fence.
- Why: a leftover descendant could still write the worktree.
- Test: `crates/supervisor/src/guardian/tests.rs::darwin_settlement_requires_complete_absence_of_members_and_recorded_escapes`;
  `crates/supervisor/src/custody_tests.rs::an_unrecorded_escape_cannot_be_cleared_by_an_empty_census`
- Owner: `crates/supervisor/src/guardian/`

### Unresolved exits and orphans

An unresolved exit keeps the guardian cleaning; once quiescence is proven it
replaces `process-exit.json` once, the only replacement of that file. With the
guardian gone, recovery takes the writer lock without waiting and signals
only a PID whose boot ID, PID and start time match the record, or that macOS
places in the attempt sandbox; anything else is named, never signalled.
Uncertainty never clears a fence; only a recorded decision with matching
authority may do so, and that decision permits no signal.
- Why: recovery must not kill a reused PID or free a busy worktree.
- Test: `crates/supervisor/src/custody/orphans.rs::an_unprovable_pid_is_never_signalled_and_is_named`;
  `crates/supervisor/src/custody_tests.rs::reconcile_requires_matching_authority_and_records_before_release`
- Owner: `crates/supervisor/src/custody/orphans.rs`

### Escalation to the user

Signalling a healthy process, weakened confinement and lost publication
escalate to the user; no environment guess or uncertain signal compensates.
- Why: these outcomes cannot be repaired safely by the supervisor alone.
- Test: none (review gate); the blocking tests in this section stop the
  signal but do not notify the user.
- Owner: `crates/supervisor/src/custody/orphans.rs`

### Durable exit publication

The exit record is published without replacing an existing record after the files it references are
durable. A failed publication keeps custody and retries; a visible link with
a failed directory sync is `durability_unconfirmed`, never absent. On ENOSPC,
supervisor releases a 64 KiB reserve for one retry, with no provider signal.
- Why: a lost or overwritten exit record loses the only proof of outcome.
- Test: `crates/supervisor/tests/publication.rs::exit_publication_enospc_and_visible_sync_retry_keeps_one_snapshot`;
  `crates/supervisor/tests/custody.rs::publication_failure_retains_the_lease_and_retries_without_overwrite`
- Owner: `crates/supervisor/src/custody.rs`

### Writer reservation and settlement

The writer reservation is published in `<worktree-slot>/<attempt>/` under
the shared writer lock before spawn and cleared only after quiescence, quarantine and run's terminal commit, or an
authorised reconcile decision. Provider exit releases its host build slot;
once quiescence is proven, the attempt custody lock is released, while the
worktree writer flock remains held through object import and publication. The
reservation remains as recovery evidence. Run journals import,
publication, terminal recording and release with one native process owner and
a counter capped at three attempts that reach import. Import claims use
persisted backoff; an unknown process observation expires after ten minutes.
Short locks protect only journal updates; Git runs
without them. A dead owner resumes at the last durable phase, including after
branch publication. Public cancel records its request without waiting for a
writer lock; reconcile queues a detached driver rather than doing import in
the caller. Lane waits continue reporting settlement until custody is released.
See [publication order](run.md#publication-order). A missing or
malformed reservation refuses
recovery and readmission of that attempt. Admission of a different attempt does
not inspect predecessor records or retired files in the shared lock slot.
`SettlementCommit`, needed for scratch deletion and fence clearing, is issued only after the attempt inode, custody lock and both
digests in `release.json` are verified. After a verified boot change, a
reservation whose recorded device number differs from the current one (APFS
can renumber volumes across a reboot) is still recoverable when the
reservation and attempt shared a device, their current directories still have
the recorded inodes, and both now share a device. Within one boot, a changed
device number refuses recovery.
- Why: a writer must never be freed on a partial or forged settlement.
- Test: `crates/supervisor/src/custody_tests.rs::missing_or_malformed_reservation_does_not_reopen_a_writer`;
  `::settlement_checks_exact_attempt_and_both_digests_before_scratch`;
  `::worktree_settlement_retains_flock_until_recovery_is_dropped`;
  `::admission_ignores_predecessor_evidence_but_shares_writer_exclusion`;
  `::reservation_listing_isolates_an_inaccessible_slot`;
  `crates/run/src/dispatch_tests.rs::reconcile_recovers_writer_when_device_numbers_change_after_reboot`;
  `::changed_device_numbers_on_same_boot_keep_writer_custody`
- Owner: `crates/supervisor/src/custody.rs`

### Cancel requests from lanes

A stop request from a held scope replaces an undecodable, foreign or
earlier-generation `cancel.json`, or a symlink, FIFO or oversized entry, without
following it; a reattached handle never replaces a later generation's
request. Because the lane can write its attempt directory, it can pre-empt
or undo the host's stop request; this residual risk is accepted.
- Why: stale or forged requests must not cancel a later attempt.
- Test: `crates/supervisor/tests/custody.rs::held_stop_replaces_what_a_lane_left_but_reattach_keeps_a_later_generation`
- Owner: `crates/supervisor/src/custody.rs`

### Stable locks and atomic writes

Lock leaves are regular, single-link, owner-controlled files whose inodes are
never unlinked while a duplicate descriptor is open. Acquisition polls `flock(LOCK_NB)`
to one monotonic deadline and never revokes or steals a lease. Locks are taken in this order:
run custody, repository metadata, writer reservation, attempt publication,
children journal. Authoritative writes use held directory handles, 0600
temporaries in 0700 directories, file sync, atomic rename or no-replace and
a parent sync.
- Why: a recreated lock inode lets two owners hold "the same" lock, and a
  crash must leave the old or new file whole.
- Test: `crates/os/src/fs/tests.rs::stable_lock_namespace_refuses_every_mutation_without_splitting_exclusion`;
  `::process_crash_at_publication_boundaries_leaves_old_or_new_complete_bytes`.
  Lock ordering has no test (gap).
- Owner: `crates/os/src/fs.rs`

### Bounded execution

Captures are bounded (8 MiB retained per stream by default, 64 MiB maximum,
64 KiB frames). Frames are decoded before truncation; an oversized frame is
decoder loss, never a fabricated terminal event, and tee loss never stalls
capture. A natural exit followed by a drain deadline is `drain_truncated`,
not `timed_out`. Utility execution reserves its reaper slot before spawn.
- Why: unbounded output or a hung pipe must not stall custody or invent an
  outcome.
- Test: `crates/os/src/capture.rs::oversized_frames_are_counted_and_never_decoded_as_terminal`;
  `crates/os/src/exec/tests.rs::reaper_reserves_before_spawn_and_releases_unused_admission`
- Owner: `crates/os/src/capture.rs`, `crates/os/src/exec/`

### Real profiles need native recordings

Fake providers cannot certify a real sandbox profile, and a missing native
recording of the platform, backend and allowed or denied operations blocks
that profile's support.
- Why: snapshot equality does not prove confinement behaviour.
- Test: none (review gate); acceptance evidence is the native recordings under
  `crates/confinement/tests/fixtures/external`.
- Owner: `crates/confinement/src/permissions.rs`

## Permission policy

Rendered profiles are pinned by
`crates/providers/src/seatbelt_snapshots.rs::seatbelt_profiles_match_snapshots`.
The rules below are what every profile must keep.

### Database IPC shares the host namespace

Database IPC is denied by default in both writer and read-only profiles.
Only an explicit `provenant run --capability postgres` declaration grants
`ipc-sysv-shm` and `ipc-sysv-sem`. The CLI preserves this in the sealed
`LaneAccess.database_ipc` field; both provider and configured-command launchers
copy it to `ProviderProfileSpec.allow_database_ipc` before compilation. Network
access, writer mode, provider identity and loopback-server support never imply
database authority. Ordinary MCP, preset and undeclared reviewer lanes stay
off. Host child validation refuses database authority beyond the parent, and
unvalidated sealed requests cannot add it. The separate backend availability
probe keeps its minimal launch-only profile.

PostgreSQL 18 always uses a small SysV shared-memory interlock, including with
`shared_memory_type=mmap`. Its [Darwin configuration](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/template/darwin)
selects SysV semaphores on supported macOS versions, so POSIX semaphores remain
denied. Configure dynamic shared memory to use files in the confined database
directory, including during initialisation:

```sh
initdb -D "$TMPDIR/pgdata" -c dynamic_shared_memory_type=mmap
postgres -D "$TMPDIR/pgdata" -c dynamic_shared_memory_type=mmap
```

This avoids PostgreSQL's host-global POSIX shared-memory namespace entirely.
No PostgreSQL POSIX shared-memory, POSIX semaphore or SysV message-queue grant
is added, even for a database-enabled lane.

This is a security-relevant confinement trade-off: macOS SysV IPC is
host-global and Seatbelt cannot restrict these operations to a lane's own
segments or semaphore sets. A lane may discover keys and access another
process's objects when ordinary Unix IPC permissions allow it, including
objects owned by the same user, including mutation and `IPC_RMID` removal.
The explicit database capability accepts that exposure; it does not create a
lane-private SysV namespace. Other users' objects remain protected under normal
restrictive permissions. Profiles without the capability cannot mutate or
remove the host's objects, including those owned by the same user.
However, macOS does not sandbox-check the SysV creation path. Even a lane
without database authority can allocate a segment or semaphore set and then
be refused permission to remove its own allocation. Repeated failed database
starts can exhaust host-global IPC limits such as `kern.sysv.shmmni`.
Capability-off therefore does not guarantee freedom from IPC resource exhaustion.

Every SysV-touching native test records host `ipcs -m -s` inventories before
and after. Trusted test code journals exact newly created IDs; a host-side
guard removes only those IDs and asserts that none remain. PostgreSQL tests
audit allocations across initdb's subprocesses, including failed starts.
Cleanup never deletes an unowned inventory difference or a pre-existing object.
A general lane-end sweep is tracked in #2454 and needs a separate ownership design: semaphore metadata
does not expose a creator PID, and exited-child ancestry cannot be reconstructed
reliably. It must not infer ownership from a UID or a before/after global diff.
The test journal is not crash-atomic: a forced kill between allocation and
journal append can leave an unrecorded object. The production ownership work
in #2454 must also address that interruption window.
File confinement still restricts database files to existing writable roots,
and executable, Mach and network rules retain their existing limits. Database
clients need the existing local or open network policy for loopback access.
Use a separate OS user or a VM when isolation from same-user IPC is required.

Both lane modes allow `(allow signal (target same-sandbox))`. PostgreSQL
backends need to notify their postmaster parent and sibling processes; the
former writer-only `children` target denied child-to-parent signals and left
client queries and clean shutdown hanging. The same-sandbox rule replaces that
target and permits communication only among processes in the same Seatbelt
sandbox. Signals to the outside supervisor, host processes and independently
launched sandboxes with byte-identical profiles
remain denied. It grants no additional execution, file, Mach, network or IPC
authority.

- Why: PostgreSQL startup and database checks need kernel IPC without broader
  file, executable or network authority, and intra-sandbox signals for queries
  and clean shutdown.
- Test: `crates/confinement/src/permissions.rs::writer_test_grants_stay_within_the_attempt_and_mode`;
  `::native_sandbox_blocks_path_escape_probes`;
  `crates/confinement/src/permissions/tests/ipc.rs::postgres_database_ipc_allows_mmap_lifecycle_in_both_modes`;
  `::postgres_without_database_ipc_refuses_initdb`;
  `::database_off_cannot_mutate_or_remove_foreign_sysv_objects`;
  `crates/confinement/src/permissions/tests/signals.rs::same_sandbox_signals_reach_parents_but_cannot_reach_host_processes`
- Owner: `crates/confinement/src/permissions.rs`

### Lane process-tree memory interface

Inside a macOS lane, run `provenant process-tree --root PID --json` with the PID
of the command being supervised. The normal command envelope has this `data`:

```json
{"schema_version":1,"root_pid":410,"sampled_at":"2026-11-01T00:00:00.000000Z","processes":[{"pid":410,"ppid":400,"pgid":410,"rss_bytes":1048576},{"pid":411,"ppid":410,"pgid":410,"rss_bytes":524288}]}
```

The root and its descendants are included, sorted by PID. RSS is in bytes;
`sampled_at` is the observation's start in RFC 3339 UTC with six fractional
digits. Sum `rss_bytes` to estimate the command tree's resident memory; shared
pages can be counted by more than one process. Zombies have no resident task;
they are omitted when the kernel no longer exposes their sandbox, or included
with zero RSS. Selecting an exited or zombie root is unavailable.
This is periodic observation, not an atomic kernel census or a kernel
memory limit: short-lived processes and changes between samples can be missed.
The tree follows current parent links. Reparented orphans are no longer in
that rooted tree and are not counted; use a separate containment policy when
commands may deliberately daemonise.

The unconfined supervisor walks descendants of its held provider identity with
libproc, checks each process against the existing attempt sandbox marker, and
rechecks parent links, start times and child lists before publishing. Host
processes and independent peer lanes are excluded even when a peer has the
same profile text or process group. No global process list is published.
`proc_listchildpids` itself returns `EPERM` inside the production lane, so this
walk runs on the host. The `kern.proc.` and `kern.procargs` denies and the
same-sandbox process-info scope remain unchanged.

One worker per live provider refreshes `<attempt>/process-tree.json` about
once a second without blocking the guardian's control loop. Publication uses
the held attempt directory and atomic replacement. The lane receives one
exact read grant for this file and its absolute path in `PROVENANT_PROCESS_TREE`;
the attempt directory and snapshot remain outside lane write authority. The
guard uses the host-provided path. Caller configuration and environment are
trusted; overriding this path to supply other data is unsupported. The file
envelope is private; use the CLI for the stable interface. The CLI opens no
host state store and needs no configured project.

The reader selects only a root present in that lane snapshot, rechecks its
native identity, and rejects an exited root, another boot, a future observation
or a sample older than five seconds on the host's sleep-aware monotonic clock.
Missing, malformed, oversized, incomplete, stale or unsupported samples return
exit **4**, status `unavailable`, error `PROCESS_TREE_UNAVAILABLE`, and null
`data`. A guard must treat this as lost observation, never zero memory; retry
or apply its own refusal policy. Invalid CLI arguments return exit 2.
Traversal is limited to 4,096 processes and a 250 ms observation budget;
detected churn or exceeded bounds invalidates the sample rather than returning
a partial tree. The deadline is checked throughout revalidation as well as
traversal. Only process churn is retried, up to three attempts per refresh;
size/time overruns and unavailable native observations stop the refresh.
The worker retains the last good sample until it expires; it never reports
an incomplete observation as a new sample. Failed publication also leaves
bytes that the freshness check expires. Provider exit
stops the worker and requests invalidation; a dead supervisor's file expires.

Supported nested lanes also get their own attempt snapshot and tree, sampled
by an unconfined host guardian. Submissions inside a lane are queued for the
parent guardian's host-side child-launch service; the lane does not launch its
child supervisor locally (`crates/cli/src/run.rs::submit_with_host_and_wait`,
`::launch_prepared_with_wait`, `crates/cli/src/run/children.rs::serve`). A supervisor
started directly inside a parent sandbox cannot use `proc_listchildpids`:
`EPERM` leaves sampling unavailable and the reader returns exit 4. That is not
the supported nested-lane launch path.

- Test: `crates/confinement/src/permissions/tests/process_tree.rs::lane_process_tree_sample_is_private_under_production_profiles`;
  `crates/supervisor/src/process_tree.rs::tests`;
  `tests/command_adapter.rs::command_lane_reads_its_supervisors_process_tree_through_the_cli`;
  `::process_tree_cli_is_unavailable_without_a_supervisor_sample`
- Owner: `crates/supervisor/src/process_tree.rs`, `crates/cli/src/run/provider.rs`,
  `src/process_tree.rs`

### Process information stays inside the sandbox

Every lane profile explicitly denies the process-info family before allowing
`(allow process-info* (target same-sandbox))`, so a lane can read process
information, including resident memory, for a PID it already knows inside its
own sandbox. The `kern.proc.` sysctl deny stops a lane from enumerating
processes, its own descendants included, so an in-lane memory guard cannot
discover its process tree. The supervisor samples that tree on the host and
serves an attempt-local snapshot through `provenant process-tree` above.
An allow-only clause is
not assumed to restrict APIs that the platform permits by default. The native
test records the unscoped baseline and compares an explicit deny,
`process-info-pidinfo` scoped to the sandbox, and the final scoped wildcard.
The lane's own processes must have nonzero RSS. An outside same-user host PID
must remain denied or invisible to libproc.

Every profile also denies process-list and argument sysctls with
`(deny sysctl-read (sysctl-name-prefix "kern.procargs") (sysctl-name-prefix "kern.proc."))`.
Native controls require self PID/argument and CPU queries to keep working,
while outside-host PID/argument and process-enumeration queries fail with
`EPERM`. The tool compatibility probe also runs under this restriction.

The native A/B probe confirms that unscoped libproc RSS queries succeed, the
explicit deny returns `EPERM`, and the scoped PIDINFO and wildcard rules permit
own-process queries while refusing the outside host. The identical-profile
peer fixture also requires `proc_pidinfo` of a separately launched live sandbox
to fail with `EPERM`, while its own RSS query succeeds.

Stock `ps` remains a separate unresolved restriction: `execve("/bin/ps", ... )`
returns `EPERM` before any process-info lookup, in both access modes. On Darwin
27, the binary is setuid and libsandbox's diagnostic `sandbox_check` reports
`forbidden-exec-sugid` denied; an unknown-operation control returns `EINVAL`.
This is a queried policy denial, rather than a captured kernel log for the
exec. `forbidden-exec-sugid` is not a grantable SBPL symbol on that host.
The process-info rule therefore does not fix consumers that invoke stock `ps`.
`provenant process-tree` supplies the supported RSS interface for consumers'
memory guards. No privileged execution, sandbox escape or Mach authority is added.

The native production-profile shell probe also runs an offline Cargo test with
two jobs. Its build script acquires and returns an inherited jobserver token,
and its compiled Rust test checks the success marker. The same probe commits
with Git and runs Node child-process spawn/fork IPC directly and through a pnpm
script. macOS CI requires Node and pnpm availability. PostgreSQL's separate
database-enabled lifecycle smoke covers both access modes.

- Test: `crates/confinement/src/permissions/tests/process_info.rs::process_info_is_same_sandbox_and_reports_own_rss`;
  `crates/confinement/src/permissions/tests/sysctl.rs::sysctl_blocks_host_process_queries_but_preserves_self`;
  `src/worktree_tests.rs::lane_prepare_isolates_nested_state` (`verify_lane_profile` shell probe);
  `crates/confinement/src/permissions/tests/ipc.rs::postgres_database_ipc_allows_mmap_lifecycle_in_both_modes`
- Owner: `crates/confinement/src/permissions.rs`

### Plans refuse unsafe roots

Confinement compiles `ReadOnly` or `WorktreeWrite(VerifiedWorktree)` from
verified anchors. It refuses traversal, symlinked state or lock leaves,
credential roots and grants enclosing them, broad home or tmp grants and
Git-internal roots. `~/.ssh`, `~/.aws`, `~/.kube`, `~/.secrets` and Cargo
`credentials` are denied after every grant. Unsupported or nested
confinement fails `ConfinementUnavailable` and never widens access; Linux has
no backend.
Literal discovery grants accept absent entries or real directories through
  held, revalidated parents. They refuse symlinks and other entry types and
  grant no reads of children beyond existing read roots. Anchor refusals during
  discovery and held-directory revalidation name the path and failure stage.
- Why: one broad grant silently voids the boundary; directory discovery must
  not pass through regular-file validation or expand configuration access.
- Test: `crates/confinement/src/permissions.rs::broad_home_tmp_and_credential_roots_are_refused`;
  `::network_levels_shared_writes_and_hard_denies_render_in_order`;
  `::directory_discovery_keeps_literal_scope_and_refuses_symlinks`;
  `::anchored_directory_detects_replacement_at_its_sbpl_path`;
  `crates/providers/src/opencode.rs::homebrew_launch_plan_anchors_with_default_xdg_homes`;
  `::homebrew_writer_launch_plan_anchors_with_discovery_directories`
- Owner: `crates/confinement/src/permissions.rs`, `crates/providers/src/opencode.rs`

### Git write set

A writer owns a complete Git directory and object store under attempt scratch,
including its index, HEAD, refs, config, temporary files, stash, rebase and merge
state. Plain Git environment selects these paths; its object alternate supplies
read access to the shared store. The original worktree gitfile remains fixed
for host identity checks. A single small wrapper restores scrubbed environment
and publishing configuration where needed.

The linked admin directory and shared Git directory are read-only throughout
their subtrees. This includes the original index, all refs and reflogs, config,
hooks and info. No per-file write exception reopens either subtree. Provider
configuration and protected instructions retain their separate protections.
After provider exit, the host imports the pinned head through Git's receive
quarantine and moves the real branch with compare-and-swap. Lane configuration
is never executed as host configuration. Read-only lanes likewise cannot write
the common Git directory.
- Why: private Git supports ordinary commands while the shared metadata and
  object store retain one host-controlled write boundary.
- Test: `crates/confinement/src/permissions.rs::linked_worktree_grants_derive_from_verified_git_paths_and_ref`;
  `crates/confinement/tests/lane_git_conformance.rs`;
  `src/worktree_tests.rs::writer_production_plan_denies_outside_writes`
- Owner: `crates/confinement/src/permissions.rs`, `crates/providers/src/launch.rs`,
  `crates/run/src/objects.rs`

### Network and sockets

Network is a level: `provider` (API endpoints, when network is off),
`local` (adds localhost and the Postgres socket, when network is off with
Postgres capability) or `open` (adds outbound IP, the default for every lane).
At every level the Unix-socket peers are the DNS responder, the attempt-bound
[host Fabric relay](fabric.md#lane-store-isolation) and, from `local`, Postgres.
No lane reaches the peer agent's socket.
- Why: the peer agent relays to unconfined `fabric peer` children.
- Test: `crates/confinement/src/permissions.rs::native_sandbox_blocks_path_escape_probes`
- Owner: `crates/confinement/src/permissions.rs`

### No writes to stores the host runs later

No lane writes the Cargo registry, Git caches, pnpm, npm or Node caches the
host later runs unconfined. Writers get a private per-worktree cache, exactly
`$HOME/Library/Caches/provenant-lane-cargo/<sha256>` of the worktree and Git
identities, created by no-follow traversal with mode 0700; read-only lanes
link shared stores read-only and build offline. Only writers may execute
fixtures, and only in their own `tmp/candidate-N`. Only a profile declaring
`allow_pseudo_tty` (Agy's) may open a PTY.
- Why: a poisoned `build.rs` or package would run on the host's next build.
- Test: `crates/providers/src/tests.rs::no_lane_can_write_shared_toolchain_stores`;
  `crates/confinement/src/permissions.rs::writer_cache_is_private_and_bound_to_repository_and_worktree_identity`
- Owner: `crates/confinement/src/permissions.rs`, `crates/providers/src/`

### Host-side delivery

A writer task may carry `deliver`. Run binds the remote's single push URL and
the lane branch under writer custody. Only after a complete, verified
successful exit does the host push the final commit, fetch it back and
require it to equal the lane head, from the primary root with hooks,
fsmonitor and tag and submodule pushes off. URLs with embedded credentials,
query or fragment, shell SSH overrides and opaque credential helpers are
refused, and credentials travel only on the guardian pipe. A sealed start
marker stops the push repeating.
- Why: delivery is the one external effect a lane causes; it must be exactly
  once and never leak credentials.
- Test: `crates/run/src/delivery_tests.rs::host_delivery_fetches_commits_pushes_and_records_upstream`;
  `::prepare_refuses_remote_urls_with_embedded_credentials`
- Owner: `crates/run/src/delivery.rs`

### Disclosure and redaction

A prompt snapshot is scanned for secret patterns before it is sealed and is
refused on a match. The streaming redactor masks known secrets, credential URLs
and token patterns across chunk boundaries. Guardian capture attaches independent
known-shape filters before stdout or stderr reaches disk; a
redactor failure discards that output. The inherited environment is never
persisted.
- Why: lane output is stored and shared with other models.
- Test: `crates/confinement/src/redact.rs::known_secret_split_across_chunks_is_never_emitted`;
  `src/compose_tests.rs::guardian_capture_masks_split_credentials_before_retention`;
  `crates/confinement/src/disclosure.rs::snapshot_refuses_known_secrets_without_returning_content`
- Owner: `crates/confinement/src/redact.rs`, `crates/confinement/src/disclosure.rs`

### Quarantine

After quiescence, quarantine archives protected disk, index and HEAD
changes before restoring anything. Only attributable ordinary `.agents/`
edits are restored automatically; control-file changes, concurrent edits or
unaccountable history keep their evidence and end `review_required`. Clean
upstream changes are exempt, and a tracked symlink is checked by its target
bytes, never followed.
- Why: a lane must not silently change the instructions the next agent
  obeys.
- Test: `crates/confinement/src/quarantine.rs::policy_control_edit_is_archived_and_retained_for_review`;
  `::quarantine_archives_then_restores_only_attributable_ordinary_edit`
- Owner: `crates/confinement/src/quarantine.rs`

### Safe Git evidence

Evidence uses a pinned Git, typed operations, literal pathspecs and a
sanitised environment with hooks, fsmonitor, textconv, external diff, pager
and lazy fetch off. The repository and user configuration belong to the user and
are trusted; what a lane can influence is a config file in a location it can
write. Opening a repository, and again each linked worktree (whose branch
decides which `onbranch:` includes apply), lists the effective configuration
with its origins. Every configured filter driver is disabled, and a setting
read from a lane-writable include (an `include.path` or `includeIf` target in a
worktree or lane scratch) is pinned to the value the rest of the configuration
gives it, left alone where no utility command can reach it, and otherwise fails
the operation for that worktree. The configuration digest covers every included
file, so editing one makes the handle stale. `info/attributes` is tolerated
rather than refused. Copied gitfiles and alternates cannot vouch for protected
history, and a timeout, truncation or moved HEAD refuses complete evidence.
`run_utility` offers only a closed set of Git and `gh` reads.
Host advertisement and fetch (the pre-push default lookup, git-context bases,
worktree base pinning and delivery) share one transport environment with
hooks, fsmonitor and protocol limits, and refuse configuration read from a
lane-writable file or a `core.sshCommand`, `credential.helper` or
`remote.*.uploadpack`/`receivepack` that runs a program there. A helper the
user keeps elsewhere stays allowed. Git rereads configuration for the command
itself, so the check also lists the raw directives (`--no-includes`) and
refuses any `include.path` or `includeIf.*.path` whose target, resolved against
the including file, `~/` and symlinks, lies in a lane area, whether or not the
target exists or the `gitdir:`, `onbranch:` or `hasconfig:` condition holds
today. Targets outside lane areas are followed for onward includes.
Residual risk: Git still rereads the validated files, so a location a lane can
write that is not one of the enumerated lane areas (a world-writable directory
the user's own configuration includes from, say) can change what Git reads
between the check and the command; the host does not snapshot the values into
`-c` parameters because credential helpers and SSH settings the user needs are
scoped by URL and multi-valued.
- Why: repository content is attacker-controlled input to the host.
- Test: `crates/confinement/src/utility.rs::onbranch_include_of_a_lane_writable_file_is_neutralised_for_its_worktree`;
  `crates/confinement/src/utility.rs::host_transport_is_hardened_and_refuses_lane_writable_programs`;
  `crates/confinement/src/transport.rs::include_targets_in_lane_areas_are_refused_even_when_unread`;
  `crates/run/src/delivery_tests.rs::origin_refresh_refuses_includes_that_name_a_lane_area_before_they_exist`;
  `crates/confinement/src/utility.rs::filters_are_disabled_not_refused_and_empty_tree_source_suppresses_worktree_attributes`;
  `crates/confinement/src/repo.rs::git_revision_accepts_frozen_base_refs_and_refuses_option_injection`
- Owner: `crates/confinement/src/utility.rs`, `crates/confinement/src/transport.rs`

## Worktrees

### Git is the worktree registry

Git's porcelain inventory and verified common-directory/backlink membership
establish linked-worktree membership. `.worktrees/` identifies managed cleanup
scope. Persistent identity schemas, inode/device pins and path-digest matches
are absent. Canonical paths only locate exclusion slots. Supervisor's attempt
bindings and reservation journals remain launch and recovery evidence.

### One writer; removal never forces

One writer holds each registered linked worktree, and custody outlives the
requesting call. Landing and driver leases never substitute for it.
`worktree remove` refuses the primary, a dirty tree, ignored files, a live
writer lock or observed process, and has no force; it deletes the branch only when
contained in the integration branch or named by `--discard`. `worktree list`
never creates a lock.
Registered detached checkouts use the same checks and preserve all branches.
An unmerged detached HEAD refuses unless `--discard` is explicit, naming the
commit and a command to preserve it. Removal rechecks the exact HEAD and
optional branch by canonical path under writer custody.
- Why: removing a tree under a live writer destroys its work.
- Test: `src/worktree_tests.rs::second_writer_is_refused_while_first_holds_the_worktree`;
  `crates/confinement/src/worktrees.rs::writer_lock_busy_is_distinct_from_observed_live_process`;
  `::remove_registered_detached_worktree`;
  `::detached_removal_keeps_dirty_unmerged_and_live_work`
- Owner: `crates/confinement/src/worktrees.rs`

### Pruning and rollback need proof

`worktree remove --merged` proves a merge by ancestry, or a squash by a
recorded merged PR whose head equals the tip; a `gh` failure keeps the
branch. The integration branch and its tip are never pruned, refs are deleted
against their expected old OID, and there is no remote deletion. A failed
`worktree create` removes only what it made, and refuses `PrimaryMoved` if
the primary checkout's HEAD changed during the add.
- Why: pruning or rollback on a guess deletes the user's work.
- Test: `crates/confinement/src/worktrees.rs::merged_pruning_is_branch_scoped_and_keeps_unselected_refs`;
  `::timed_out_add_keeps_a_branch_it_did_not_create`. `PrimaryMoved` has
  no test (gap).
- Owner: `crates/confinement/src/worktrees.rs`

### GC deletes only proven-idle state

`gc` collects caches and expired provider state; merged worktree and output
deletion requires `--apply`. `--dry-run` previews all collection.
Cache collection takes the writer lock
without waiting; a held lock reports busy and keeps the cache. Missing lock
state is created only for mutation. Old identity and reservation records do not
gate cache collection. v1 owners are never idle.
Merged worktrees go only when clean, registered and with terminal lanes;
output pruning skips tracked and keep paths, dirty trees, symlinks and
writes within 24 hours. Deletion detaches and re-verifies identity first.
- Why: GC runs unattended at load and hourly; any doubt keeps data.
- Test: `crates/confinement/src/cache.rs::gc_idle_refuses_writers_that_became_live_or_unobservable`;
  `src/worktree_tests.rs::gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links`
- Owner: `crates/confinement/src/cache.rs`, `crates/confinement/src/worktrees.rs`

## Records

The attempt layout is the `AttemptFile` table, pinned by
`crates/records/src/attempt_file.rs::directories_and_classes_match_the_layout`.
Persisted state tolerates added fields within its supported schema version.
Version, scope, identity and custody validation still apply. Owner references and launch summaries carry no
PID-derived or replayable authority, and a stale observation cannot
establish death.
- Why: authority comes from held custody, never a file a lane can write.
- Test: `crates/supervisor/src/custody_tests.rs::contradictory_never_spawned_records_never_prove_absence`
- Owner: `crates/records/src/`, `crates/supervisor/src/custody.rs`

## Not provided

- Arbitrary-PID `reap`: a start-string check cannot reserve identity.
- `Drop`-driven stopping: abort and SIGKILL bypass `Drop`.
- `worktree remove --force` and stall verdicts.
