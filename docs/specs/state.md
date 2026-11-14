# State and config

Owner: `crates/core` (IDs, roots, time), `crates/records` (codecs, digests),
`crates/config` (config, release, managed install)
Anchor: `crates/core/src/roots.rs`, `crates/records/tests/contracts.rs`,
`crates/config/src/managed_tests.rs`

Keys and defaults: `provenant config schema`. Operator steps: the
[install](../runbooks/install.md) and [GC](../runbooks/gc.md) runbooks.

## Roots and identities

### Root selection never falls back

An empty or relative explicit root fails and never yields to the default. A
root equal to home or `/`, inside the checkout or a pre-1.0 root, or containing
`.agent-run` is refused. Reads create no state.
- Why: a wrong root mixes or destroys another installation's state.
- Test: `crates/core/src/roots.rs::roots_refuse_home_checkout_old_roots_and_any_agent_run_component`;
  `tests/parser.rs::root_uses_release_location_and_explicit_state_without_creating_it`
- Owner: `crates/core/src/roots.rs`, `src/roots.rs`

### Identifiers have one spelling

A value that can normalise two ways is rejected at the wire boundary.
- Why: two spellings let one record alias another.
- Test: `crates/core/src/ids.rs::identifiers_are_validated_at_the_wire_boundary`
- Owner: `crates/core/src/`

### Graph names retain one identity

A graph name identifies one graph generation for its project's lifetime.
Checkpoint collection leaves a durable name reservation because retained
attempt, chair and event records continue to refer to the graph by name. A
collected name is not allocated to a later graph.
- Why: reusing the name can collide with retained attempt keys or inherit a
  stale chair association.
- Test: `crates/fabric-runtime/src/graph/journal_tests.rs::checkpoint_collection_keeps_pending_graphs_and_reserves_collected_names`
- Owner: `crates/fabric-runtime/src/graph/journal.rs`,
  `src/fabric/interface.rs`

## Records

### Commit before signal

Owners commit durable state before the lossy change-generation signal; a
failed signal never fails the committed write. Readers treat an absent or reset
generation as unknown and re-read.
- Why: a hint must not become a second source of truth.
- Test: `crates/fabric-store/src/tests.rs::signal_failure_reports_a_committed_fabric_write`;
  `crates/cli/src/console.rs::changes_reports_unknown_and_comparison`
- Owner: `crates/os/src/changes.rs`

### Strict, bounded records

Unsupported versions fail before mutation. The reader rejects ambiguous or
unbounded input and never echoes the rejected value. Unknown fields survive round
trips. Edge-built facts are `Serialize` only.
- Why: a forged file must not become evidence.
- Test: `crates/records/tests/contracts.rs::bounded_decode_fails_closed_for_duplicates_utf8_trailing_and_integer_overflow`;
  `crates/records/tests/contracts.rs::typed_decode_errors_do_not_disclose_rejected_values`
- Owner: `crates/records/src/`

### One digest preimage

`CanonicalV1` bytes are deterministic and domain-framed; it is not RFC 8785.
- Why: a digest is an identity only if every writer computes the same bytes.
- Test: `crates/records/tests/contracts.rs::canonical_v1_sorts_keys_uses_required_escapes_and_separates_digest_domains`
- Owner: `crates/records/src/canonical.rs`

## Configuration

### Config never widens authority

Syntax, version and filesystem-boundary failures refuse loading; an invalid
optional value warns and uses the default, and a malformed restriction never
becomes permission. `config set` publishes atomically and keeps unknown keys;
`config show` redacts secrets.
- Why: errors need visible remedies without expanding authority.
- Test: `crates/config/src/tests.rs::config_warns_on_optional_input_but_refuses_syntax_and_versions_without_echoing_input`;
  `crates/config/src/tests.rs::knob_edit_preserves_unknown_keys_while_applying_the_edit`
- Owner: `crates/config/src/`

### Retention keeps evidence

