# State and config

Design for the workshop chair and Rust build lanes. Author route:
`codex/gpt-6.1-sol@high`. [PORTING v7](rust-workspace.md), the
[rewrite plan](README.md) and the binding
[cross-area decisions](README.md#cross-cutting-decisions) govern this design.
State publishes the frozen paths and flags below; other areas cite this
contract. CLI owns command rendering and exit codes.

## Purpose and feature decisions

`provenant-core` supplies identities, paths and explicit time/cancellation
values. `provenant-records` supplies bounded codecs, canonical bytes, digests
and shared transport models. `provenant-config` loads typed configuration and
installs the binary, assets and harness entries. Core has no filesystem,
environment, clock, entropy or process access.

Source material is the installation/configuration scripts, `scripts/lib/`,
`skills/_shared/`, `config/`, their tests and the inert Rust crates at
`1b3d48e4c549811ce3636edc30b047f36fd24ada`. The usage audit found installation,
root lookup and builds in executable shell-tool positions, but no executed
profile set/use/reset or per-knob environment overrides. No
`harness-profile.json` exists. The old layering machinery is not a product
requirement.

| Keep | Implementation |
| --- | --- |
| One root resolver, host and project identities | Resolve at the binary edge; pass typed values. Persist random host-local IDs once; hostnames are labels. |
| Source identity and managed installation | One release manifest, one receipt, per-entry skill links and conflict refusal. Explicit harness selection; preserve unmanaged files. |
| Five live harness registrations | Claude Code, Codex, Cursor, Agy and OpenCode have active surfaces. Format-specific editors share one planner. |
| Compiled presets and typed TOML | Optional user `profile =`; read-only `config show` and `check config`. Domain owners supply policy types and defaults. |
| External YAML subset | Agent Skills frontmatter and `agents/*.yaml` stay external inputs under ADR 0028. |

| Drop | Reason and consequence |
| --- | --- |
| Fused/split roots, root pointers, seeded defaults and three-way refresh | Fresh state and immutable release assets remove copied catalogue reconciliation. No `AGENTS_HOME`, `AGENT_FABRIC_*` root fallback or per-command stale-checkout probe. |
| Profile mutation, overlays and knob registry | `config set` and `unset` return (user decision, #1118) over the embedded registry and a `[knobs]` table; still no `config reset/profiles`, project `.provenant.toml`, task-class tables, `PROVENANT_PROFILE` or `PROVENANT_KNOB_*`; no strengthen-only re-resolution. Domain policy still enforces authority, confinement and review minima. |
| Generic installer transactions | No bundle/plan/apply/recover commands, plan digest, rollback journal, exchange primitive or binary backup. Re-apply `install` after failure; rollback reinstalls `pre-rust-rewrite`. |
| Kiro registration | No observed use; do not carry another unsupported client. |
| `_shared`, JS workflows and old link layouts | Replace retained skills individually; remove exact retired links at cut-over. Do not migrate custom skill contents or read old receipts. |
| Runtime provisioning, OTel service installation and Codex skill overrides | Product needs no Python/Node/daemon. Telemetry retires the collector; leave another tool's skill preferences alone. |
| Python/V8 emulation, differential oracles and `pending` copies | R0 requires a new design, not runtime-language compatibility. |

CLI owns which hook behaviours survive; config owns their registration and
removal. Confinement and custody requirements do not depend on CLI usage.

## Frozen paths and root selection

| Input or path | Contract |
| --- | --- |
| Default root | `~/.local/share/provenant/v1`, one root per host on macOS and Linux |
| Override | Global `--state-dir ABS`, then `PROVENANT_STATE_DIR`, then default; empty or relative explicit values fail. No XDG/old-root fallback. |
| Environment | State owns the persistent state inputs: `PROVENANT_STATE_DIR` and `PROVENANT_AGENT_LABEL` (separate inboxes for agents of one provider, replacing `AGENT_FABRIC_LABEL`). Provider/harness variables pass through under their external contracts; no other `PROVENANT_*` or `AGENT_FABRIC_*` inputs. |
| Build root | An uninstalled binary requires explicit absolute `PROVENANT_ROOT`; installed binaries use their release location, never this override |
| User config | `<state>/config.toml`; optional, user-owned global configuration |
| Host identity | `<state>/state.json` |
| Install receipt and lock | `<state>/install/receipt.json`, `<state>/install/install.lock` |
| Immutable release | `<state>/releases/<SourceDigest>/{bin/provenant,release.json,skills/,library/,config/,templates/,instructions/,scripts/}` |
| Stable command | `<bin-dir>/provenant`; default bin directory `~/.local/bin` |
| Project binding | `<state>/projects/<ProjectId>/project.json`; host-local identity only |
| Read-only write exceptions | `<state>/projects/<ProjectId>/read-only.json`; confinement-owned, explicit `readOnlyWritable` list (default empty), authorised in #1343. Not a project config overlay and never loaded from old `.agents/fabric-policy.json`. |
| Runs | `<state>/runs/<RunId>/`; run owns its top-level registry store and this subtree |
| Worktree custody | `<state>/worktrees/<ProjectId>/`; supervisor/confinement own repository locks, writer leases and reservations |
| Fabric | `<state>/fabric/`; fabric-store owns its database and companions |
| Other owners | `<state>/projects/<ProjectId>/workflow/`, `<state>/quota/`, `<state>/routing/`; owners define retention and record bodies |
| Scratch | `<state>/tmp/`; operation-scoped, never a whole-root purge |

There is no telemetry ledger subtree: `telemetry stats` reduces run's terminal
records using the bundled TOML price asset. No telemetry import or source-store
mutation is installed here.

Capture home/environment once. Core selects paths from supplied inputs;
supervisor binds canonical directories and checks containment. Reject roots
equal to home or filesystem root, or equal to/beneath the product checkout or
any legacy root: `~/.agents`, `~/.local/state/agent-harness`,
`~/.local/state/provenant`, selected workspaces' `.agent-run`, and explicitly
supplied old roots. Reject any `.agent-run` path component even without a
workspace. Resolve existing ancestor aliases and append only a normal missing
tail; lexical checks alone cannot protect filesystem access. Never initialise
through a legacy alias. Do not open legacy root contents to test containment.

Reads, help and `root` do not create state. Mutations initialise explicitly:
directories 0700, files 0600, executable 0755. Config never opens SQLite.
Each owner uses supervisor's safe locks and filesystem publication. Aliases
bind to the same canonical lock; no whole-root sweeper is introduced.

### Identities and time

IDs are distinct newtypes over 16 supplied random bytes, formatted as 32
lowercase hex digits. Supervisor supplies entropy. State initialisation
persists `HostId` once under lock. `ProjectId` is **host-local**, randomly
allocated per canonical Git common directory; linked worktrees share it but
have separate `WorkspaceId` bindings and writer leases. Non-Git workspaces
bind their canonical directory. It is never a federation key.

Config owns `bind_project(roots, ProjectLocation, SafeFs) -> ProjectBinding`:
under a project-binding lock it finds or creates the canonical binding.
Confinement owns Git discovery via supervisor's fixed `run_utility` Git reads
(primary/linked/non-Git fixtures); core does no Git work. Fabric-runtime owns
`bind_federated_project(ProjectBinding, CrossHostProjectKey)` and the key's
validation, persistence and peer mapping. Equal remote URLs do not silently
merge identities. Moves/rebinding require an explicit project operation.

`ActorId` identifies an attributed actor, not authority or a model route;
fabric-runtime resolves external addresses to a host-qualified actor.
`WorkScope` is a closed enum: `Project(ProjectId)`,
`Workspace { project: ProjectId, workspace: WorkspaceId }`, or
`Run { project: ProjectId, run: RunId }`. Domain boundaries validate membership.
`CancelToken` is a cloneable, in-memory cooperative flag (`Arc<AtomicBool>`)
with `cancel()` and `is_cancelled()`; it is neither serialised nor a provider
signal. Run/supervisor decide any permitted stop under their custody rules.

`WallTime` (`UtcMicros`) is UTC integer microseconds with one RFC3339 `Z`
rendering. `MonoTime`, `DurationMicros` and `Deadline` use checked integer
arithmetic; monotonic values are process-local and never persisted as a
cross-process deadline. Core owns `Clock { wall_now(), mono_now() }` and the
time values; supervisor owns production `SystemClock` using `jiff` for wall
time and `std::time::Instant` for elapsed time. Tests inject a fake clock.
Supervisor's state machine receives sampled values, never reads the clock.

`GitOid` is `Sha1([u8; 20]) | Sha256([u8; 32])`;
`GitOid::parse(&str)` accepts complete 40/64 hexadecimal digits, emits lowercase
and rejects abbreviations or other lengths. Git discovery supplies the
repository's object format and validates the variant. This is distinct from
Provenant's SHA-256 `Digest`.

Paths stay `PathBuf`/`OsString`. Durable Unix paths use tagged `PathWire`
(`utf8` or lowercase hex native bytes), never lossy conversion. Protocols
requiring UTF-8 fail explicitly. `RelativePath` rejects absolute components,
`.` and `..`; supervisor enforces no-follow, handle-based containment.

### Record models

`StateHeader`, `ProjectBinding`, `InstallReceipt` and `ImportReceipt` are
stored typed serde models with `schema_version = 1`, closed enums and
`deny_unknown_fields`. Unknown major versions fail before mutation; absence
has only documented defaults. Domain owners choose their own extension rules.
Persisted models can derive `Serialize, Deserialize`; facts built only by
trusted edge code derive **`Serialize` only** (including workflow validation
facts and run's `AttemptFact`). Decode a separate stored record and validate
it before constructing a fact; deserialisation cannot manufacture evidence.

### Codecs and canonical bytes

Use serde/serde_json, sha2, thiserror and jiff from the allowlist. The records
reader rejects duplicate object members, invalid UTF-8, trailing input,
non-finite numbers and integers outside i64/u64, including ignored fields,
before typed deserialisation. Default limits: 1 MiB per config/metadata file,
8 MiB per general JSON record, nesting depth 64 and 100,000 collection members;
callers request a smaller bound or an explicit domain-specific larger limit.
Enforce bytes while reading, not after allocation. Errors carry code, path and
location, never the rejected value.

Define `CanonicalV1` for digest-bearing typed records: UTF-8 JSON without BOM,
whitespace or final newline; object keys recursively sorted by UTF-8 bytes;
array order preserved; strings escape quote/backslash and controls (lowercase
`\u00xx`, with `\b`, `\t`, `\n`, `\f`, `\r` for those controls), leave other
Unicode literal, and do not normalise Unicode. Integers use shortest decimal
(`-0` becomes `0`); booleans/null use JSON spellings. Floats are forbidden in
a canonical value: owners use integer units for cost/time or explicit decimal
strings. Unknown fields do not silently enter a signed/digested record. This
is a Provenant contract, not a claim of RFC 8785 compatibility.

`record_digest(kind, version, bytes)` is SHA-256 over
`"provenant-record\0"`, the kind, NUL, decimal schema version, NUL, then canonical
bytes; represent the full digest as `sha256:<64 lowercase hex>`. File digests
hash exact raw bytes with a separate `FileDigest` type. Never use a display
prefix as an identity. Attempt records and install receipts
call this owner rather than depending on map iteration or Python JSON output.
Domain owners decide which fields enter each digest.

The YAML reader returns only typed maps/sequences/string/bool values, with
line/column spans. Accept block maps/sequences, quoted/plain scalars, comments,
`>`, `>-`, `|`, and flow sequences of quoted strings. Plain scalars resolve
only `true`/`false` or strings. Ambiguous boolean words, numeric-looking and
null-like values require quoting. Refuse anchors, aliases, tags, flow maps,
multiple documents, tabs, duplicate keys and multiline plain scalars, naming
the construct and line. Handle quoting/comment boundaries and block folding
explicitly; do not use regular expressions as a YAML parser. Frontmatter must
start at the beginning of `SKILL.md` with `---` and terminate with `---`;
parse only that bounded block. Sidecars parse as a complete document.
Limit external YAML to 256 KiB, depth 32 and 10,000 nodes. The workflow skill
validator owns allowed metadata keys; records owns syntax, not skill policy.

### Configuration

Use `toml_edit` with its R6-approved `serde` feature to load one strict typed
`<state>/config.toml`. Files declare `schema_version = 1`; unknown keys,
duplicates, bad types/ranges and unsupported versions fail. Absent means
compiled defaults; unreadable/malformed means error. No include,
interpolation, executable configuration or automatic migration.

The optional `profile = "balanced" | "economy" | "rigorous"` selects one
compiled profile; omission selects `balanced`. Domain crates export a
`DomainSection { name, defaults: fn(Profile) -> &'static str, validate }`
entry and their own per-profile TOML assets. Config nests each asset under
`[domains.<name>]`; composition registers the entries with
`DomainSections::<Aggregate>::new(&ENTRIES)`. `DomainSection::new::<Section>`
uses the section's closed serde type and `DomainConfig::validate`, which returns
all independent semantic failures. Config contains no domain policy assets or
const-concatenation. These assets are the only preset channel: balanced defaults; economy concurrency cap 4, context cap
200,000 and weighted pool rotation; rigorous effort +1 except mechanical/bulk,
one additional review, cross-family required and full re-review. Domain
owners validate those values and enforce their minima. The config crate
loads/validates user domain sections over the selected compiled defaults once;
there is no profile layering or per-request profile selector. Knobs resolve
through the embedded registry (default, profile preset, optional task class,
then the user's `[knobs]` override); there is no other generic knobs API. A preset never grants authority or a
provider stop reason.

Every allowed section key appears in its complete default asset. Tables merge
recursively; arrays replace as a whole. Structured arrays provide a complete
representative default element, allowing key/type/range checks at each indexed
leaf before merging. Domain types and assets live with their domain owner;
config has no dependency on those runtimes. The aggregate supplied by composition
remains typed. No `Preset` policy struct or config-owned policy asset remains.

`load`/`read` refuse the first ordered finding. `config_show(roots, fs, sections)`
and `check_config(roots, fs, sections, limit)` use the registered domains.
`check_config` collects unknown-key, type, range and semantic findings, ordered
by source line, column, dotted path and rule, with a limit of 1–100, a total count
and a truncation flag. Messages cap key paths at 256 characters. CLI owns
cursor/digest pagination and maps findings to exit 1; malformed TOML is a distinct
`ConfigError::Syntax` mapped to exit 2. Unsupported integer schema versions are
refused before header or domain decoding. Diagnostics retain one-based line and
column, dotted key and `Origin` (syntax without a recoverable key uses `$`);
invalid compiled assets are `Origin::Compiled`,
never attributed to the user's file. `Redacted<T>` serialises and formats as
`[redacted]`; domain owners use it for secret-bearing leaves. The `test-support`
feature exports `assert_leaf_origins` for every serialised effective leaf,
including array members and empty containers. A reader of `None` means the file
is absent; no optional config-owned policy value remains.

Catalogue, adapter table and prices ship as TOML release assets, never copied
into mutable state as defaults. Routing/providers/quota/workflow validate semantics. Telemetry owns
the price schema (`PriceTable`); state owns how bundled TOML assets load,
including the replacement for `config/telemetry-prices.json`. `config show`
returns effective non-secret values and origins;
`check config` validates without mutation or provider access. There are no
config/install MCP tools. Processes use an immutable startup snapshot.

## Commands and installation

CLI's [binding command contract](README.md#cross-cutting-decisions) supplies the
JSON envelope (`schema_version`, `command`, `status`, `data`, `error`), 1 MiB
result cap and exit mapping. Config returns typed outcomes. Install success
or no-op is `ok`; drift is findings; foreign selectors are refused; malformed
input is invalid; I/O or lock contention means cannot proceed. After a partial
write, `data` lists completed/remaining actions and a re-apply instruction;
CLI maps the underlying failure to its existing code, never exit 4. JSON
stdout is one result; diagnostics are bounded stderr. Harness changes are
printed before applying (stderr preview in JSON mode, final actions in data).
A failed preview stream prevents writes.

| Command | Inputs and behaviour |
| --- | --- |
| `root` | No flags; prints the absolute release root as one newline-terminated path, so `$(provenant root)/skills/...` works. An uninstalled build requires explicit `PROVENANT_ROOT`. No Git discovery, SQLite, state creation, threads or provider probes. |
| `config show`, `check config` | Effective config/origins or validation findings; only the global state selector. No mutation flags. |
| `install [--dry-run]` | `--source ABS` identifies a staged release; `--harness NAME` repeatable and explicit; optional `--bin-dir ABS`, `--hooks on\|off` (default off), `--adopt-existing`, `--legacy-checkout ABS`. Validate all destinations and print every replace/remove/create before writes. Dry-run creates no root, lock or receipt. |
| `check install` | Optional `--source ABS` staged release, as for `install`, for digest comparison; compare installed binary, assets, receipt and owned harness selectors without repair. Joins CLI's existing `check` subtree. |
| `uninstall` | Explicit `--harness NAME` repeatable; remove unchanged owned entries only. Keeps state, release assets, user skills/config and the running binary. No recursive deletion. |

### Release identity and bootstrap

CI/contributors build `cargo build --release --locked` in `tools/front-door`.
A workspace `build.rs` uses records' pure source-digest module. The digest
covers workspace/member manifests, lockfile, toolchain/config files when
present, every member's source/build scripts and explicit bundled assets,
including product skills/templates/instructions and surviving script/runtime
owners outside the Rust workspace.
Hash sorted workspace-relative POSIX names, framed by big-endian u64 name
length, name bytes, u64 content length and exact contents; prefix
`provenant-source-v1\0`. Reject duplicate, missing or symlink/escaping inputs.
Cargo rerun directives track additions/deletions; no digest environment override.

Build packaging emits typed `release.json` beside `bin/provenant` and the
product asset directories,
with source digest, target triple, toolchain and bounded file inventory/digests.
This is a build artefact, not a second executable or installer command.
`--version --json` reports the embedded digest and record/config versions.
Release builds trim source paths so checkout and registry paths do not affect
binary bytes. Packaging and source-aware install checks compare the binary's
reported source digest, target, toolchain and asset inventory with the manifest.
Executable assets retain mode 0700; other assets are 0600.

An installed binary resolves its own `bin/provenant` location to the release
root holding `release.json`; `provenant root` prints that root. The release
contains `skills`, `library`, `templates`, `config`, `instructions`, `scripts`
and `runtime` directly so `$(provenant root)/skills/...` remains usable. An uninstalled
build requires an explicit absolute `PROVENANT_ROOT`; it refuses without it.
These reads never create state. An installed release launches no Python owner by default. Without a checkout
Git entry, a local `.venv` interpreter or explicit `HARNESS_PYTHON`, each
Python-owned route fails with typed `owner unavailable in installed release`
and exit 4. Tests that exercise a Python owner set `HARNESS_PYTHON` explicitly.
Install verifies manifest, platform, file bytes and modes, then asks the
binary composition edge to run bounded version/help smoke checks through
supervisor (30 s, capped output, no provider access). A digest proves identity,
not publisher trust. Runtime startup never compiles itself.

Install copies verified files into the immutable release directory, then
atomically publishes a link to `bin/provenant` at the stable bin path and per-entry
links to release skills/instructions. Re-application accepts exact desired
bytes/links as no-ops, including writes completed before a crash. Until
`release.json` is published, mismatching unfinished files may be replaced.
That manifest is published after all other release entries; afterwards the
release is immutable. Equal source digests with different manifest bytes fail
with typed `ReleaseConflict`, naming both manifest digests and recovery.
Upgrades remove unchanged owned entries dropped by each selected target;
changed or unmanaged entries remain. The sole installation lock is
`<state>/install/install.lock`; no locks are left in harness or bin directories. Receipt
records release identity, resolved targets, selectors, expected owned
values and the optional absolute `legacy_checkout` substitution root plus
compiled manifest digest, so `check install` can repeat exact legacy checks. Publish it last.
A target is its harness plus resolved skills directory, so one receipt may hold
several targets of a harness, such as one per `CLAUDE_CONFIG_DIR`. Targets never
share a destination; each harness entry belongs to the target whose paths admit
it. Install replaces only the selected targets and carries the rest over;
`check install` verifies every recorded target. Uninstall selects the target
matching an environment-supplied home, or else every recorded target of the
harness. The receipt shape is unchanged, so version-one receipts still load.
No cross-file or two-host atomicity is claimed.

### Harness registry

Defaults below derive from each selected harness home. Typed user-only
`[install.targets.<harness>]` may override absolute home/config paths;
`[install]` may set `bin_dir`. Explicit flags win over config, which wins over
documented `CLAUDE_CONFIG_DIR`/`CODEX_HOME`, then defaults. Other legacy
environment overrides disappear. Paths are resolved once and shown in the
preview/receipt; they cannot be changed by a project/request.

| Harness | MCP file and owned selector | Hook file | Skills directory | Instruction path |
| --- | --- | --- | --- | --- |
| Claude Code | `~/.claude.json`: `mcpServers.fabric`, stdio command/args/env | `~/.claude/settings.json`: owned command leaves in `hooks.<event>[]` | `~/.claude/skills/` | `~/.claude/CLAUDE.md`, companion `HARNESS.md` |
| Codex | `~/.codex/config.toml`: `[mcp_servers.fabric]`, command/args/env | `[features] hooks = true` in config plus `~/.codex/hooks.json`; own only required feature/command entries | `~/.codex/skills/` | `~/.codex/AGENTS.md`, companion `HARNESS.md` |
| Cursor | `~/.cursor/mcp.json`: `mcpServers.fabric` | No supported hook registration | `~/.cursor/skills/` | No supported global instruction registration |
| Agy | `~/.gemini/config/mcp_config.json`: `mcpServers.fabric` | No supported hook registration | `~/.gemini/skills/` | `~/.gemini/GEMINI.md`, companion `HARNESS.md` |
| OpenCode | `~/.config/opencode/opencode.jsonc`: `mcp.fabric`, `type="local"`, command array/environment | No supported hook registration | `~/.config/opencode/skills/` | `~/.config/opencode/AGENTS.md`, companion `HARNESS.md` |

Every MCP entry executes the absolute stable binary with
`--state-dir ABS fabric mcp --client NAME`; JSON/TOML clients use command and
args, OpenCode uses a combined array. No checkout, Python, Node or secret env.
CLI owns accepted startup arguments and the `HookSpec` table (harness,
protocol event, matcher, timeout, handler), the installer's source for hook
entries, supplied to config by binary composition without a cli dependency.
CLI's fixtures define stdin/stdout and exits. Codex fixtures cover the feature
switch and `hooks.json`, rather than invent a Claude settings or notify hook.
Unsupported hooks refuse before writes. When disabling/removing Codex hooks,
remove only an unchanged installer-owned feature value; preserve pre-existing
user `hooks = true`. Never register a hook whose fixture/policy is absent.

Use `toml_edit` document mutation to preserve unrelated comments/order/values.
JSON/JSONC uses records' bounded token/span parser to splice owned leaves and
reparse; preserve unrelated bytes, comments, trailing commas and array order.
Reject malformed/duplicate/ambiguous containers unchanged. Never serialise an
entire user config or log its raw contents. Config files must be regular;
symlinked configs refuse with their path, requiring an explicitly selected
regular destination. `~/.claude.json` has a 16 MiB byte bound; other harness
configs retain the 1 MiB bound. Quit Claude Code before installation because
it rewrites this file while running. Recheck identity and bytes immediately
before publication. Instruction/skill links are inspected with `readlink`,
never opened through the old link.

### Legacy entry reconciliation

`--legacy-checkout ABS` authorises matching **only** the compiled legacy
entry table below at the selected harness paths.
`assets/install/legacy.toml` is the authoritative, compiled matching manifest:
finite entries with harness, config selector/event/matcher, exact command or
link template and replace/remove disposition. Only captured home and the
explicit legacy checkout substitute into templates; no prefix/glob/regex
ownership matches. Fixtures must cover every entry. The table enumerates names,
exact command templates/link targets and each new release's `replace` or
`remove` disposition. Do not infer ownership from a basename, substring,
arbitrary shell parsing or an old receipt. The build fixtures enumerate every
legacy skill name below, including removed skills and `_shared`, and every
accepted interpreter/quoting form; unsupported commands
are conflicts, never guessed matches. No directory scan under the checkout or
`~/.agents` is performed during install. `~` in this table means captured
home, not a literal tilde or shell expansion.

The compiled legacy skill names are `autopilot`, `code-review`, `d2-diagrams`,
`deliver`, `diagnose`, `engineering-docs`, `engineering-writing`, `evaluate`,
`grill-me`, `implement`, `legal-writing`, `natural-writing`, `orchestrate`,
`prototype`, `refactor`, `release`, `retrospect`, `scope`, `session`,
`setup-repo`, `skill-craft`, `tdd`, `tracker`, `ui-ux-design`, `work-map` and
`_shared` under `skills/`; `playwright`, `react-performance`, `tanstack-query`
and `typescript-clean-code` under `library/`. Build tests freeze these names
independently of the new release's retained-skill list.

| Legacy entry | Exact match and planned action |
| --- | --- |
| Stable command file | At `~/.local/bin/provenant`, replace only the old installer's managed regular binary form: a manifest-listed ELF or Mach-O header and its complete embedded managed marker and source identity. Other files conflict unless explicitly adopted. |
| Git hook links | Resolve the hooks directory from canonical `--legacy-checkout`, independently of `--project`. Only manifest-listed links whose `readlink` equals that checkout's listed source may be replaced or removed there. Preserve and report regular files and foreign links. A `replace` link remains unresolved without a fixture-backed `HookSpec`. Every other removal obeys protected, state and source root checks. |
| Four Claude hooks | Compiled commands for `<legacy>/skills/session/scripts/compact_reinject.py`, `<legacy>/skills/session/scripts/state_check.py --hook`, `<legacy>/scripts/utility_output_hook.py`, `<legacy>/scripts/lane_safety_guard.py`, at `SessionStart`/matcher `compact`, `PreCompact`/no matcher, `PostToolUse`/matcher `Bash` and `PreToolUse`/matcher `Bash`, respectively. A `replace` entry requires a fixture-backed CLI `HookSpec`; without one, preserve the legacy entry byte-for-byte and report it unresolved. Remove only exact `remove` entries. Preserve sibling commands, even in the same hook group. |
| Skill links in all five skills directories | For each compiled name, `readlink` equals `<legacy>/skills/<name>` or `<legacy>/library/<name>` (normalised relative targets allowed). Replace retained names; remove retired names and `_shared`. A whole skills-directory link to exactly `<legacy>/skills` is unlinked and replaced by a real directory of managed entries; never traverse it. Foreign files/directories conflict. |
| Claude workflow links | `codebase-polish.js`, `cross-verify.js`, `implement-run.js`, each targeting `<legacy>/workflows/<name>`. Remove all three; no JS workflow replacement. Handle an exact whole-directory link without traversal. |
| Instructions and constitution | Replace Claude `CLAUDE.md` and Codex `AGENTS.md` links whose `readlink` is exactly `~/.agents/AGENTS.md`, plus the registry's instruction links to `<legacy>/AGENTS.md` and companions targeting `<legacy>/HARNESS.md`. Link directly to the new release instruction assets. Apply the authorised variable rewrite below before replacing links. |
| OpenCode instruction array | Exact `instructions[]` strings for `~/.agents/AGENTS.md` and `<legacy>/HARNESS.md`: replace with new release instruction assets, preserving other array entries. Do not dereference old paths. |
| MCP entries in all five clients | Match compiled old command/args/env shapes for the stable shim (`~/.local/bin/provenant`, absent/empty args and the exact environment below) only. Other launch commands conflict. Known path values may reference old stores but are never dereferenced. Replace with the new stdio entry and remove obsolete owned env keys. Unknown/secret-bearing variants conflict. |

Legacy MCP environment keys are exactly `AGENT_FABRIC_STATE_DIRECTORY` =
`~/.local/state/agent-harness/fabric`, `AGENT_FABRIC_SEAT` and
`AGENT_FABRIC_CLIENT_LABEL` = the selected seat (`claude`, `codex`, `cursor`,
`agy`, `opencode`). The default old install omits
`AGENT_FABRIC_INSTANCE_ROOT`; an explicit non-default root is accepted only
as a recorded literal host entry in the compiled manifest.
Claude also has `type = "stdio"`; OpenCode has `type = "local"`,
`enabled = true`, a one-element command array and `environment` instead of
`env`. Other recorded host-specific values/interpreter paths must be finite
literal entries in the compiled manifest before cut-over. Session hook
commands use `python3 "<script>"` (plus `--hook` for state check); lane safety
uses the old installer's exact `shlex.quote` rendering of `python3 <script>`;
utility output uses its recorded absolute Python interpreter with that
rendering. Never accept arbitrary interpreters, shell suffixes, labels or env
keys by pattern.

The installer rewrites `AGENT_FABRIC_LABEL` to `PROVENANT_AGENT_LABEL` in
`~/.agents/AGENTS.md` and the files linked to it. New release instruction
assets use `PROVENANT_AGENT_LABEL` before publication. Print each path and
exact before/after change before applying it; a failed preview prevents writes.
This instruction-only exception to the old-root prohibition authorises the
edit. Plan each regular instruction file once, verify its identity and
unchanged bytes before atomic publication, and refuse unresolved or foreign
links. Never write the primary checkout; replace a link into it with the
updated release asset. No other old-root contents are opened or changed.
Re-application accepts already updated text; `check install` verifies no old
variable remains in these files.

Absolute and relative link targets are compared lexically after resolving
relative components against the link's parent; target contents are read only
for the authorised instruction rewrite. Only named harness entries and those
instructions change, not old runtime state, transcripts or the primary
checkout. Print matched selector/target, disposition and replacement before
any write. An unresolved legacy entry is preserved and reported in the install
preview/outcome and as a `check install` finding (exit 1). The cut-over
end-to-end suite requires zero unresolved entries. `check install` checks all
compiled legacy selectors for residual references. Unrelated historical files
are outside the installer's scope.

`--adopt-existing` is limited to an occupied destination selector or the
stable regular executable, printing/pinning its exact identity and digest.
It never authorises deletion or a broad legacy sweep; link replacement/removal
requires the exact table match and `--legacy-checkout`. Foreign directories
and symlinked executables refuse. Adopting inline secret env values refuses,
so neither preview nor receipt becomes a credential store. Apart from the
authorised variable rewrite, preserve existing regular user instructions;
report a manual include if no owned link can be installed. Do not seed them.

### Publication and recovery

Supervisor supplies no-follow snapshots, metadata identity, entropy, deadline
locks (non-blocking flock poll), same-directory staging, fsync and publish
with or without replace. Config uses only that safe filesystem API, never raw
libc or `Command`. Lock the install root and selected destination parents in
stable order; validate all proposals before writing. Recheck target identity
and content immediately before each atomic publication; on drift stop and
report the selector. These locks serialise cooperating installers; an
uncooperative editor racing the final recheck/rename is not excluded. Cut-over
requires stopping harness clients and config editors before the dry-run and
keeping them stopped through install/check; chairs own that cut-over gate.
User-edit preservation applies to those quiescent files and edits detected
at the recheck, not an arbitrary simultaneous writer. Do not claim
compare-and-swap or exchange semantics.

Each file/link update is individually atomic and parent-fsync'd. On failure,
return the applied/remaining actions and re-run the same `install` after
resolving conflicts. Deterministic exact-desired matching plus the compiled
legacy table makes this idempotent even if no receipt was published. No
automatic inverses, saved binaries or displaced user files. Removal compares
current values against the receipt and only unlinks unchanged owned entries;
never recursively delete a directory. ENOSPC, broken output and uncertain
publication are typed failures, never silent success or provider stop requests.
Rollback is an operator reinstall of `pre-rust-rewrite`, preserving old state.

## APIs and ownership boundaries

| Owner | Public API or contract |
| --- | --- |
| core | Distinct `HostId`, `ProjectId`, `WorkspaceId`, `RepoId`, `ActorId`, `RunId`, `TaskId`, `AttemptId`, `DeliveryId`, `SessionId`, `InstallId`, `ImportId`; `GitOid`, `Digest`, `WorkScope`, `CancelToken`; `RootInputs -> Result<Roots>`, `RelativePath::parse`; `WallTime`/`UtcMicros`, `MonoTime`, `DurationMicros`, `Deadline`, `Clock`. No ambient discovery or I/O. |
| records | `decode<T: DeserializeOwned>(reader, Limits)`, `encode<T: Serialize>`; `CanonicalV1::bytes`, record/file/source digests; `PathWire`; bounded frontmatter/sidecar/JSONC parsers; state/install/import models. Stored records and `Serialize`-only facts are separate types. |
| records (CLI contract) | `CommandResult<T> { schema_version, command, status, data: T, error: Option<CommandError> }`, closed status enum and redacted typed errors; `Finding` and `HelpCatalogue`. Output supports additive fields within v1. CLI constructs/renders envelopes and maps exits; no library imports cli. |
| config | `load(roots, reader) -> Result<ResolvedConfig>` with origins and selected compiled preset; `bind_project(roots, ProjectLocation, SafeFs) -> Result<ProjectBinding>`; pure `plan_install(Release, Targets, Snapshots) -> Result<InstallPlan>`; `install(plan, SafeFs, reporter) -> Result<InstallOutcome>`; `check_install`, `uninstall`. Plan is ephemeral, not a persisted recovery protocol. |
| supervisor | Production `SystemClock` (`jiff` wall time), entropy, safe no-follow FS/locks/publication and bounded version/help smoke processes. `run_utility` for fixed Git/gh reads; test-only fixture binary with a scoped `Command::new` allow for process tests. |
| confinement | `discover_project(Workspace, &DiscoveryAuthority) -> Result<ProjectLocation>` using supervisor Git reads; validates Git object format, repository/path membership and authority. Config consumes the result without a reverse dependency. |
| fabric-runtime | Cross-host project key, `bind_federated_project`, external address-to-actor binding and federation routing of run queries. Fabric owns its read-only import and receipt. |
| run | Registry, admission, attempts/status and atomic admission/operation insertion in run's durable store; top-level runs layout, `AttemptReader::attempt(AttemptId) -> AttemptFact`, `AttemptReader::attempts(filter) -> impl Iterator<Item = Result<AttemptFact>>` and `snapshot` with active count. State owns no duplicate registry. |

`AttemptFact` lives in records and derives `Serialize` only. Run constructs
it from its store: task, attempt number, operation, project, terminal state,
exit, started_at/finished_at, resolved route (provider, model, effort, account
label), usage counts (`u64`), reviewer and `delivery_id`.
Terminal records are immutable; highest attempt number selects the latest
attempt. Telemetry's `stats(facts, &PriceTable, window) -> Stats` accepts any
iterator of `AttemptFact`, filtering on `finished_at`, without revision dedupe.
Retention is out of scope. CLI composes run's reader with telemetry, so run
has no telemetry dependency; execution-run removes that R2 edge with a note.
Routing owns account eligibility; run records the resolved account label and
telemetry reads it from the fact. There is no `--account` flag.

Records does not own lifecycle, cooldown, deletion or review policy. Config
has no domain-runtime dependency: binary composition connects its values to
supervisor/confinement/run/Fabric. Domain owners supply typed config sections
without importing cli. `ExecutionContext` belongs to run/composition, carrying
validated project, actor and authority, never raw argv or an untyped config bag.
Before discovery, confinement validates a `DiscoveryAuthority` containing the
caller and permitted workspace paths, with no ProjectId. Composition calls
`discover_project`, then `bind_project`, then constructs `ExecutionContext`.

The other cross-area asks stay with their named owners: run's `wait(Any|All)`
wakes on terminal or `InputRequired`; every wait has an explicit budget,
capped at 14400 s CLI/55 s MCP.
`events(cursor)` is lazy pull, and cancel/resume/read_output/export take
`&ExecutionContext`. Run owns `git_evidence_packet` via supervisor Git reads
and completed-attempt facts with every field workflow consumed (terminal
state/exit, subject, output, route, reviewer); dispatch carries optional
`delivery_id`. The run-directory receipt/manifest/finaliser contract is cut
in favour of run records plus guardian scratch settlement. Telemetry drops `upsert_delivery` and
the legacy delivery/change reducers; its terminal-record `stats` reducer stays.
Workflow owns writing-rule data and
CLI freezes `check writing PATHS`. Use an in-house snapshot helper, no `insta`
dependency.

## Import, fixtures and cut-over

Only Fabric imports: explicit one-shot read-only open/blocked tasks and
unacknowledged requests from the last 14 days; new cursors start after import.
Fabric defines its command and receipt. Install/startup never imports. Do not
read transcripts, revive runs/processes/claims/leases/sessions, or import
telemetry/quota history. The old ledger remains available to the old install;
telemetry owns retirement of the OTel collector. `tests/test_host_identity.py`
and the old OTel cases in `tests/test_install_harness.py` are counted once by
state. The separately moved collector helper tests and Swift collector tests
retire with the OTel sources in slice 4; they are excluded from telemetry's
budget. Existing installed services and data remain outside source deletion.

Source reads must not write SQLite journals, SHM or checkpoints. Require a
quiescent self-contained source or supplied consistent snapshot; refuse a
live WAL requiring writable reader sidecars. Do not use `immutable=1` on a
changing DB. Fabric bounds/validates input, deduplicates source/old-row IDs
and publishes destination facts transactionally. Core supplies identity/codecs,
not an import engine.

Before chairs install on either Mac, both targets must pass isolated-root
fake dispatch, MCP handshake and every retained hook. Build one accepted
revision, verify release digests, quiesce old clients without killing healthy
providers, and run `install --dry-run --legacy-checkout ABS` for all five live
harnesses. Resolve every legacy reference, then re-run without dry-run.
Restart clients and verify `check install`, version, fake dispatch, MCP and
hook events. Report each host separately. Re-apply a failed install; rollback
reinstalls the old tag. No build lane touches the stable install or primary
checkout, and no live provider run is implied.

Record sanitised real-client contracts in
`tools/front-door/tests/fixtures/contracts/` with client/version, platform,
capture method, redactions and parsed expectations. Use isolated homes and fake
MCP executables; old Python/TS output is not an oracle.

| Contract | Required fixtures |
| --- | --- |
| Five harnesses | Accepted MCP config/stdio argv; Codex feature flag and hooks.json; TOML comments/inline tables; OpenCode JSONC comments/trailing commas; unrelated entries and unsupported-version refusal. |
| Hooks | Actual enabled client event input, allow/no-op/deny output and exits, plus accepted registration. CLI supplies policy. No missing-fixture install. |
| Legacy cut-over | Four exact hook commands (including recorded interpreter/quoting forms), all compiled skill links plus `_shared` across five harnesses, three workflow links, instruction/HARNESS links, the global-instruction variable rewrite and live MCP entries. Include near-matches, mixed hook groups, foreign directories and broken links; assert only the authorised instruction files are read/changed in old roots, and every change is printed first. |
| Skills/Git/MCP | Every tracked SKILL.md/agents YAML subset; primary/linked/non-Git discovery and SHA-1/SHA-256 repos; Fabric `initialize`/list/EOF/cancellation and stdout purity. |

Unit tests prove ID/path/version/time overflow, canonical bytes and digest
separation, integer/duplicate/bounds refusal, YAML/JSONC syntax, typed TOML,
presets, origins and editor byte preservation. Test cancellation scope and
`Serialize`-only fact construction at the trusted edge. Integrations use real
commands with synthetic release/provider/MCP fixtures: first install, no-op,
partial failure then re-apply, drift, malformed config, adoption, exact legacy
removals, `check install`/`uninstall` and two independent host roots. Assert
help/root creates nothing and unowned spans/old-state fixture trees remain byte-identical
except the authorised instruction-variable rewrite.

CI process tests on Linux/macOS use FIFO/handshake barriers for installer
contention/deadlines, edits detected before publication, crash at each write,
ENOSPC/fsync/rename errors and re-apply without a receipt. All process access
is supervisor-owned; no sleeps, healthy-provider signals or exchange primitive.
Tests set HOME, state and harness paths to temporary roots and use repo-relative
`include_str!` fixtures; canonicalise macOS temporary aliases. Local checks are
fast and targeted; full workspace/harness and platform checks run in CI.

## Shared-leaf cut-over (Lane D, 2026-10-24)

`config show` now binds `config_show` with the complete registered domain
aggregate. Compiled `Profile` and the single typed `config.toml` replace the
profile CLI's layered JSON knob layers. `config set`/`unset` edit `profile` and
the `[knobs]` table (default, then profile preset, then override) against the
embedded `config/harness-knobs.json`.

`config::bind_host` owns the random host ID in the selected state's `host-id`.
The old hostname slug, `hosts.json` alias migration and telemetry host aliases
are retired. `core::RootInputs` and the binary's `roots` module own selected
state, installed asset and project resolution; the `AGENT_FABRIC_PRODUCT_ROOT`
environment variable is retired. `PROVENANT_ROOT` still selects assets for an
uninstalled binary; installed builds resolve their release directory.

Lane D deletes `scripts/harness_profile.py` (212 lines),
`scripts/host_identity.py` (102), `scripts/lib/roots.py` (31),
`tests/test_harness_profile.py` (437) and `tests/test_host_identity.py` (127).
The chair explicitly assigned these deletions to Lane D; the parallel Python
cut lane removes the remaining importers and shared Python implementations.
These are cut-over deletions, not Python-to-Rust adapters. Existing Rust
config, host binding and root boundary tests cover their retained behaviour.

## Deletion ledger and build order

Counts are physical lines at the source baseline above (`wc -l`), including
comments/tests. These are build-lane deletions, not changes in this design PR.
Every listed replacement lands with its old subject/tests removed in the same
PR; update surviving skill/hook/docs callers then, without forwarding adapters.

Slice 2 / PR #1369 deletes **no live Python, shell, registry, help or golden
fixture files**. Its repair restores `config/harness-knobs.json` and
`tests/test_harness_profile.py` exactly from `origin/main`, along with the
`provenant profile` launcher/wiring, rotation tests and surviving profile
instructions. Silence-watchdog environment overrides, Fabric `--knob`,
`PROVENANT_RISK_TIER` export and economy profiles remain live in Python.
The old inert Rust config/knob implementation was already deleted by slice 1
(PR #1367); it stays deleted. The three config-owned policy preset assets and
`Preset` policy types introduced during slice 2 are withdrawn before landing,
so they are not deletions relative to `origin/main`.

Slice 3 retired `scripts/install-harness`, `scripts/install-skills` and the
checker entrypoint as exit-3 stubs. Slice 4 deletes the two shell stubs and the
checker module's CLI stub when command wiring lands. The read-only
`provider_registration_status` query and `tests/test_check_provenant_install.py`
remain because `scripts/fabric_hosts.py` still imports the query. The OTel
installer helper and both collector test modules retire with the Swift source
and plist. Tests are removed only when their subject retires.

Slice 5 removes `skills/_shared/harness_profile.py`,
`scripts/harness_profile.py`, `config/harness-knobs.json`,
`tests/test_harness_profile.py`, `scripts/provenant-profile` and its front-door,
help/golden and caller-test wiring only after the last Python importer retires.
It also removes surviving profile/knob instructions then. The final deletion
inventory belongs to that slice, not to this typed-loader PR.

The CLI parser step retired `scripts/configure-fabric-mcp.py` (818 actual
lines) and the remaining `tests/test_configure_fabric_mcp.py` (1,148 actual
lines) with the implicit no-argument MCP route. The inventories below retain
the original planning baseline; do not count those deletions again in later
owner slices. The explicit transport and new registrations land at cut-over.

| Slice | Python source/helper | Lines |
| --- | --- | ---: |
| 5 | `scripts/lib/roots.py` | 33 |
| 5 | `skills/_shared/roots.py` | 45 |
| 5 | `scripts/host_identity.py` | 102 |
| 4 | `scripts/lib/jsonc.py` | 68 |
| 1 | `tools/front-door/crates/core/scripts/generate_unicode.py` | 29 |
| 5 | `skills/_shared/harness_profile.py` | 403 |
| 5 | `scripts/harness_profile.py` | 212 |
| 3 | `scripts/lib/front_door_build.py` | 249 |
| 3 | `scripts/lib/product_root_resolver.py` | 107 |
| 3 | `scripts/write-product-root-pointer.py` | 27 |
| 3 | `scripts/lib/stale_checkout.py` | 76 |
| 3 | `scripts/install-provenant-command.py` | 272 |
| 3 | `scripts/managed_installation_manifest.py` | 175 |
| 3 | `scripts/manage_installation.py` | 850 |
| 3 | `scripts/instance_installation.py` | 811 |
| 3 | `scripts/install-workflows` (Python) | 315 |
| CLI parser, PR #1386 (deleted here; state owner accepted) | `scripts/configure-fabric-mcp.py` | 818 |
| 4 | `scripts/configure-claude-hooks.py` | 134 |
| 4 | `scripts/configure-codex-skill-overrides.py` | 140 |
| retained query | `scripts/check-provenant-install.py` (`__main__` stub removed) | 0 |

| Slice | Deleted or trimmed Python tests | Lines removed |
| --- | --- | ---: |
| 5 | `tests/test_host_identity.py` | 129 |
| 1 | `tests/test_product_root_single_resolver.py` | 59 |
| 5 | `tests/test_harness_profile.py` | 445 |
| 3 | `tests/test_front_door_build.py` | 342 |
| 3 | `tests/test_product_root_resolver.py` | 151 |
| 3 | `tests/test_split_root_contract.py` | 147 |
| 3 | `tests/test_stale.py` | 204 |
| 3 | `tests/test_instance_installation.py` | 931 |
| 3 | `tests/test_install_provenant_command.py` | 272 |
| 3 | `tests/test_install_skills.py` | 1,311 |
| CLI parser, PR #1386 (remaining module deleted here) | `tests/test_configure_fabric_mcp.py` | 1,148 |
| retained query | `tests/test_check_provenant_install.py` | 0 |
| 3 | `tests/test_install_harness.py` (deleted) | 2,177 |

Planning baseline Python total: **12,705 lines removed**, of which the state
slices remove **10,739**. PR #1386 deletes the remaining MCP configurer and its
tests, **1,966 lines**, with state-owner acceptance and no live callers. These deletions belong to the CLI parser
change and must not be counted again in a later state slice. Shell entrypoints removed in the
corresponding slices: `scripts/provenant-profile` **5**, `scripts/install-skills`
**87**, `scripts/install-harness` **702**; shell total **794**. The removed
installation test module also covered collector, workflow and other hook
subjects. Its OTel tests moved to `tests/test_install_otel_collector.py`; the
2,177 deleted lines include their old cases once, and telemetry excludes those
cases. State deletes all of `tests/test_install_skills.py` with the old installer
in slice 3; execution-run has no edit/deletion claim on that file. Do not carry
unrelated old tests into Rust config.

No TS file is wholly owned/deleted by this area. The dependent lanes delete
these mixed files with their own consumers, without double-counting them here:
Fabric's `runtime/fabric/src/identity.ts` **196** and `run-root.ts` **96**;
supervisor/Fabric's `process-identity.ts` **392** and associated TS tests.
Likewise run owns `skills/orchestrate/scripts/layout.py` **245** and
`tests/test_exec_layout.py` **158**, `attempt_evidence.py` **107** and
`tests/test_attempt_evidence.py` **83**; supervisor alone owns/deletes
`attempt_storage.py` **176** with its last provider/cleanup importer.
Fabric/routing owns `fabric_records.py` **647**
and `tests/test_fabric_records.py` **816**. Their shared byte primitives call
records; their domain logic does not move into records.

Keep shared `tests/support/{modules,front_door,proc}.py` until their last
Python test consumer is deleted; CLI/final runtime removal owns that deletion,
along with `scripts/lib/harness-python.sh` **77** and
`scripts/lib/agent-fabric-tsx-loader.sh` **166**. Hook bodies/tests,
`install-otel-collector`, JS workflows and provider account identity retire with
CLI/telemetry/workflow/provider owners. State/config deletes no `schemas/`
files: those are source material, and each domain owner decides the fate of its
record schemas when its typed records land. No schema-file deletion is
authorised or counted by this area's ledger. `tests/test_record_schemas.py`,
`scripts/lib/projection.py` (70 lines) and old golden/oracle machinery follow
the rewrite plan's other deletion owners. None is an unlisted State
implementation to port. Config-data deletions are additional to the language
totals: slice 2 removes
`config/harness-knobs.json` (118 lines), replacing the knob registry with
compiled preset defaults. Domain owners replace the remaining policy
files in their TOML-default PRs: routing `model-routing.json` (1,090),
`model-preferences.json` (17), `adapter-compatibility.yaml` (306); run/Fabric
`graph-defaults.json` (8); telemetry `telemetry-prices.json` (319); workflow
`delivery-profiles.json` (109), `security-evidence.json` (42), `risk-policy.json`
(69). They report those data-file line changes separately. No owned YAML config survives
its domain's TOML replacement PR.

Replace/prune the inert crate source trees: core **6,624**, records **3,181**,
config **8,197** Rust lines (18,002 total). Also delete the binary modules
`tools/front-door/src/resolve.rs` **495**, `stale.rs` **150**, `json.rs` **389**,
`check.rs` **115** and `exec.rs` **109** (1,258 lines); root, codec, install-check
and process responsibilities move to the owners above. The crate deletions
include all `pending`,
`oracle`, `test_support`, core `py/*`/`js.rs` and their tests. Keep only useful
new-design code; delete misplaced policy copies rather than preserve old API
shapes. The language shims/oracle runner alone account for 4,084 Rust lines;
the Python generator's 29 lines are already counted above. Planning estimate:
about **6,000 Rust lines including replacement tests**, replacing 19,260 Rust
and deleting 13,125 Python/zero TS lines: expected Python+TS+Rust net
**-26,385** (including shell, **-27,179**). This is an estimate, not a build
quota; each PR reports its actual `Net lines:` and explains growth. Domain
moves count once in their landing PR, not twice in subsystem estimates.

Land these slices in dependency order:

1. **Values and codecs:** core/records, clock trait, cancellation/scope/OID
   types, stored models versus facts; delete JSONC and emulation owners with
   their tests. Shared root/host leaves wait for slice 5. Update Rust consumers;
   no old-language adapters.
2. **Typed config:** single-file load, compiled presets, show/check; delete
   knob machinery and its tests; shared profile leaves wait for slice 5.
   Domains land their TOML assets.
3. **Release and managed files:** source stamping, manifest, receipt and
   idempotent install; delete builders, pointers and instance installers.
4. **Harness cut-over:** fixture-backed editors, compiled legacy entries,
   five-client MCP/hooks/links, `check install`/`uninstall`; delete remaining
   installers and update skill/docs callers. Domain tests retire with their subjects first.
5. **Shared leaves, with the last importer:** state deletes both `roots.py`
   helpers, `host_identity.py`, both `harness_profile.py` files and their tests
   only after routing, providers, run, Fabric and workflow remove every static,
   dynamic and test/oracle importer. Check the importer set in the deletion PR;
   delay deletion to that owner's final consumer step if still live. This is
   execution-run's last-importer rule, not an early slice 1/2 deletion.
6. **Acceptance:** Fabric's bounded read-only importer, old-root no-write
   tests and both-Mac end-to-end checks; chairs own installation and rollback.

Each build PR needs native diff review, chair/cross-family review and exact-head
CI. Lanes do not merge or arm auto-merge.

Deleted inert code designated for reuse in the CLI, config, supervisor, confinement, providers and run designs is recoverable from `e0613438`; the owning lanes rehome it without the removed emulation.

The JSONC reader retains token spans; a path-to-span map shared with the TOML editor is deferred to the config lane rather than added to this values-and-codecs slice.
