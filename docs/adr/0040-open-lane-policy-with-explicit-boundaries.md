# ADR 0040: Ambient lane access with explicit mandatory boundaries

**Status:** Accepted

Changed: 2026-11-08.

## Context

This personal harness should give an agent the access it would have when called
from the CLI, including children launched through the host. Restrictions outside
[ADR 0035's four boundaries](0035-four-hard-boundaries-and-flexible-everything-else.md)
should warn or be host-configurable. [ADR 0039](0039-graph-of-templates.md)
requires the same launch policy for children and direct launches.

The current macOS renderer starts with `(deny default)`. The launch edge builds
empty secret and protected lists in
`tools/front-door/crates/cli/src/run/provider/confinement.rs#confinement_request`.
Changing the default without first specifying explicit denials would expose
credentials and host state and make read-only sources writable.

## Decision

Use the existing macOS renderer with `(allow default)` and explicit denials.
There is one renderer and no raw SBPL configuration, compatibility reader or
alternate profile mode. Ordinary filesystem, process and network access is
ambient unless a mandatory boundary, explicit lane request, adapter offline
requirement or optional host policy restricts it.

The policy contract lives in
`tools/front-door/crates/confinement/src/permissions/policy.rs#LanePolicy` and
`tools/front-door/crates/confinement/src/run_config.rs#RunConfig::policy_for_launch`.
The launch edge consumes
this contract at `confinement_request`, derives complete mandatory roots from the
verified launch context, reports once at launch and enforces them with the ambient
default. An empty input list must never mean that mandatory protection is
absent. Resolution is data, not proof of enforcement.

### Optional host policy

The sole host override is `[domains.run.host_policy]` in the selected state
root's `config.toml`. It has no project, lane or child override layer:

```toml
[domains.run.host_policy]
deny_read = ["/absolute/private-directory"]
deny_write = ["/absolute/archive"]
network = "local"
```

Absent policy resolves to no optional read/write denials and `network = "open"`,
with source `ambient`. An explicit table, even an empty one or one equal to the
default, reports the exact host config path and section. The launch owner calls
`LanePolicy::report_launch` once after resolution; rendering, explain and retries
of internal preparation do not duplicate that attempt's report. Each new attempt
gets its own report. Explain may inspect the typed source without emitting it.

Read/write entries name absolute, normalised paths of at most 4096 bytes, with
no NUL or unrepresentable characters. They use subtree matching, including the
named path itself; an absent path still receives a denial. No globs, environment
expansion, home expansion, executable policy, SBPL or allow exceptions are
accepted. Broad denials such as `/Users` are valid because they grant no access.
The filesystem root `/` is omitted with a path warning, matching the shared path
validator. `deny_read` denies content reads and execution from that root;
`deny_write` denies mutations. Optional denials take precedence over all ordinary
grants and permitted mandatory exceptions. Choosing a host restriction can make
a provider unusable; that is an enforcing denial, not malformed configuration.

Network values are `open`, `local` and `provider`, using the existing
`NetworkAccess` meanings. The effective network is the narrower of the host
ceiling and explicit lane request. Adapter offline requirements still deny all
network, including DNS; a host setting cannot widen them. Provider endpoints,
localhost and the exact attempt relay socket retain their typed meanings.
With an open host and no lane restriction, ordinary network access is open.

Invalid entries warn individually and are omitted, preserving valid siblings.
A malformed network value falls back to open optional policy. A malformed table
warns and resolves to open optional policy. Unknown fields warn and have no
effect, including attempted changes to mandatory roots or supplied SBPL. Warnings
name a stable field and code and do not echo rejected values or unknown key
names. Invalid TOML, unreadable config and malformed authority controls keep
their existing config-reader behaviour; tolerance applies to this optional
policy, not to authority gates. Typed editor metadata lives with `RunConfig`.

### Mandatory root inventory

Let `H` be the canonical host home and `S` the selected canonical state root
(default `H/.local/share/provenant/v1`, otherwise the resolved `--state-dir` or
`PROVENANT_STATE_DIR`). Paths below are literal relative suffixes, not globs.
They are denied even when absent when the profile is compiled. Host discovery
must supply canonical aliases as well as the public spelling where aliases
exist; a symlink or ancestor rename must not bypass the boundary.

The mandatory credential/preservation subtree roots under `H` are:

- `.ssh`, `.aws`, `.azure`, `.gnupg`, `.kube`, `.secrets`, `Monero`;
- `.codex`, `.claude`, `.cursor`, `.gemini`;
- `.config/gh`, `.config/gcloud`, `.config/cursor`, `.config/opencode`;
- `.local/share/opencode`;
- `Library/Keychains`, `Library/Application Support/GitHub CLI`;
- `Archives/git-preservation`.

The mandatory exact credential files under `H` are `.cargo/credentials`,
`.cargo/credentials.toml` and `.claude.json`. The last can contain MCP secrets
and is deliberately excluded by the current Claude profile; allow-default
must preserve that protection. These paths and scopes are compiled by
`permissions/policy.rs#MandatoryRoots::new` alongside the existing credential
registries. The renderer slice replaces the old `render.rs#hard_denied`
inventory with this shared owner. The compiler must deny content reads, mutation
and execution of credential roots, with only the exceptions below. Preservation data stays
immutable and hidden under the same rule.

The host also adds every registered provider identity directory, each isolated
Agy home (including each actual `H/.agy-*` directory name), and relocated
credential stores resolved from configured Codex/Claude homes and XDG roots.
This includes unselected accounts. The absolute host `SSH_AUTH_SOCK` is an exact
mandatory credential/socket root, including its canonical alias; there is no
lane SSH-agent exception. The host supplies resolved directories and exact
credential endpoints through the shared `MandatoryRoots` inventory; no wildcard
or credential-content read is required to inventory their names. Selected credential files outside the
standard roots must be added too. The registry and provider setup own those
concrete paths; optional host settings cannot remove them. The host must retain
its credential discovery/selection checks, including keychain-service checks,
rather than trusting a lane's path strings.

`S` is a mandatory secret and immutable subtree. This covers, without separate
removable entries, `S/config.toml`, `S/credential-authority.json`,
`S/availability.json`, `S/run.sqlite3`, `S/runs`, `S/graphs`, `S/projects`,
`S/worktrees`, `S/fabric`, `S/workflow`, `S/quota`, `S/install` and `S/releases`.
The rule includes SQLite `-wal`, `-shm`, `-journal` and lock siblings. Overrides
cannot grant an extra root inside `S` or an ancestor enclosing it.

Host Git roots are the canonical common Git directory and the linked worktree's
Git directory, supplied with `MandatoryRoots::add_git`. The source checkout's
`.git` file/directory is immutable as well. Git authority always comes from
verified handles, selected branch and the Git-grant digest; optional settings
cannot replace it. Extra write roots overlapping the chosen source/worktree,
host Git, state or credentials in either direction remain refused, for children
and direct launches alike. Component-prefix siblings such as `source-other`
are not overlaps. `MandatoryRoots::validate_extra_write` owns the shared
contract; submission and compile edges must still canonicalise and revalidate.

### Permitted exceptions

Exceptions are host-derived typed grants, never optional configuration:

1. **Selected authentication.** Only the selected, fixture-backed authentication
   mechanism may read its exact declared credential paths. In-place refresh
   writes require the existing `AuthenticationProfile::in_place_writes` subset
   check and grant only that mechanism's exact files; they confer no create,
   unlink or rename authority over the credential directory. Codex's selected
   `auth.json`, Cursor's declared login store and Agy's selected isolated
   `Library/Keychains/login.keychain-db` follow their provider profile. Claude's
   host credential acquisition and private environment delivery remain
   host-owned; they do not grant general keychain access. Unselected identities,
   real-home keychains and unrelated services remain inaccessible. Keychain
   IPC and `/usr/bin/security` must be explicitly denied unless a verified
   selected mechanism requires the precise service. Provider setup is not
   permission to expose an entire home. Credentials never cross hosts.
   OpenCode currently reads its host configuration/account store through runtime
   grants without a selected mechanism. Before flipping, declare its selected
   host config root (`XDG_CONFIG_HOME/opencode`, default `H/.config/opencode`)
   and exact account file (`XDG_DATA_HOME/opencode/auth.json`, default
   `H/.local/share/opencode/auth.json`) as a fixture-backed authentication
   mechanism with read-only grants. Other adapters receive neither root.
2. **GitHub CLI access.** A lane with effective open network may read
   `H/.config/gh` and `H/Library/Application Support/GitHub CLI` (and
   host-resolved locations), read-only lanes included: the owner wants lanes to
   read issues and pull requests through `gh`, and the steward template depends
   on it. These stores stay immutable. The token this exposes can also write to
   GitHub, so a read-only lane is read-only on this host, not on GitHub; the owner
   accepts that. A profile's explicit credential denial still wins, and a lane
   without open network gets no access.
3. **Attempt-private state.** Grant read/write only to this attempt's disposable
   scratch and retained provider state, normally
   `S/runs/<run>/attempts/<attempt>/tmp/candidate-<n>` and
   `S/runs/<run>/attempts/<attempt>/provider-state/candidate-<n>`, using the actual host-derived
   `LaunchPaths` bindings. Read only the selected continuation inputs. Do not
   open the whole attempt directory, another attempt or the runs store.
   Host-prepared prompt files and sealed attachments/inputs receive their
   exact declared read grants. The host process-tree snapshot and provisioned
   build-slot directory/lockfiles receive exact read-only coordination grants;
   locking existing slot files does not grant content writes or open their
   state-root siblings. Host-captured user skill snapshots, normally
   `S/runs/<run>/attempts/<attempt>/tmp/candidate-<n>-skills-<nonce>`, may be
   read/executed and stay immutable; their original host credential/config
   parents are not opened. The exact attempt-bound Fabric socket remains
   usable without exposing Fabric databases. Host indexes, target seeds,
   credential authority and config stay inaccessible and immutable. Scratch
   remains private to the owner; ancestor mutation, hard links and path swaps
   must not expand it.
   Before flipping, relocate Cursor's current `H/.cursor/chats` writes and
   OpenCode's host `XDG_DATA_HOME/opencode` writable stores (`opencode.db`,
   `opencode.db-shm`, `opencode.db-wal`, `log`, `storage`, `snapshot`, `shell`)
   into this attempt's private provider-state root. Rebind their session
   continuation and model-observation paths there too. Claude's read-only
   transcript writes currently target the host config's `projects/<project-key>`
   tree: move those into private `claude-config/projects/<project-key>` as well,
   setting `CLAUDE_CONFIG_DIR` independently of user-skill discovery. Do not exempt these
   shared host stores from their mandatory parent denial: Cursor transcripts
   and OpenCode's mutable database are state, not authentication. OpenCode's
   server requires writable `shell` state; denying it produces HTTP 500 rather
   than a usable run. This is a renderer prerequisite, not a migration shim or
   a change implemented in this policy slice.