GC expires resumable provider state only after archiving history, and leaves
results, captures, exit evidence and digests; resume then fails
`PROVIDER_STATE_EXPIRED` before spending. It never waits for locks, spares
reservations and pending continuations, deletes through identity-checked
tombstones, never follows links and writes nothing on a dry run
([ADR 0031](../adr/0031-retain-provider-history-and-expire-resume-state.md)).
The `RetentionClass` table in `config/gc.rs` records each class's owner,
custody proof, effective age keys and trigger. One scheduler routes regular
and full GC to lane caches, staging, provider state/history, chair logs, target seeds and
build-directory adapters. Admission uses the same table to trigger build-directory
reclamation. Each owner retains its deletion and identity checks.
Git worktree fences and permanent observation ledgers have no age rule;
retained, unimported Git work is never made deletable by age or disk pressure.
There is no collection-wide lock. Each adapter tries its item custody, reports
busy items in GC output and leaves them for a later pass. Admission and Git
integrity boundaries remain in force. Alert and cadence contention also skip
the busy item rather than wait.

Quota history and usage rotation share `os::fs::ledger::Ledger`: each JSONL
record is one O_APPEND write, and rotation renames whole files into the
configured tail. Neither append nor rotation takes a ledger lock. Terminal
usage evidence has no age limit. Concurrent terminal retries may append the
same attempt; readers reduce duplicates by project and attempt and report
conflicting rows, so retry races never inflate spend. Quota cache updates
try-lock and skip when busy. Config writes publish a temporary file by atomic
rename; concurrent config edits use last publication wins. Advisory config
history rewrites can likewise omit a concurrent entry. First project bindings
converge on a canonical-path-derived identity and publish one record with
no-replace atomic publication; a competing publisher validates the winner.
Host identity uses the same no-replace publication and reads the winning ID.
- Why: freeing space must not destroy evidence or a session in use.
- Test: `crates/run/src/retention.rs::gc_spares_nonterminal_attempts_and_contention_and_reservations`;
  `crates/run/src/retention.rs::gc_retries_only_authorised_tombstones_and_never_follows_links`
- Test: `crates/os/src/fs/ledger.rs::tests`;
  `crates/telemetry/src/ledger/tests.rs::concurrent_retries_count_an_attempt_once`;
  `crates/run/src/retention.rs::age_collection_keeps_unimported_git_until_custody_is_proven`;
  `crates/run/src/target_seed_gc.rs::tests::active_clone_lock_prevents_collection`
- Owner: `crates/run/src/retention.rs`

### 2.0 runs from a fresh state root

The default state root is `~/.local/share/provenant/v2`
(`provenant_core::DEFAULT_STATE_SUFFIX`); the 1.x root
`~/.local/share/provenant/v1` (`LEGACY_V1_STATE_SUFFIX`) is the only other
spelling. 2.0 never writes, moves or deletes the 1.x root and refuses any state
root inside it, so no 2.0 code opens a 1.x store. The first install at the
default root, while that root is absent or empty and a 1.x root exists, stops
every 1.x launch agent by label, then copies `config.toml`, `host-id` and
`install/receipt.json` (regular files only, modes kept) into a staged directory
renamed into place. A failed stop copies nothing; a non-empty root and any
other explicit root are never seeded. The copied receipt's links stay owned, so
the install replaces them. A dry run reports the copy and writes nothing.
- Why: rollback is reinstalling 1.x, which needs its root intact, and two
  versions must never run agents at once.
- Test: `src/state_handoff.rs::copies_only_owner_configuration_and_never_writes_the_old_root`;
  `tests/install.rs::first_install_at_a_fresh_root_copies_only_1x_configuration`
- Owner: `src/state_handoff.rs`, `src/install.rs`, `install.sh`

The format-1 reset in `src/state_reset.rs` is not on the 2.0.0 install path. A
later release turns it into an owner-gated removal of the 1.x root. Its table
of versions shipped by 1.x releases, audited across `v1.0.0` through `v1.11.1`,
is `FORMAT_ONE_VERSIONS`:

