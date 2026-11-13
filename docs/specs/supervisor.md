# Supervisor and confinement

Owner: `crates/supervisor` (custody), `crates/confinement` (permissions,
evidence, worktrees), `crates/os` (bounded I/O and execution)
Anchor: `crates/supervisor/tests/custody.rs`, `crates/supervisor/src/custody_tests.rs`,
`crates/confinement/src/permissions.rs`, `src/worktree_tests.rs`

Supervisor holds a provider's process, output and custody through durable exit
publication; confinement turns an authorised plan into filesystem, Git, network
and provider permissions. Wiring and platform limits are in the
[crate notes](../../tools/front-door/crates/supervisor/README.md). Paths are
relative to `tools/front-door/`.

## Custody

### Closed stop reasons; natural exit wins

Only `ProviderGroup::stop(StopReason)` signals a provider: cancellation, the
wall-clock runtime deadline, protocol rejection, a supervisor signal or
post-terminal cleanup. Silence, inactivity, capture failure, ENOSPC and failed
publication are never stop reasons, and there is no startup deadline. A natural
exit wins over a late cancel or deadline, and a host sleep cannot extend the
deadline.
- Why: a bookkeeping fault or slow start must never kill a healthy provider,
  and a stuck one must not hold a worktree for hours.
- Test: `crates/supervisor/src/machine.rs::silence_capture_and_observation_failures_are_not_stop_reasons`;
  `::a_wnowait_exit_wins_over_late_cancel_and_deadline`;
  `::a_host_sleep_cannot_extend_the_wall_deadline`
- Owner: `crates/supervisor/src/machine.rs`

### Signals need proven identity and scope

The PID stays reserved until the handle is reaped last. Only the unreaped
provider, its group while the leader is unreaped, and proven attempt-sandbox
escapees may be signalled; a nested owner with proven custody is spared. A
census or cwd observation never grants signal authority, and recovery signals
only a PID whose recorded identity matches.
- Why: signalling a recycled PID or a sibling's group kills unrelated work.
- Test: `crates/supervisor/tests/process.rs::inherited_wait_policy_preserves_pid_reservation`;
  `crates/supervisor/tests/custody.rs::stale_complete_census_does_not_signal_a_late_shared_group_owner`;
  `::nested_owner_stays_protected_when_native_custody_proof_is_uncertain`;
  `crates/supervisor/src/custody/orphans.rs::an_unprovable_pid_is_never_signalled_and_is_named`
- Owner: `crates/supervisor/src/process_os/`, `crates/supervisor/src/custody.rs`

### Guardian holds custody past its owner

The guardian keeps the provider, attempt lock and writer lease after its owner
dies. Secrets travel in memory and pipes, never argv or disk.
A missing launch acknowledgement is an unknown launch, never permission to
launch again.
- Why: losing the caller must not lose custody or double-launch a writer.
- Test: `crates/supervisor/tests/custody.rs::owner_death_keeps_guardian_and_writer_live`;
  `crates/os/src/exec/tests.rs::secret_input_arrives_by_descriptor_with_eof_and_no_token_environment`.
  The unknown-launch rule has no test (gap).
- Owner: `crates/supervisor/src/guardian/`

### Quiescence before release

A writer stays fenced until the provider is reaped, two complete censuses show
no surviving member or escapee, and a fresh lock on the original lease inode
succeeds. Failed, racing or incomplete reads keep the fence; only a recorded
decision with matching authority clears one, and that decision permits no signal.
- Why: a leftover descendant could still write the worktree.
- Test: `crates/supervisor/src/guardian/tests.rs::darwin_settlement_requires_complete_absence_of_members_and_recorded_escapes`;
  `crates/supervisor/src/custody_tests.rs::an_unrecorded_escape_cannot_be_cleared_by_an_empty_census`;
  `::reconcile_requires_matching_authority_and_records_before_release`

### Provider descriptors and detached services

Writer, custody, port and directory descriptors close on provider exec. Only
explicit executable inputs (secrets and confinement profiles) cross that edge;
the private guardian handoff retains its capability transfer. An ordinary
service detached by an unsandboxed provider therefore cannot retain the lane's
inherited writer lease. Guardian loss still leaves the durable unresolved
reservation fenced, even if the kernel lease is free.

Cleanup has a bounded stop budget. At its deadline a separate, cooperatively
bounded diagnostic census names observable PIDs, short native command names and
open writer-lock paths, with a reconcile action. An open path is not proof of
flock ownership and grants no signal authority. Unknown or unreadable holders
remain explicit uncertainty. The guardian continues retaining unresolved
custody; a timeout alone never authorises settlement or an unrelated signal.
- Test: `crates/supervisor/src/process_os/provider.rs::lane_descriptors_are_cloexec`;
  `::orphaned_provider_descendant_does_not_retain_writer_flock`;
  `crates/supervisor/tests/custody.rs::guardian_loss_never_invents_exit_or_releases_the_reservation`