4. **Read-only installation and packages.** Verified immutable
   `S/releases/<source-digest>/...` assets may be read/executed; no write to the
   release, releases parent or install receipt is granted. Preserve the
   existing read/exec-only Codex standalone package exception at
   `H/.codex/packages/standalone/releases/<release>/...` where its existing
   validator accepts it. No sibling credential/config/session root is opened.
5. **Source and Git.** The source checkout is readable. Read-only attempts deny
   every source mutation explicitly. Existing host-owned `ReadOnlyPolicy`
   exceptions apply only to their declared paths in the ordinary read-only
   view; live sources allow none, and isolated source views carry none into
   the live checkout. Provider state/tool caches may not silently override
   source protection. A writer may mutate its verified worktree except `.git`,
   and owns a complete lane-private Git directory in attempt scratch.
   `GIT_DIR` points there; `GIT_WORK_TREE` points at the bound worktree. The
   host common/linked Git dirs, shared refs, config, hooks, info and indexes
   stay immutable. Only the verified host object database is a read-only
   alternate; no lane write reaches shared objects, refs or delivery state.
   Preserve single-link control checks, held identities, selected HEAD/ref
   validation, writer custody, object digest/fsck checks and host delivery.

Mandatory baseline denials precede these narrowly scoped exceptions; final
non-exempt mandatory denials and explicit profile/host denials win over every
ordinary grant. The renderer must encode exceptions by excluding only those
verified paths/operations from a parent denial, without reopening siblings.
It must explicitly forbid rename/unlink of protected ancestors and hard-link
creation across boundaries. Ordering alone is not evidence: native tests must
prove the resulting behaviour with an allow-default profile.