| Store | Released 1.x current versions |
| --- | --- |
| `run.sqlite3` | 3, 4, 6, 7, 8, 9, 10 |
| `fabric/fabric.sqlite3` | 7, 13, 15 |
| `fabric/host.sqlite3` (introduced in v1.3.0) | 13, 15 |
| `admission.sqlite3` | 1, 2, 5, 6 |
| `pr-contributions.sqlite3` (introduced in v1.3.0) | 4 |

## Release identity

### The source digest is the binary's identity

It covers shipped source, lockfile, toolchain, packager and bundled assets, with
no override; test-only edits do not change it.
- Why: the installed binary must be provably the reviewed source.
- Test: `crates/records/src/source.rs::hashes_sorted_framed_names_and_exact_contents`;
  `build.rs::member_test_edits_do_not_change_the_source_digest`
- Owner: `build.rs`, `crates/records/src/source.rs`

### Releases are immutable

Install verifies and smoke-tests a release before trusting it. `release.json`
is published last and a published release is never repaired. A digest proves
identity, not publisher trust.
- Why: a partial or swapped release must never run.
- Test: `crates/config/src/managed_tests.rs::bytes_and_modes_are_verified_and_existing_release_is_immutable`;
  `crates/config/src/managed_tests.rs::interrupted_release_publish_leaves_no_manifest_and_reapplies_with_real_staging_file`
- Owner: `crates/config/src/release.rs`

## Managed install

### Preview before every write

A failed preview prevents writes, and `--dry-run` creates no root, lock or
receipt. Identity and bytes are rechecked before each publication; drift stops
the install.
- Why: the user's harness config changes only in view.
- Test: `crates/config/src/managed_tests.rs::dry_run_and_broken_preview_create_nothing`;
  `crates/config/src/managed_tests.rs::drift_between_preview_and_apply_refuses_before_file_writes`
- Owner: `crates/config/src/managed.rs`

### Only proven-owned entries change

Removal unlinks only entries whose value still matches the receipt, never
recursively. `--adopt-existing` never authorises deletion. After a home or
state-root move, stale entries are reported without their paths being inspected.
- Why: an old path may now belong to something else.
- Test: `crates/config/src/managed_tests.rs::uninstall_keeps_edited_links_and_unmanaged_siblings_and_binary`;
  `crates/config/src/managed_tests.rs::uninstall_after_home_rename_never_acts_on_old_paths`
- Owner: `crates/config/src/managed.rs`

### Foreign bytes and user hooks survive

Edits replace only owned values, keeping comments and order; malformed or
symlinked configs are refused unchanged. Omitting `--hooks` refreshes unchanged
owned hooks and releases edited ones; `--hooks on` refuses an edited hook.
- Why: a harness config holds the user's other tools and secrets.
- Test: `crates/config/src/managed_tests.rs::five_client_install_preserves_foreign_hooks_comments_and_reapplies`;
  `crates/config/src/managed_tests.rs::edited_inherited_hook_is_relinquished_but_explicit_on_names_recovery`
- Owner: `crates/config/src/splice.rs`, `crates/config/src/hooks.rs`

### LaunchAgents are owned like any entry

The peer-agent and tick plists join the preview and receipt. A symlink at the
plist or a parent is refused, and an older plist is adopted only on an exact
byte match.
- Why: a LaunchAgent runs unattended with the user's authority.
- Test: `crates/config/src/managed_tests.rs::peer_agent_refuses_symlinked_plist_and_parent_before_writes`;
  `crates/fabric-runtime/src/peer_agent.rs::provider_path_skips_a_registered_project_and_clones_nested_in_it`
- Owner: `crates/config/src/managed.rs`, `src/tick_agent.rs`

### Re-apply is the recovery

A partial install lists applied and remaining actions, and rerunning `install`
recovers it, even with no receipt. There are no rollback journals.
- Why: one idempotent path is easier to trust than a rollback engine.
- Test: `crates/config/src/managed_tests.rs::crash_after_any_publication_reapplies_without_a_receipt`;
  `crates/config/src/managed_tests.rs::receipt_publish_failure_reports_partial_actions_and_reapplies`
- Owner: `crates/config/src/managed.rs`
