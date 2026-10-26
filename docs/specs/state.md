# State and config

Owner: `crates/core` (IDs, roots, time), `crates/records` (codecs, digests),
`crates/config` (config, release, managed install)
Anchor: `crates/core/src/roots.rs`, `crates/records/tests/contracts.rs`,
`crates/config/src/tests.rs`, `crates/config/src/managed_tests.rs`

State owns root selection, identities, record codecs, typed configuration and
the managed installation. The [cross-cutting
decisions](README.md#cross-cutting-decisions) fix the root, layout and
environment. Config keys are generated reference in the [config
reference](../reference/config-keys.md); the attempt layout is `AttemptFile`
in `crates/records/src/attempt_file.rs`. Operator steps are in the [install
runbook](../runbooks/install.md) and [GC runbook](../runbooks/gc.md). Paths
are relative to `tools/front-door/`.

## Roots and identities

### Root selection never falls back

The root is `--state-dir ABS`, then `PROVENANT_STATE_DIR`, then
`~/.local/share/provenant/v1`; empty or relative explicit values fail.
Roots equal to home or `/`, within the product checkout or a pre-1.0 root,
or containing `.agent-run` are refused, after resolving existing ancestor
aliases. Old-root contents are never opened. Reads, help and `root` create no
state; mutations create directories 0700 and files 0600.
- Why: a wrong root mixes or destroys another installation's state.
- Test: `crates/core/src/roots.rs::roots_refuse_home_checkout_old_roots_and_any_agent_run_component`;
  `tests/parser.rs::root_uses_release_location_and_explicit_state_without_creating_it`
- Owner: `crates/core/src/roots.rs`, `src/roots.rs`

### Exact identifiers and time

IDs are newtypes over 16 random bytes, shown as 32 lowercase hex digits.
`GitOid` accepts only complete 40 or 64 digit names. `RelativePath` rejects
absolute components, `.` and `..`, and durable paths use tagged `PathWire`,
never lossy text. `WallTime` is UTC integer microseconds with one rendering,
durations use checked arithmetic, and `CancelToken` is never serialised or a
provider signal.
- Why: a value that normalises two ways lets one record alias another.
- Test: `crates/core/src/ids.rs::identifiers_are_validated_at_the_wire_boundary`;
  `crates/core/src/paths.rs::relative_paths_reject_normalisation_and_parent_traversal`
- Owner: `crates/core/src/`

## Records

### Strict, bounded records

Stored records are typed serde models with `schema_version`, closed enums and
`deny_unknown_fields`; unknown major versions fail before mutation. The
reader rejects duplicate members, invalid UTF-8, trailing input, non-finite
numbers and out-of-range integers, bounds bytes while reading (1 MiB config,
8 MiB records, depth 64) and never echoes the rejected value. Edge-built
facts are `Serialize` only, so deserialisation cannot manufacture evidence.
- Why: ambiguous JSON parses differently in different readers, and a forged
  file must not become evidence.
- Test: `crates/records/tests/contracts.rs::bounded_decode_fails_closed_for_duplicates_utf8_trailing_and_integer_overflow`;
  `::typed_decode_errors_do_not_disclose_rejected_values`
- Owner: `crates/records/src/`

### Digest preimage

`CanonicalV1` is UTF-8 JSON with no whitespace, keys sorted recursively by
UTF-8 bytes, array order preserved, minimal string escapes, no Unicode
normalisation, shortest decimal integers and no floats; it is not RFC 8785.
`record_digest(kind, version, bytes)` hashes a fixed domain prefix, kind,
version and canonical bytes as `sha256:<64 hex>`. `FileDigest` hashes raw
bytes as a separate type. Domain owners choose which fields enter a digest.
- Why: a digest is an identity only if every writer computes the same bytes.
- Test: `crates/records/tests/contracts.rs::canonical_v1_sorts_keys_uses_required_escapes_and_separates_digest_domains`;
  `crates/records/src/canonical.rs::record_digest_uses_exact_domain_framing_and_canonical_input_bytes`
- Owner: `crates/records/src/canonical.rs`

### YAML subset

Skill frontmatter and `agents/*.yaml` use a bounded YAML subset (ADR 0028)
that refuses anchors, aliases, tags, flow maps, multiple documents, tabs,
duplicate keys and multiline plain scalars; only `true` and `false` resolve.
- Why: YAML's implicit typing and references are a parsing hazard.
- Test: `crates/records/tests/contracts.rs::yaml_subset_parses_frontmatter_and_sidecars_and_refuses_ambiguous_values`;
  `crates/records/tests/external_yaml.rs::skill_frontmatter_and_agent_sidecars_use_the_subset`
- Owner: `crates/records/src/`

## Configuration

### Strict typed config

`config.toml` declares `schema_version = 1`; unknown keys, duplicates, bad
types or ranges and unsupported versions fail, absence means defaults, and a
symlinked or unreadable file is an error. There is no include,
interpolation, migration, profile layering or per-knob environment
variable, and a preset never grants authority. `config set` writes
atomically and refuses to rewrite an invalid file; `config show` redacts
secret leaves and never mutates.
- Why: implicit or executable configuration cannot be audited.
- Test: `crates/config/src/tests.rs::closed_toml_boundary_refuses_unknowns_duplicates_bad_types_and_versions_without_echoing_input`;
  `::knob_edit_refuses_to_rewrite_an_invalid_config`
- Owner: `crates/config/src/`

### Provider-state retention

Settlement prunes only proven regenerable provider caches. GC expires
credential entries after 24 hours and resumable state after
`retention.provider_state_days`, archiving history first and leaving result,
captures, exit evidence and digests untouched; resume then fails
`PROVIDER_STATE_EXPIRED` before spending. GC takes locks without waiting,
spares reservations and pending continuations, disposes through
identity-checked tombstones, never follows links and writes nothing on a dry
run (ADR 0031).
- Why: freeing credentials and space must not destroy evidence or a session
  in use.
- Test: `crates/run/src/retention.rs::gc_spares_nonterminal_attempts_and_contention_and_reservations`;
  `::gc_retries_only_authorised_tombstones_and_never_follows_links`
- Owner: `crates/run/src/retention.rs`

## Release identity

### Source digest preimage

The source digest covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain, Cargo
configuration, the release packager and bundled assets. Names are sorted
workspace-relative POSIX paths, framed by lengths, under the prefix
`provenant-source-v1`. Duplicate, missing, symlinked or escaping inputs are
rejected and there is no override; test-only edits do not change it.
- Why: the digest is the installed binary's identity.
- Test: `crates/records/src/source.rs::hashes_sorted_framed_names_and_exact_contents`;
  `build.rs::member_test_edits_do_not_change_the_source_digest`
- Owner: `build.rs`, `crates/records/src/source.rs`

### Immutable releases

Install verifies manifest, platform, bytes and modes and smoke-tests the
binary before trusting a release. `release.json` is published last; the
release is then immutable and never repaired. Equal digests with different
manifests fail `ReleaseConflict`. A digest proves identity, not publisher
trust.
- Why: a partial or swapped release must never run.
- Test: `crates/config/src/managed_tests.rs::bytes_and_modes_are_verified_and_existing_release_is_immutable`;
  `::interrupted_release_publish_leaves_no_manifest_and_reapplies_with_real_staging_file`
- Owner: `crates/config/src/release.rs`

## Managed install

### Preview before every write

Every harness-config change is printed before it is applied, a failed
preview prevents writes, and `--dry-run` creates no root, lock or receipt.
Identity and bytes are rechecked before each atomic publication, and drift
stops the install. Locks serialise cooperating installers only, so quit
harness clients first.
- Why: the user's harness config changes only in view.
- Test: `crates/config/src/managed_tests.rs::dry_run_and_broken_preview_create_nothing`;
  `::drift_between_preview_and_apply_refuses_before_file_writes`
- Owner: `crates/config/src/managed.rs`

### Only unchanged owned entries are removed

Removal unlinks only entries whose current value matches the receipt, never
recursively; edited entries, unmanaged siblings, state, user skills, config
and the running binary stay. `--adopt-existing` never authorises deletion
and refuses symlinked executables, foreign directories and inline secret
environment. Protected destinations and mount descendants are refused before
any read.
- Why: install must never delete what it cannot prove it wrote.
- Test: `crates/config/src/managed_tests.rs::uninstall_keeps_edited_links_and_unmanaged_siblings_and_binary`;
  `::mcp_conflict_does_not_adopt_foreign_env_or_create_state`
- Owner: `crates/config/src/managed.rs`

### Editors preserve foreign bytes

TOML edits use `toml_edit`; JSON and JSONC edits splice owned leaves and
reparse, keeping comments, trailing commas and order. Malformed, duplicate
or ambiguous containers are rejected unchanged, the full user config is
never logged, and symlinked configs are refused. Each MCP entry runs the
absolute stable binary with `--state-dir ABS fabric mcp --client NAME` and
no secret environment, matching the recorded client contracts.
- Why: a harness config holds the user's other tools and secrets.
- Test: `crates/config/src/managed_tests.rs::five_client_install_preserves_foreign_hooks_comments_and_reapplies`;
  `::mcp_registry_matches_recorded_client_config_contracts`
- Owner: `crates/config/src/splice.rs`, `crates/config/src/harness.rs`

### Hook ownership

Omitting `--hooks` refreshes unchanged owned hooks and relinquishes edited
ones; a first install leaves hooks off. `--hooks on` refuses edited Claude
hooks and names `--hooks off` as recovery, which removes exact owned
matches. A user's own Codex `hooks = true` is kept.
- Why: a user's hook edits outrank the installer's.
- Test: `crates/config/src/managed_tests.rs::edited_inherited_hook_is_relinquished_but_explicit_on_names_recovery`;
  `tests/install.rs::hook_ownership_survives_upgrade_without_flags_and_explicit_off_removes_it`
- Owner: `crates/config/src/hooks.rs`

### Moved homes are reported, never acted on

After a home or state-root move, stale entries produce findings without
their paths being inspected or changed; uninstall forgets selected stale
ownership and reinstall leaves dangling links for manual cleanup.
- Why: an old path may now belong to something else.
- Test: `crates/config/src/managed_tests.rs::uninstall_after_home_rename_never_acts_on_old_paths`;
  `::check_after_home_rename_never_inspects_old_config_paths`
- Owner: `crates/config/src/managed.rs`

### LaunchAgents

On macOS with the default root, install owns the peer and GC LaunchAgents.
They join the preview and receipt; publication refuses symlinks at the plist
or any parent and stages with `O_NOFOLLOW | O_EXCL`. The peer agent's PATH
never includes a directory inside a registered project's worktree. A failed
bootout keeps the plist owned, and a historical plist is adopted only on
exact template bytes.
- Why: a LaunchAgent runs unattended at login with the user's authority.
- Test: `crates/config/src/managed_tests.rs::peer_agent_refuses_symlinked_plist_and_parent_before_writes`;
  `crates/fabric-runtime/src/peer_agent.rs::provider_path_skips_a_registered_project_and_clones_nested_in_it`
- Owner: `crates/config/src/managed.rs`, `src/gc_agent.rs`

### Re-apply is the recovery

Each update is atomic and parent-synced. A partial install lists applied and
remaining actions, and re-running the same `install` recovers it, even with
no receipt published. There are no rollback journals, saved binaries or
displaced user files, and ENOSPC or uncertain publication is a typed
failure.
- Why: one idempotent path is easier to trust than a rollback engine.
- Test: `crates/config/src/managed_tests.rs::crash_after_any_publication_reapplies_without_a_receipt`;
  `::receipt_publish_failure_reports_partial_actions_and_reapplies`
- Owner: `crates/config/src/managed.rs`

## Not provided

- Root pointers, seeded defaults and old-root fallbacks.
- `config reset`, project overlays and per-knob environment variables.
- A telemetry ledger or import of old state.