### Backend and renderer acceptance

Linux has no enforcing backend. It stays refused with the existing typed
backend-unavailable result; implementing Linux enforcement is out of scope.
No fallback to unconfined execution is permitted.

The renderer slice must pass these checks before flipping the default:

- The four `run_config::policy_tests` acceptance tests; resolution and reporting
  through a real launch once per attempt, for both direct and child launches.
- Ambient files, tools, subprocesses and open network work without catalogue
  grants. Host read/exec and write denials work; mixed invalid settings warn
  without discarding valid restrictions. All nine network ceiling/request
  pairs and adapter-offline denial retain their effective meaning.
- Every mandatory credential root (including absent paths created after
  compilation), relocated/unselected identities, real keychains and state
  roots remains unreadable/immutable with open optional policy, broad ordinary
  grants and attempted optional overrides. Selected authentication refresh,
  GitHub CLI, prompt/inputs, relay, immutable release and standalone package
  exceptions work without exposing siblings. Open-network lanes, read-only ones
  included, can run `gh`; local-network lanes and explicit denials fail
  GitHub-store reads.
  Cursor, OpenCode and read-only Claude must complete ordinary runs and resume using relocated
  attempt-private stores, with model observation following the private database;
  shared host state remains immutable. OpenCode's typed selected-auth store
  reads work while other adapters cannot read them. `.claude.json` remains
  hidden from all adapters, including Claude itself. User skills on/off must
  preserve private state binding and immutable staged snapshot access.
