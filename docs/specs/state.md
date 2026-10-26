# State and config

State publishes the frozen paths, identities, record codecs, configuration
and managed-installation contract that the other area specs cite. The
[cross-cutting decisions](README.md#cross-cutting-decisions) govern it, the
[Rust workspace rules](rust-workspace.md) bound its crates, and
[CLI](cli.md) owns command rendering and exit codes.

## Purpose and ownership

Three crates share the area:

- `provenant-core` supplies identities, paths and explicit time and
  cancellation values. It has no filesystem, environment, clock, entropy or
  process access.
- `provenant-records` supplies bounded codecs, canonical bytes, digests and
  shared transport models.
- `provenant-config` loads typed configuration, binds projects and hosts, and
  installs the binary, assets and harness entries.

Binary composition connects them to supervisor, confinement, run and Fabric.
Config has no dependency on those runtimes.

## Boundaries

| State owns | Delegated to |
| --- | --- |
| Root selection, path layout, host and project identity | Supervisor: safe no-follow filesystem, locks, publication, entropy and the production clock |
| Record header rules, canonical bytes, digests, bounded JSON, JSONC and YAML readers | Each domain owner: record bodies, retention, which fields enter a digest |
| Typed `config.toml`, compiled profiles, `config show` and `check config` | Domain owners: policy types, defaults and semantic validation |
| Release identity, `install`, `check install`, `uninstall`, harness registrations | CLI: the `HookSpec` table and accepted MCP startup arguments |
| `bind_project` | Confinement: Git discovery. Fabric-runtime: the cross-host project key |
| Run's `AttemptFact` model (in records) | Run: constructs it from its store. Telemetry: reduces it. CLI composes the two |

State owns no run registry, no telemetry ledger and no Fabric import. Routing
owns account eligibility, and run records the resolved account label.

## Contract

### Paths and root selection

| Input or path | Contract |
| --- | --- |
| Default root | `~/.local/share/provenant/v1`, one root per host |
| Override | Global `--state-dir ABS`, then `PROVENANT_STATE_DIR`, then the default. Empty or relative explicit values fail. |
| Environment | State owns `PROVENANT_STATE_DIR` and `PROVENANT_AGENT_LABEL` (separate inboxes for agents of one provider). Provider and harness variables pass through under their own contracts. No other `PROVENANT_*` inputs. |
| Uninstalled binary | Requires an explicit absolute `PROVENANT_ROOT` for assets. Installed binaries use their release location and ignore it. |
| User config | `<state>/config.toml`; optional, user-owned |
| Host identity | `<state>/host-id` |
| Install receipt and lock | `<state>/install/receipt.json`, `<state>/install/install.lock` |
| Immutable release | `<state>/releases/<SourceDigest>/` holding `bin/provenant`, `release.json` and the bundled asset directories (`skills`, `library`, `config`, `instructions`, `scripts`) |
| Stable command | `<bin-dir>/provenant`; default bin directory `~/.local/bin` |
| Project binding | `<state>/projects/<ProjectId>/project.json`; host-local identity only |
| Read-only write exceptions | `<state>/projects/<ProjectId>/read-only.json`; confinement-owned `readOnlyWritable` list, default empty. Not a config overlay. |
| Runs | `<state>/runs/<RunId>/` and run's top-level registry store |
| Worktree custody | `<state>/worktrees/<ProjectId>/`; supervisor and confinement own locks, leases and reservations |
| Fabric | `<state>/fabric/`; fabric-store owns its database |
| Other owners | `<state>/workflow/projects/<ProjectId>/{deliveries,sessions}`, `<state>/quota/` |
| Usage counters | `<state>/usage.jsonl`, `usage.jsonl.1` and `usage.lock`; appended only when the root already exists |
| Scratch | `<state>/tmp/`; operation-scoped, never a whole-root purge |

There is no telemetry ledger subtree. `telemetry stats` reduces run's terminal
records using the bundled TOML price asset.

Capture home and environment once. Reject roots equal to home or the
filesystem root, equal to or beneath the product checkout, or equal to or
beneath a pre-1.0 root (`~/.agents`, `~/.local/state/agent-harness`,
`~/.local/state/provenant`, a workspace's `.agent-run`) or any `.agent-run`
path component. Resolve existing ancestor aliases and append only a normal
missing tail, because lexical checks cannot protect filesystem access. Do not
open old-root contents to test containment.

Reads, help and `root` create no state, though they still append to an
existing root's usage log. Mutations initialise explicitly with directories
0700, files 0600 and executables 0755. Config never opens SQLite. Each owner
uses supervisor's locks and publication; no whole-root sweeper exists.

### Identities and time

IDs are distinct newtypes over 16 supplied random bytes, shown as 32 lowercase
hex digits. State initialisation persists `HostId` once under lock.
`ProjectId` is host-local, allocated per canonical Git common directory.
Linked worktrees share it but have separate `WorkspaceId` bindings and writer
leases. Non-Git workspaces bind their canonical directory. It is never a
federation key; Fabric-runtime owns `bind_federated_project` and the
cross-host key, and equal remote URLs never merge identities silently.
Rebinding needs an explicit project operation.

`ActorId` names an attributed actor, not authority or a route. `WorkScope` is
a closed enum over project, workspace and run, and domain boundaries validate
membership. `CancelToken` is an in-memory cooperative flag that is never
serialised and never a provider signal.

`WallTime` is UTC integer microseconds with one RFC 3339 `Z` rendering.
Monotonic values and durations use checked integer arithmetic and are never
persisted as cross-process deadlines. Core owns the `Clock` trait; supervisor
owns the production clock; tests inject a fake. Supervisor's state machine
receives sampled values and never reads the clock.

`GitOid` accepts only complete 40 or 64 digit hexadecimal object names and
emits lowercase. It is distinct from Provenant's SHA-256 `Digest`.

Paths stay `PathBuf` or `OsString`. Durable Unix paths use tagged `PathWire`
(UTF-8 or lowercase hex bytes), never lossy conversion. `RelativePath`
rejects absolute components, `.` and `..`.

### Records and codecs

Stored records are typed serde models with `schema_version`, closed enums and
`deny_unknown_fields`. Unknown major versions fail before mutation. Facts
built only by trusted edge code (workflow validation facts, run's
`AttemptFact`) derive `Serialize` only. Decode a separate stored record and
validate it before constructing a fact, so deserialisation cannot manufacture
evidence.

The records reader rejects duplicate object members, invalid UTF-8, trailing
input, non-finite numbers and integers outside i64 and u64 before typed
deserialisation. Default limits: 1 MiB per config or metadata file, 8 MiB per
general JSON record, nesting depth 64 and 100,000 collection members. Callers
may request a smaller or an explicit larger bound. Bytes are bounded while
reading. Errors carry code, path and location, never the rejected value.

`CanonicalV1` defines digest-bearing bytes: UTF-8 JSON with no whitespace,
keys sorted recursively by UTF-8 bytes, array order preserved, minimal string
escapes, no Unicode normalisation, shortest decimal integers, and no floats
(owners use integer units or decimal strings). It is a Provenant contract, not
RFC 8785. `record_digest(kind, version, bytes)` hashes a fixed domain prefix,
kind, version and canonical bytes, and renders `sha256:<64 hex>`. File digests
hash raw bytes under a separate `FileDigest` type. Domain owners choose which
fields enter each digest.

The YAML reader is a bounded subset for Agent Skills frontmatter and
`agents/*.yaml` sidecars (ADR 0028). It returns typed maps, sequences, strings
and booleans with line and column spans. It refuses anchors, aliases, tags,
flow maps, multiple documents, tabs, duplicate keys and multiline plain
scalars, naming the construct and line. Plain scalars resolve only `true` and
`false`; ambiguous words need quoting. Limits: 256 KiB, depth 32, 10,000
nodes. The workflow skill validator owns allowed metadata keys.

### Configuration

One strict typed `<state>/config.toml` declares `schema_version = 1`. Unknown
keys, duplicates, bad types or ranges and unsupported versions fail. Absent
means compiled defaults, and unreadable or malformed means error. There is no
include, interpolation, executable configuration or automatic migration.

The optional `profile = "balanced" | "economy" | "rigorous"` selects a
compiled profile; omission selects `balanced`. Domain crates export a
`DomainSection` entry and their own per-profile TOML assets, which config
nests under `[domains.<name>]`. Config holds no domain policy. Domain owners
validate values and enforce their minima, and a preset never grants authority.
The config crate loads user sections over the selected defaults once, with no
profile layering, project overlay or per-request selector.

Every allowed key appears in the section's complete default asset. Tables
merge recursively and arrays replace as a whole. Knobs resolve through the
embedded registry: default, profile preset, then the user's `[knobs]`
override. `config set` edits `profile` or a registry knob (a user decision,
#1118) and `config unset` removes a knob override. There is no
`config reset`, profile listing, project `.provenant.toml`, `PROVENANT_PROFILE`
or per-knob environment variable.

`check config` collects unknown-key, type, range and semantic findings,
ordered by source line, column, dotted path and rule, with a limit of 1 to
100, a total count and a truncation flag. CLI paginates and maps findings to
exit 1. Malformed TOML is a distinct syntax error mapped to exit 2. Diagnostics
keep one-based line and column, the dotted key and an origin; an invalid
compiled asset is attributed to the compiled origin, never the user's file.
`Redacted<T>` serialises as `[redacted]` for secret-bearing leaves.

The catalogue, adapter table and prices ship as TOML release assets and are
never copied into mutable state. Routing, providers, quota and workflow
validate their semantics, and telemetry owns the price schema. `config show`
returns effective non-secret values with their origins. Neither command
mutates state or contacts a provider, and there are no config or install MCP
tools. Processes use an immutable startup snapshot.

## Commands and installation

CLI supplies the JSON envelope, the 1 MiB result cap and exit mapping. Install
success or no-op is `ok`, drift is findings, a foreign selector is refused,
malformed input is invalid, and I/O or lock contention means cannot proceed.
After a partial write, `data` lists completed and remaining actions and a
re-apply instruction. Harness changes are printed before they are applied
(stderr preview in JSON mode), and a failed preview stream prevents writes.

| Command | Behaviour |
| --- | --- |
| `root` | Prints the absolute release root as one line, so `$(provenant root)/skills/...` works. No Git, SQLite, state creation or provider probes. |
| `config show\|set\|unset`, `check config` | Effective configuration and origins, knob edits, and validation findings. |
| `install [--dry-run]` | Requires `--source ABS` (a staged release) and one or more `--harness NAME` (`claude`, `codex`, `cursor`, `agy`, `opencode`). Optional `--bin-dir`, `--hooks on\|off` (omission preserves hook ownership; disabled on first install), `--adopt-existing`, `--legacy-checkout ABS`. Validates every destination and prints every replace, remove and create before writing. Dry-run creates no root, lock or receipt. |
| `check install` | Optional `--source ABS`. Compares the installed binary, assets, receipt and owned harness selectors without repair. |
| `uninstall` | Explicit `--harness NAME`. Removes unchanged owned entries only and keeps state, release assets, user skills, config and the running binary. |

### Release identity and bootstrap

`cargo build --release --locked` in `tools/front-door` runs a workspace
`build.rs` that computes a source digest with records' pure digest module. It
covers workspace and member manifests, the lockfile, toolchain files, every
member's source and build scripts, and the bundled assets. Names are sorted
workspace-relative POSIX paths, framed by lengths, under the prefix
`provenant-source-v1`. Duplicate, missing, symlinked or escaping inputs are
rejected, and there is no digest environment override. Release builds trim
source paths so checkout paths do not affect binary bytes.

Packaging emits a typed `release.json` beside `bin/provenant` with the source
digest, target triple, toolchain and a bounded file inventory with digests.
`--version --json` reports the embedded digest and record and config
versions. Executable assets keep mode 0700 and other assets 0600.

An installed binary resolves its own location to the release root holding
`release.json`, and `provenant root` prints it. A digest proves identity, not
publisher trust. Install verifies the manifest, platform, file bytes and
modes, then runs bounded version and help smoke checks through supervisor
(30 s, capped output, no provider access).

Install copies verified files into the immutable release directory, then
atomically publishes the stable binary link and per-entry skill and
instruction links. Re-application accepts exact desired bytes and links as
no-ops, including writes completed before a crash. `release.json` is published
after all other release entries, and afterwards the release is immutable. Equal
source digests with different manifest bytes fail with a typed
`ReleaseConflict` naming both digests. Upgrades remove unchanged owned entries
a target no longer ships and leave changed or unmanaged entries. The only lock
is `<state>/install/install.lock`.

The receipt records the release identity, resolved targets, selectors,
expected owned values and, when given, the `legacy_checkout` root and compiled
manifest digest. It is published last. A target is a harness plus its resolved
skills directory, so one receipt may hold several targets of a harness (for
example one per `CLAUDE_CONFIG_DIR`). Targets never share a destination.
Install replaces only the selected targets and carries the rest over, and
`check install` verifies every recorded target. Uninstall selects the target
matching the environment's home, or else every recorded target of the harness.
No cross-file or cross-host atomicity is claimed.

After a home or state-root move, the recorded binary link identifies the old
state root. Old-home candidates come from the current bin-directory tail, the
recorded default bin directory, the moved default state root, recorded harness
paths compared with defaults, then the legacy home. The first candidate that
neither contains nor sits within the current home identifies the old home,
independently of the current bin setting or state layout. Legacy staleness
depends only on its recorded home. Stale entries produce `relocated_stale_entry`
findings without inspecting or changing their paths; a moved state root also
produces `state_root_moved`. These findings remain visible when no target is
selected. Uninstall forgets selected stale ownership and retains unselected
entries and the binary record. A stale `git-hook:pre-push` entry and its legacy
record also remain in the receipt after a full uninstall; its old path stays
untouched. Selectors and Git hooks outside the old home retain their existing
behaviour. Reinstall drops stale targets and an old-home legacy record from
the fresh receipt. A Git hook outside the old home may still be refreshed, but
its ownership is omitted when that legacy record is dropped. Reinstall
requires manual cleanup of dangling links; see the
[install runbook](../runbooks/install.md).

Omitting `--hooks` on re-install retains and refreshes the selected target's
unchanged owned hook registrations and the shared legacy Git pre-push launcher.
Edited hooks and launchers are preserved, relinquished from the receipt and
reported. The receipt's legacy record names each relinquished Git hook in
`relinquished_hooks`, so `check install` stops auditing that hook while other
legacy hooks in the same directory stay audited. Missing launchers also leave
the receipt. The legacy checkout flag
need not be repeated. Explicit `--hooks on` refuses edited Claude hooks and
names `--hooks off` as recovery. Explicit `--hooks off` removes exact owned
matches, relinquishes modified entries and reports them.

On macOS, installation into the account's default state root also owns
`~/Library/LaunchAgents/dev.provenant.peer.plist`. The plist participates in
the preview, receipt and `check install`. Publication refuses symlinks at
the plist or any parent, stages with `O_NOFOLLOW | O_EXCL`, then renames.
Its PATH starts with `/usr/bin:/bin:/usr/sbin:/sbin`, followed by the absolute
install bin directory and existing absolute directories holding resolved
Claude, Codex and Git executables, when known. Empty and relative entries are
excluded. Uninstall of the last recorded target boots out the service before
removing an exact receipt match. A modified plist and its service are kept
and reported; failed bootout leaves the plist owned for retry.
The earlier installer's unreceipted plist is recognised only when its complete
bytes match the historical template for the validated receipt's binary path
and state root, with mode 0600 or 0644. Upgrade replaces it with the sanitised
rendering and records ownership; uninstall can remove it directly. Unknown
or modified historical plists are preserved.

### Harness registry

Defaults derive from each harness home. Typed user-only
`[install.targets.<harness>]` may override absolute home and config paths, and
`[install]` may set `bin_dir`. Precedence is explicit flag, then config, then
`CLAUDE_CONFIG_DIR` or `CODEX_HOME`, then defaults. Paths are resolved once
and shown in the preview and receipt; a project or request cannot change them.

| Harness | MCP file and owned selector | Hooks | Skills directory | Instruction path |
| --- | --- | --- | --- | --- |
| Claude Code | `~/.claude.json`: `mcpServers.fabric` | `~/.claude/settings.json`: owned command leaves in `hooks.<event>[]` | `~/.claude/skills/` | `~/.claude/CLAUDE.md`, companion `HARNESS.md` |
| Codex | `~/.codex/config.toml`: `[mcp_servers.fabric]` | `[features] hooks = true` plus `~/.codex/hooks.json` | `~/.codex/skills/` | `~/.codex/AGENTS.md`, companion `HARNESS.md` |
| Cursor | `~/.cursor/mcp.json`: `mcpServers.fabric` | none supported | `~/.cursor/skills/` | none supported |
| Agy | `~/.gemini/config/mcp_config.json`: `mcpServers.fabric` | none supported | `~/.gemini/skills/` | `~/.gemini/GEMINI.md`, companion `HARNESS.md` |
| OpenCode | `~/.config/opencode/opencode.jsonc`: `mcp.fabric` (`type="local"`) | none supported | `~/.config/opencode/skills/` | `~/.config/opencode/AGENTS.md`, companion `HARNESS.md` |

Every MCP entry executes the absolute stable binary with
`--state-dir ABS fabric mcp --client NAME`, with no checkout, interpreter or
secret environment. CLI owns the `HookSpec` table (harness, protocol event,
matcher, timeout, handler) that the installer reads, and binary composition
passes it to config. Unsupported hooks refuse before writes, and no hook is
registered without a fixture and policy. Removing Codex hooks removes only an
unchanged installer-owned feature value and preserves a user's own
`hooks = true`.

Editors preserve unrelated bytes. TOML uses `toml_edit` document mutation.
JSON and JSONC use records' bounded span parser to splice owned leaves and
reparse, keeping comments, trailing commas and array order. Malformed,
duplicate or ambiguous containers are rejected unchanged, and the full user
config is never serialised or logged. Config files must be regular; a
symlinked config refuses with its path. `~/.claude.json` has a 16 MiB bound
and other configs 1 MiB. Quit Claude Code before installing, because it
rewrites its file while running. Identity and bytes are rechecked immediately
before publication. A harness write between that recheck and publication is
lost, so quit the harness before installing. Links are inspected with
`readlink` and never opened through the old target.

`--adopt-existing` covers only an occupied destination selector or the stable
regular executable, pinning its identity and digest. It never authorises
deletion, refuses symlinked executables and foreign directories, and refuses
inline secret environment values so neither preview nor receipt becomes a
credential store. Existing regular user instructions are preserved, and if no
owned link can be installed the preview reports a manual include.

## External contracts

- Harness configuration files and their MCP and hook shapes are owned by each
  client. Sanitised real-client captures live in
  `tools/front-door/tests/fixtures/contracts/`, with client and version,
  platform, capture method and redactions. They use isolated homes and fake
  MCP executables.
- Agent Skills frontmatter and `agents/*.yaml` are external inputs that the
  YAML subset reads (ADR 0028).
- Git object formats (SHA-1 and SHA-256) arrive through confinement's
  discovery. State does no Git work.

### Migrating from a pre-1.0 install

`--legacy-checkout ABS` authorises matching only the finite entries in the
compiled manifest `assets/install/legacy.toml`, at the selected harness paths.
Each entry names the harness, selector, exact command or link template and a
`replace` or `remove` disposition. Only captured home and the explicit
checkout substitute into templates. Ownership is never inferred from a
basename, substring, shell parsing, a prefix or glob, or an old receipt, and
nothing under the checkout or `~/.agents` is scanned. Unsupported commands are
conflicts, not guessed matches.

The manifest covers the old stable command (managed Mach-O or ELF form only),
Git hook links, the four Claude hook commands, skill links in all five skills
directories (including a whole-directory link, which is unlinked and replaced
without traversal), the three JavaScript workflow links, instruction and
`HARNESS.md` links, the OpenCode `instructions[]` strings and old MCP
entries. Install prints each matched selector, disposition and replacement
before writing. An unresolved entry is preserved, reported in the outcome and
raised as a `check install` finding. Instruction files linked to
`~/.agents/AGENTS.md` have `AGENT_FABRIC_LABEL` rewritten to
`PROVENANT_AGENT_LABEL`, printed first, as the one authorised edit under an
old root.

## Failure and recovery

Locks cover the install root and selected destination parents in stable
order, and all proposals are validated before writing. Identity and content
are rechecked before each atomic publication, and on drift install stops and
reports the selector. The locks serialise cooperating installers; an
uncooperative editor racing the final recheck is not excluded, so quiesce
harness clients and config editors before installing. No compare-and-swap or
exchange semantics are claimed.

Each file or link update is atomic and parent-fsynced. On failure, the outcome
lists applied and remaining actions, and re-running the same `install` after
resolving conflicts is the recovery path. Exact-desired matching makes it
idempotent even when no receipt was published. There are no automatic
inverses, saved binaries or displaced user files. Removal compares current
values with the receipt and unlinks only unchanged owned entries, never
recursively. ENOSPC, broken output and uncertain publication are typed
failures, never silent success. To go back to a pre-1.0 install, reinstall it
from its own checkout.

## Deliberate cuts

| Cut | Reason |
| --- | --- |
| Fused or split roots, root pointers, seeded defaults, three-way refresh | Fresh state and immutable release assets remove copied-catalogue reconciliation. No `AGENTS_HOME` or `AGENT_FABRIC_*` root fallback and no stale-checkout probe. |
| `config reset`, profile listing, project overlays, task-class tables, per-knob environment variables | Never used in the usage audit. Domain policy still enforces authority, confinement and review minima. |
| Generic installer transactions (bundle, plan, apply, recover, rollback journal, binary backup) | Re-applying `install` is the recovery path. |
| Kiro registration | No observed use. |
| `_shared`, JavaScript workflows, old link layouts | Retained skills are replaced individually; exact retired links are removed by the legacy manifest. Custom skill contents and old receipts are not read. |
| Runtime provisioning and Codex skill overrides | The product needs no Python, Node or daemon. Another tool's skill preferences are left alone. |
| Telemetry ledger and generic import | Telemetry reduces run records. Nothing imports old state; an old Fabric store is left untouched. |
| Configuration includes, interpolation, automatic migration | Executable or implicit configuration cannot be audited. |

## Verification

- Core and records unit tests (`crates/core`, `crates/records`) cover ID, path,
  version and time overflow, canonical bytes and digest separation,
  integer, duplicate and bound refusal, YAML and JSONC syntax, and
  `Serialize`-only fact construction.
- Config tests (`crates/config/src/tests.rs`, `release_tests.rs`,
  `managed_tests.rs`) cover typed TOML, presets, knob resolution, origins,
  editor byte preservation, first install, no-op, partial failure then
  re-apply, drift, malformed config, adoption, exact legacy removals and
  `uninstall`.
- `tools/front-door/tests/install.rs` and `checks.rs` drive the real commands
  with synthetic release, provider and MCP fixtures, and assert that help and
  `root` create nothing and that unowned spans stay byte-identical.
- Process tests on Linux and macOS use handshake barriers for lock contention,
  deadlines, edits detected before publication, crash at each write and
  ENOSPC, fsync and rename errors. All process access is supervisor-owned,
  with no sleeps. Tests point HOME, state and harness paths at temporary
  roots. Full workspace and platform runs happen in CI.