### Durable exit publication

The exit record is published without replacing an existing one, after what it
references is durable. A failed publication keeps custody and retries; an
unconfirmed sync is `durability_unconfirmed`, never absent.
- Why: a lost or overwritten exit record loses the only proof of outcome.
- Test: `crates/supervisor/tests/publication.rs::exit_publication_enospc_and_visible_sync_retry_keeps_one_snapshot`;
  `crates/supervisor/tests/custody.rs::publication_failure_retains_the_lease_and_retries_without_overwrite`

### Writer reservation and settlement

The writer reservation is published before spawn and cleared only after
quiescence and run's terminal commit, or an authorised reconcile; the worktree
flock stays held through import and [publication](run.md#publication-order). A
missing or malformed reservation refuses recovery of that attempt, and scratch
deletion needs a verified settlement commit.
- Why: a writer must never be freed on a partial or forged settlement.
- Test: `crates/supervisor/src/custody_tests.rs::missing_or_malformed_reservation_does_not_reopen_a_writer`;
  `::settlement_checks_exact_attempt_and_both_digests_before_scratch`;
  `::worktree_settlement_retains_flock_until_recovery_is_dropped`

### Stable locks and atomic writes

Lock inodes are never unlinked while held, and acquisition never revokes or
steals a lease. Authoritative writes are private temporaries, synced, renamed
atomically and followed by a parent sync. Owner records carry no PID-derived or
replayable authority.
- Why: a recreated lock inode lets two owners hold one lock, and a crash must
  leave the old or new file whole.
- Test: `crates/os/src/fs/tests.rs::stable_lock_namespace_refuses_every_mutation_without_splitting_exclusion`;
  `::process_crash_at_publication_boundaries_leaves_old_or_new_complete_bytes`;
  `crates/supervisor/src/custody_tests.rs::contradictory_never_spawned_records_never_prove_absence`
- Owner: `crates/os/src/fs.rs`

### Bounded execution

Captures and frames are bounded; an oversized frame is decoder loss, never a
fabricated terminal event.
- Why: unbounded output must not stall custody or invent an outcome.
- Test: `crates/os/src/capture.rs::oversized_frames_are_counted_and_never_decoded_as_terminal`

## Permission policy

Lane profiles start open and deny the boundaries
([ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md)); rendered
profiles are pinned by
`crates/providers/src/seatbelt_snapshots.rs::seatbelt_profiles_match_snapshots`.

### Plans refuse unsafe roots

Confinement refuses traversal, symlinked state or lock leaves, credential roots
and grants enclosing them, broad home or tmp grants and Git-internal roots;
credential directories are denied after every grant. Unsupported confinement
fails `ConfinementUnavailable` and never widens access. An optional root that
resolves into a credential root is omitted with a warning.
- Why: one broad grant silently voids the boundary.
- Test: `crates/confinement/src/permissions/tests/validation.rs::broad_home_tmp_and_credential_roots_are_refused`;
  `::directory_discovery_keeps_literal_scope_and_refuses_symlinks`
- Owner: `crates/confinement/src/permissions.rs`

### Shared Git metadata is read-only

A writer owns a private Git directory with the shared object store as a
read-only alternate; shared refs, config, hooks and admin metadata stay
read-only, and the host imports the result and moves the branch by
compare-and-swap.
- Why: ordinary Git works in the lane while one host boundary guards shared
  metadata.
- Test: `crates/confinement/src/permissions/tests/grants.rs::linked_worktree_grants_derive_from_verified_git_paths_and_ref`;
  `crates/confinement/tests/lane_git_conformance.rs::lane_git_supports_stash_rebase_and_merge_with_shared_metadata_read_only`

### No writes to stores the host runs later

No lane writes the Cargo registry, Git, pnpm, npm or Node caches the host later
runs unconfined; writers get a private per-worktree cache.
- Why: a poisoned `build.rs` or package would run on the host's next build.
- Test: `crates/providers/src/tests_lane_toolchains.rs::no_lane_can_write_shared_toolchain_stores`;
  `crates/confinement/src/permissions/tests/grants.rs::writer_cache_is_private_and_bound_to_repository_and_worktree_identity`

### Network, IPC and process information

No lane reaches the peer agent's socket. Lanes may read process information
but never another process's arguments, and signals stay inside the lane's own
sandbox. SysV IPC is host-global on macOS, so same-user IPC isolation needs a
separate OS user or VM.
- Why: the peer agent relays to unconfined children, and Seatbelt can enforce
  no more.
- Test: `crates/confinement/src/permissions.rs::native_sandbox_blocks_path_escape_probes`;
  `crates/confinement/src/permissions/tests/signals.rs::same_sandbox_signals_reach_parents_but_cannot_reach_host_processes`;
  `crates/confinement/src/permissions/tests/process_info.rs::process_info_includes_host_and_reports_own_rss`

### Lane process-tree memory interface

`provenant process-tree --root PID --json` reads a host-sampled snapshot of the
lane's own descendants; the lane gets one exact read grant and no process-listing
authority. A missing, stale, incomplete or unsupported sample is exit 4
`PROCESS_TREE_UNAVAILABLE` with null data, never a partial tree or zero memory.
The sample is periodic observation, not an atomic census or a memory limit.
- Why: a memory guard that reads lost observation as zero would approve a
  runaway build.
- Test: `crates/confinement/src/permissions/tests/process_tree.rs::lane_process_tree_sample_is_private_under_production_profiles`;
  `tests/command_adapter.rs::command_lane_reads_its_supervisors_process_tree_through_the_cli`;
  `::process_tree_cli_is_unavailable_without_a_supervisor_sample`
- Owner: `crates/supervisor/src/process_tree.rs`, `src/process_tree.rs`

### Disclosure and safe Git evidence

A prompt snapshot with a known secret is refused before sealing, and output is
redacted across chunk boundaries before it reaches disk. Host Git runs with
hooks, filters, fsmonitor and external programs off, and refuses configuration
from a lane-writable include. Host delivery pushes once, from the host, and
refuses credential-bearing URLs.
- Why: repository content and lane output are attacker-controlled input to the host.
- Test: `crates/confinement/src/redact.rs::shell_output_secrets_split_across_chunks_are_never_emitted`;
  `crates/confinement/src/disclosure.rs::snapshot_refuses_known_secrets_without_returning_content`;
  `crates/confinement/src/transport.rs::include_targets_in_lane_areas_are_refused_even_when_unread`;
  `crates/run/src/delivery_tests/settlement.rs::prepare_refuses_remote_urls_with_embedded_credentials`
- Owner: `crates/confinement/src/redact.rs`, `crates/confinement/src/utility.rs`

## Worktrees

### One writer; removal never forces

Git's own inventory is the worktree registry. One writer holds each linked
worktree, and custody outlives the requesting call. `worktree remove` refuses
the primary, a dirty tree, a live writer or observed process, and has no force;
it deletes a branch only when merged or named by `--discard`.
- Why: removing a tree under a live writer destroys its work.
- Test: `src/worktree_tests/writer_cases.rs::second_writer_is_refused_while_first_holds_the_worktree`;
  `crates/confinement/src/worktrees/lifecycle_tests/gc_safety.rs::detached_removal_keeps_dirty_unmerged_and_live_work`
- Owner: `crates/confinement/src/worktrees.rs`

### Unreadable processes need positive non-occupancy evidence

A root is `Free` only when every possible writer was observed completely with
stable identity; a placed occupant makes it `Live`, and a failed or partial
query leaves it `Unknown`, never empty. A process whose real, effective and
saved uids are all stable system uids other than the root owner's is outside
the writer cohort; host exceptions are in
[configuration](../runbooks/configuration.md#unreadable-process-census-exceptions).
Landing and collection require `Free`; the census grants no signal authority.
- Why: a retained cwd or descriptor survives permission changes, so current
  permissions cannot prove absence.
- Test: `crates/confinement/src/worktrees/census_tests.rs::writer_cohort::unreadable_foreign_system_process_allows_merged_worktree_and_branch_cleanup`;
  `::writer_cohort::possible_user_writers_and_positive_system_occupants_remain_protected`;
  `::native_full_census_places_clean_merged_worktree_and_same_user_sleep`
- Owner: `crates/confinement/src/worktrees.rs`, `crates/os/src/sys/pid.rs`

### Pruning and GC need proof

Merged pruning proves a merge by ancestry or a recorded merged PR at the tip,
never prunes the integration branch, and deletes refs against their expected
old OID. GC keeps anything in doubt: a held writer lock keeps a cache, and a
worktree goes only clean, registered and with terminal lanes.
- Why: GC runs unattended; pruning on a guess deletes the user's work.
- Test: `crates/confinement/src/worktrees/lifecycle_tests/worktree_operations.rs::merged_pruning_is_branch_scoped_and_keeps_unselected_refs`;
  `crates/confinement/src/cache.rs::gc_idle_refuses_writers_that_became_held_or_unobservable`
- Owner: `crates/confinement/src/worktrees.rs`, `crates/confinement/src/cache.rs`