- Read-only source mutations fail through shell tools, provider state/cache
  grants, symlink aliases and ancestor operations. Live/isolated source and
  ordinary read-only write-exception semantics remain distinct. Writers can
  commit in private Git; shared refs/objects/config/hooks/info/indexes and the
  `.git` marker stay immutable. Overlap checks reject both ancestor and child
  roots while allowing component-prefix siblings.
- Existing focused render/grant/validation tests and provider profile snapshots
  pass after deliberate expectation changes for ambient authority. In
  particular, retain `writer_programs_keep_filesystem_and_credential_boundaries`,
  `expanded_authentication_family_grants_only_selected_credentials`,
  `worktree_access_read_only_profile_limits_checkout_writes`,
  `network_levels_shared_writes_and_hard_denies_render_in_order`,
  `credential_writes_stay_inside_auth_roots_and_state_stays_outside` and
  `lane_root_table_agrees_with_compiler_checks` in the confinement tests.
- On a configured macOS host/CI, run
  `permissions::tests::escape::native_sandbox_blocks_path_escape_probes` and
  extend it for every denial/exception above, including rename, hard link,
  symlink/root swap, same-UID process access and keychain IPC. Lanes that cannot
  run Seatbelt report `ProbeFailed`/skip; that is not enforcement evidence.
  Preserve `linux_has_no_confinement_backend` on Linux CI.

## Known limits

Boundaries are enforced by pathname. A hard link to a protected file that already
exists outside granted roots, such as an object in an earlier `git clone --local`,
is not covered. New hard links remain denied. The owner accepts this limit;
launches are not refused merely because a file has multiple links.

Credential directory symlinks are inventoried without following directory
links, up to a depth and entry limit per root. Hitting a limit warns and leaves
the remaining links uninventoried. Links whose targets cover the lane source or
scratch are skipped with a warning. Links introduced after inventory, or links
that cannot be resolved or represented during the walk, cannot be covered by
this walk. Existing declared endpoint validation still rejects unresolved
dangling endpoint links.

## Consequences

Optional host policy is observable and tolerant, while the boundaries remain
host-owned. More ambient capability means the renderer can no longer rely on
omitting allows: every confidentiality and integrity requirement needs an
explicit denial and a tested exception. This contract deliberately precedes the
renderer change so incomplete inventories cannot silently weaken enforcement.
