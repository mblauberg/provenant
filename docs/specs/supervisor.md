# Supervisor and confinement

Owner: `crates/supervisor` (custody), `crates/confinement` (permissions,
evidence, worktrees), `crates/os` (bounded I/O and execution)
Anchor: `crates/supervisor/tests/custody.rs`, `crates/supervisor/src/custody_tests.rs`,
`crates/confinement/src/permissions.rs`, `src/worktree_tests.rs`

Supervisor holds processes, output and kernel locks from launch through
durable exit publication. Confinement turns an authorised plan into
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

The writer reservation is published under the writer lock before spawn and
cleared only after quiescence, quarantine and run's terminal commit, or an
authorised reconcile decision; a missing or malformed reservation refuses
acquisition. `SettlementCommit`, needed for scratch deletion and fence
clearing, is issued only after the attempt inode, custody lock and both
digests in `release.json` are verified. After a verified boot change, a
reservation whose recorded device number differs from the current one (APFS
can renumber volumes across a reboot) is still recoverable when the
reservation and attempt shared a device, their current directories still have
the recorded inodes, and both now share a device. Within one boot, a changed
device number refuses recovery.
- Why: a writer must never be freed on a partial or forged settlement.
- Test: `crates/supervisor/src/custody_tests.rs::missing_or_malformed_reservation_does_not_reopen_a_writer`;
  `::settlement_checks_exact_attempt_and_both_digests_before_scratch`;
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

Rendered profiles are generated reference in
`docs/reference/seatbelt/<provider>-<mode>.sb`, pinned by
`crates/providers/src/seatbelt_snapshots.rs::seatbelt_profiles_match_snapshots`.
The rules below are what every profile must keep.

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

Writers may change only their linked tree, its private index, HEAD and merge
state, objects, exactly their branch ref with its lock and reflog, and private
`FETCH_HEAD`. Remote-tracking refs and reflogs, other refs, packed refs,
common and worktree config, gitfiles, provider configuration and protected
instructions stay immutable. Read-only lanes never write the common Git directory.
- Why: a writer that could move another ref or config could rewrite history
  or plant hooks.
- Test: `crates/confinement/src/permissions.rs::linked_worktree_grants_derive_from_verified_git_paths_and_ref`;
  `src/worktree_tests.rs::writer_production_plan_denies_outside_writes`
- Owner: `crates/confinement/src/permissions.rs`

### Network and sockets

Network is a level: `provider` (API endpoints, when network is off),
`local` (adds localhost and the Postgres socket, when network is off with
Postgres capability) or `open` (adds outbound IP, the default for every lane).
At every level the only Unix-socket peers are the DNS responder and, from
`local`, Postgres, so no lane reaches the peer agent's socket.
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

Disclosure scanning is bounded (10,000 files, 64 MiB) and refuses when the
budget runs out. One streaming redactor masks known secrets, credential URLs
and token patterns across chunk boundaries before anything reaches disk; a
redactor failure discards that output. The inherited environment is never
persisted.
- Why: lane output is stored and shared with other models.
- Test: `crates/confinement/src/redact.rs::known_secret_split_across_chunks_is_never_emitted`;
  `crates/confinement/src/disclosure.rs::bounded_scan_refuses_secret_patterns_without_returning_content`
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
and lazy fetch off. Copied gitfiles and alternates cannot vouch for
protected history, and a timeout, truncation or moved HEAD refuses complete
evidence. `run_utility` offers only a closed set of Git and `gh` reads.
- Why: repository content is attacker-controlled input to the host.
- Test: `crates/confinement/src/utility.rs::filter_config_is_refused_and_empty_tree_source_suppresses_worktree_attributes`;
  `crates/confinement/src/repo.rs::git_revision_accepts_frozen_base_refs_and_refuses_option_injection`
- Owner: `crates/confinement/src/utility.rs`

## Worktrees

### One writer; removal never forces

One writer holds each registered linked worktree, and custody outlives the
requesting call. Landing and driver leases never substitute for it.
`worktree remove` refuses the primary, a dirty tree, ignored files, a live
writer and any reservation, and has no force; it deletes the branch only when
contained in the integration branch or named by `--discard`. `worktree list`
never creates a lock.
- Why: removing a tree under a live writer destroys its work.
- Test: `src/worktree_tests.rs::second_writer_is_refused_while_first_holds_the_worktree`;
  `crates/confinement/src/worktrees.rs::removal_refuses_live_writer_and_any_reservation_record`
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
without waiting; a held lock keeps a cache live, and a missing lock, a
reservation or unverifiable registration keeps it. v1 owners are never idle.
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
Writers refuse unknown versions and fields; read-only observers tolerate
added fields within v1. Owner references and launch summaries carry no
PID-derived or replayable authority, and a stale observation cannot
establish death.
- Why: authority comes from held custody, never a file a lane can write.
- Test: `crates/supervisor/src/custody_tests.rs::contradictory_never_spawned_records_never_prove_absence`
- Owner: `crates/records/src/`, `crates/supervisor/src/custody.rs`

## Not provided

- Arbitrary-PID `reap`: a start-string check cannot reserve identity.
- `Drop`-driven stopping: abort and SIGKILL bypass `Drop`.
- `worktree remove --force` and stall verdicts.
