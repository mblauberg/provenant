# State and config

Design proposal for the workshop chair and build lanes. Author route:
`codex/gpt-6.1-sol@high`. Governed by [PORTING v7](https://github.com/mblauberg/provenant/blob/1b3d48e4c549811ce3636edc30b047f36fd24ada/docs/specs/rust-port/PORTING.md), especially
R0, R1 and R9, the [rewrite plan](../README.md), and the dependency allowlist in
[ADR 0028](../../../adr/0028-rust-port-dependencies.md). The command surface
below is a proposal for the command-tree freeze in `design/cli.md`.

## Purpose and decisions

`provenant-core` gives every area the same identities, root and path values,
and explicit time values. `provenant-records` owns bounded codecs, canonical
bytes and digest contracts. `provenant-config` resolves typed configuration
and profiles, and installs the binary, skills, MCP registrations and hooks
without claiming the user's other files. Start with fresh state at
`~/.local/share/provenant/v1`, use TOML for Provenant configuration, and keep
runtime records in JSON. Other areas consume these APIs instead of discovering
roots or reimplementing codecs. Core performs no filesystem, environment,
clock, entropy or process access.

## Evidence and feature decisions

The source baseline is commit `1b3d48e4c549811ce3636edc30b047f36fd24ada`.
Read material includes `scripts/lib/`, installation/configuration scripts and
their tests, `skills/_shared/harness_profile.py`, `config/`, `schemas/`, and
the inert Rust in core, records and config. Tests describe hazards to address;
they do not dictate the new interface. There is no top-level `install-harness`
file at this baseline; its implementation is `scripts/install-harness`.

The usage check on 23 October 2026 selected 69 Claude project transcripts and
287 Codex sessions with a Provenant working directory. It counted executable
positions in shell-tool calls, excluding prose, edits, reads and test names:
Claude/Codex respectively ran `install-harness` 4/6 times and `provenant root`
1/13 times; Claude ran the MCP configurer once and binary installer twice;
Codex ran `provenant profile show` once and the builder three times. This is
local observed use, not a fleet-wide absence claim. Reproducible record anchors
(no transcript contents are bundled): Claude session
`1c21e420-fb1c-4a3f-be95-eb42e8489edb` records 256/4354/9263 (install), 4411
(MCP), 308 (publish), 13855 (root); Codex sessions
`01a1625b-3ff9-7d60-8461-04a6be1fc14e` records 69/106/161 (build),
`01a167a6-ebe1-70e2-bdb9-f92539427952` record 279 (root), and
`01a162a9-09dc-7b33-a80a-d57571bd811f` record 498 (profile).

| Keep | Evidence and smaller implementation |
| --- | --- |
| One root resolver and explicit project identity | `tools/front-door/src/resolve.rs`, `skills/_shared/roots.py` and its deliver/implement/session callers need a consistent product/state distinction. Resolve once at the binary edge and pass values. |
| Stable local host identity | `scripts/host_identity.py` is called by Fabric hosts, provider identity and telemetry. Persist a random ID once; a hostname is a display label, not an identity or an alias. |
| Skill ownership, reconciliation and conflict refusal | `scripts/manage_installation.py`, `scripts/install-skills`, the installation tests and `MAINTAINING.md` require preservation of unmanaged entries. Use one per-entry layout and one installation receipt. |
| Source identity and prebuilt validation | `scripts/install-harness` and `scripts/check-provenant-install.py` call `scripts/lib/front_door_build.py`; it already covers workspace members. Keep a complete source digest and bounded smoke check. |
| Six harness registration shapes | `scripts/configure-fabric-mcp.py` and `scripts/install-harness` have concrete Claude, Codex, OpenCode, Agy, Cursor and Kiro callers and tests. Share one planner with format-specific editors; explicit selection replaces auto-detection. |
| Executable profile controls and review settings | `dispatch_run.py`, `provider_exec.py`, `batch_run.py`, `model_route.py` and `model_route_pools.py` consume profile values; code-review and implement skills read the review settings. Preserve the eight settings listed below, even if nobody invokes their editing CLI directly. |
| External YAML subset | Tracked `SKILL.md` and `agents/*.yaml` are harness inputs. Retain the ADR subset, with tracked-file coverage, independent of old PyYAML behaviour. |

| Drop | Evidence and consequence |
| --- | --- |
| Fused/split instance modes, seeded copies of routing defaults, desired-state projections and three-way routing refresh | These exist to maintain two roots and copied catalogues in `instance_installation.py`. Fresh state and compiled defaults plus an explicit user overlay remove that problem. An old `--refresh-routing` invocation establishes an upgrade need, not a need for the merge algorithm. |
| Old root pointers, `AGENTS_HOME`, `AGENT_FABRIC_*` root fallbacks and stale-checkout warnings on every command | They locate Python/TS owners in a mutable checkout (`src/resolve.rs`, `src/stale.rs`, `lib/stale_checkout.py`). The new installed binary executes its own owners. `install check --source` detects source drift explicitly; help/root never runs Git. |
| Whole-directory skill links, `_shared` Python libraries and custom-skill projection/migration | The extra layout and migration branches exist in `_migrate_directory_link` and `install-skills` to accommodate helpers/custom content. One managed link per product skill leaves user skills in place; users install their custom skills themselves. Do not read old receipts to reconstruct ownership. |
| Legacy Claude-agent and Agy receipt cleanup | `_retire_legacy_agents` and `check-legacy-agy` are historical upgrades. R1 forbids an old-state cleanup pass. Leave old files untouched. |
| Advisory-only knobs: `repair.max_cycles`, `council.junior_size`, `writing.final_polish`, `context.summary_tokens`, `quota.pace_tolerance` | All five are marked `advisory` in `config/harness-knobs.json`; their named readers use fixed constants or prose rather than resolved values. Remove the ineffective config surface; domain owners retain real policy. |
| Lenient profile fallback and per-knob environment variables | `harness_profile.lenient/value` can silently substitute defaults after malformed policy; dispatch stamps errors after continuing. Resolve strictly before admission. Pass a typed resolution to routing/run instead of exporting and reparsing `PROVENANT_KNOB_*`. |
| Python/V8 emulation, differential oracles and `pending` copies | These crates are explicitly inert; R0 rejects runtime-language emulation. Unicode tables, surrogateescape ordering, Python errors/repr/numbers, JS coercion and duplicate ordered JSON disappear. |
| Separate installer flags for Python/Node dependency provisioning, JS workflow installation and OTel service installation | `install-harness` provisions runtimes and invokes `install-workflows`/`install-otel-collector`; R2/R5 require one executable and no daemon. Workflow owns retirement/replacement of the JS workflows; telemetry owns collector retirement. This installer installs neither. |
| Automatic disabling of Codex's bundled `skill-creator` | `configure-codex-skill-overrides.py` implements a product preference, not MCP/hook registration. Leave another tool's skill choice to the user; no new override is written. |

Do not drop confinement or custody controls because they lack direct CLI use.
The CLI area decides which hook behaviours survive; this area owns their
registration, safe editing and removal, not their policy.

## Roots, records and configuration

### One state layout

`--state-dir ABS` overrides `PROVENANT_STATE_DIR`, otherwise use the default
above, on both macOS and Linux. Empty/relative explicit values are errors.
There is no XDG or old-root fallback. The binary captures environment and home
once; core resolves supplied values without I/O. Reject a state root equal to
home or filesystem root, equal to or beneath any legacy state root, or equal to or beneath the product checkout. Resolve the
existing ancestor/any symlink aliases before testing containment; when a root
is missing, check the canonical parent plus normal relative tail. Named legacy
roots include `~/.agents`, each selected workspace's `.agent-run`,
`~/.local/state/agent-harness`, `~/.local/state/provenant`, and explicitly
configured old roots. Also reject any `.agent-run` component in either the
requested or canonical state path, even without a selected workspace. Guard
equality as well as descendants; never initialise through a legacy-root alias.
A missing root stays missing for reads, help and `root`; mutating commands
initialise it explicitly with private permissions (directories 0700, files
0600; installed executable 0755).

```text
<state>/
  state.json                       # StateHeader: schema_version=1, HostId
  config.toml                      # optional, user-owned configuration
  install/
    current.json                   # completed InstallReceipt
    transactions/<InstallId>.json  # resumable journal, paths/digests/owned edits
    locks/                         # supervisor-owned OS lock anchors
  releases/<SourceDigest>/         # immutable binary + product skills/assets
    provenant
    assets/
  projects/<ProjectId>/
    project.json                   # canonical repository/workspace binding
    runs/<RunId>/                  # run owner: tasks, attempts, output, scratch
    workflow/                      # workflow owner: delivery evidence
  fabric/fabric.sqlite3            # Fabric owner, with its SQLite companions
  telemetry/                       # telemetry owner: ledger and imports
  quota/                           # quota owner: snapshots and cooldown state
  routing/                         # routing owner: bounded discovery cache
  imports/<ImportId>.json           # source fingerprint and import outcome
  tmp/                             # per-operation scratch, never a global purge
```

Each area owns its children and retention. Nothing writes `.agent-run/`,
`~/.agents`, old telemetry/Fabric stores or harness transcripts. Config never
opens SQLite. Fabric/quota/telemetry choose their own record bodies and DB
schema versions; they use this root and the common IDs/codecs. Locks refer to
one canonical root, so aliases cannot create two writers. No whole-root
sweeper is introduced.

`StateHeader`, `ProjectBinding`, `InstallReceipt`, `InstallJournal`,
`ImportReceipt` and `ProfileStamp` are typed records with integer
`schema_version = 1`, closed enums and `deny_unknown_fields`. Unknown major
versions fail before mutation; missing files have only the documented defaults.
A successful operation never rewrites an unrecognised version. Domain record
owners specify their own extension policy; there is no generic untyped bag.

IDs are distinct Rust newtypes over 16 supplied random bytes, represented as
32 lowercase hex digits, with typed field names rather than a stringly ID
registry. Supervisor supplies entropy; core validates/formats it. HostId is
created under the state initialisation lock and never changes with hostname.
ProjectId is randomly allocated once per canonical Git common-directory
binding; linked worktrees share it but retain separate workspace bindings and
writer leases. Non-Git workspaces bind their canonical directory. Discovery
and locking happen outside core. Rebinding a moved repository is explicit
Fabric/project work, never a heuristic match of remote URLs.

`WallTime` uses UTC integer microseconds and a single `Z` RFC3339 rendering;
`MonoTime`/durations are integer values scoped to a process, never persisted
as cross-process deadlines. Callers provide both clocks. Reject overflow and
invalid timestamps; do not reproduce Python's permissive date parser. Keep
filesystem paths as `PathBuf`/`OsString`. When a durable Unix path needs text,
use a tagged `PathWire` (`utf8` string or lowercase hex native bytes), never
lossy conversion. Provider protocols requiring UTF-8 receive a typed error.
Relative record paths reject absolute components, `.` and `..`; supervisor
performs no-follow, handle-based containment, since lexical checks alone are
not a security boundary.

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
prefix as an identity. Attempt receipts, profile stamps and install receipts
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

### TOML and profiles

One config format: TOML, for compiled product defaults, user overrides and
project overrides. Replace owned `config/*.json` and
`config/adapter-compatibility.yaml` with domain-reviewed TOML sections; domain
owners define routing, prices, graph and delivery bodies. JSON remains a wire
and record format, not an alternative config syntax. Defaults are bundled
with the release, not copied into mutable state on each installation.

Load only `<state>/config.toml` and, when a project is selected,
`<workspace>/.provenant.toml`. No ancestor walk outside the selected workspace,
include directive, interpolation, executable config or automatic old-file
migration. Parse with toml_edit into a validated typed model; no new dependency
or feature expansion is required. All config documents declare
`schema_version = 1`. The top-level optional `profile` selector is an enum
(`balanced`, `economy`, `rigorous`), separate from the eight settings under
`[knobs]`; `[task_classes.<class>]` can name its own `profile` and has an
optional `[task_classes.<class>.knobs]` table.
`config set profile=NAME` edits this selector; other profile keys address
`knobs.<name>`. Domain sections have their own typed fields. Unknown keys,
duplicates, bad types/ranges and unsupported versions fail. Files absent means defaults; unreadable/malformed means error.
`config show` exposes provenance of resolved non-secret settings.

Resolution order, low to high: built-in defaults, the highest selected preset,
user fields, project fields, `PROVENANT_PROFILE`, explicit CLI/request fields.
The environment selector only selects the preset; it does not erase explicit
user/project values. Within each file, the selected task-class section wins
over its general section. Select the highest named preset first, apply its
general/task-class values once, then overlay explicit fields. Request fields
are typed input, never mutated process environment. A project can only set
allowlisted task behaviour and strengthen review; it cannot choose state,
installation paths, peer credentials, executable/provider commands or authority.
Re-resolve review settings without the project and take the stronger result;
apply risk-tier and task-class minima last. A profile cannot grant authority,
lower the review ladder or turn silence into a stop reason.

Keep presets `balanced` (defaults), `economy` (concurrency 4, context ceiling
200,000, weighted pool rotation), and `rigorous` (effort +1, one extra review,
cross-family required, full re-review; no effort increase for mechanical/bulk).
Keep only these settings, with existing useful bounds:

| Setting | Default and bound | Consumer |
| --- | --- | --- |
| `effort.shift` | 0; -1..1; floor 0 for crucial/terminal | routing, constrained by model/task minimum |
| `lanes.silence_seconds` | 300; 1..3600 | supervisor/run observation only |
| `lanes.max_concurrency` | 8; 1..8 | run; cap the requested concurrency |
| `context.ceiling_tokens` | omitted = catalogue; 100,000..1,000,000 when set | providers/run |
| `review.extra_reviewers` | 0; 0..2, added to the ladder | workflow |
| `review.cross_family` | `prefer` or stronger `require`; default `prefer` | workflow |
| `review.rereview_scope` | `delta` or stronger `full`; default `delta` | workflow |
| `routing.pool_mode` | `top` or `rotate`; default `top` | routing |

Omission represents optional defaults in TOML; no string `null` coercion.
A `ProfileStamp` carries selected preset, task class, risk tier, effective typed
values, floor adjustments and their full canonical digest. Provenance is
reported separately and excluded from the behaviour digest. Routing and run
consume the same immutable resolution and stamp it once per attempt.

## Proposed commands and installer behaviour

All commands accept `--json` and global `--state-dir ABS`; `--project ABS`
selects a workspace where applicable. Human output names the resolved paths,
effective values and actions. JSON stdout is exactly one versioned result
object (`schema_version`, `command`, `status`, `data`, `changes`, `errors`);
stderr carries bounded diagnostics only. Changes contain path, owned selector,
operation and safe before/after values. Errors contain stable code and path,
not input excerpts. Exit 0 = successful/no-op, 1 = check found drift/missing
installation, 2 = usage/invalid input or config, 3 = refusal/conflict/I/O,
4 = partial installation requiring recovery. JSON error envelopes use the same
exit code; clap usage errors need no result object. No aliases.

| Command | Flags/input | Result |
| --- | --- | --- |
| `root` | `--kind state\|assets\|project` (default state) | One path, or typed path/identity JSON. Project requires `--project` and prints that selected directory without Git discovery; uninstalled assets is missing. No state creation, SQLite, threads or provider/Git probes. |
| `config show` | `--task-class CLASS --risk-tier TIER --profile NAME --set KEY=VALUE` (last repeatable) | Effective non-secret configuration, per-field origin and profile stamp; safe defaults if files are absent. No raw harness config dump. |
| `config check` | same resolution flags | Validity and bounded errors; does not call providers. |
| `config set` | `KEY=VALUE...`, `--scope user\|project`, optional `--task-class CLASS` | Atomically edits TOML, validates the complete proposal and prints the owned-key changes. User scope default. Values use their declared type; `profile` selects a preset and `knobs.<name>` edits a retained setting. |
| `config reset` | `KEY...`, same scope/class | Removes overrides; prints resulting changes. Unknown keys fail. |
| `config profiles` | none | Presets, retained settings, ranges, minima and consumers. Select a persistent preset with `config set profile=NAME`. |
| `install bundle` | `--source ABS --binary ABS --out ABS` | Verifies the stamped Cargo-built binary against source inputs and publishes a complete bundle directory, refusing an occupied output. No state/harness mutation. |
| `install plan` | `--bundle ABS --harness NAME` (repeatable), `--bin-dir ABS` (default `~/.local/bin`), `--hooks on\|off` (default off), `--adopt-existing` | Validates bundle and selected destinations, prints precise proposed changes/conflicts and a plan digest; read-only. At least one explicit harness required; no `all` discovery. |
| `install apply` | same flags; optional `--plan-digest SHA` | Re-plans under lock, checks supplied digest, previews human changes before publishing, then reports applied/no-op/partial results (JSON emits one final object). Refuses drift from a supplied plan. |
| `install check` | optional `--source ABS` | Compares binary, bundle, owned registrations/hooks/skill links and receipt; source flag additionally checks source digest. Does not repair. |
| `install remove` | `--harness NAME` (repeatable), optional `--hooks-only` | Removes only unchanged owned entries; reports conflicts. Keeps state, release bundles, user skills and config. |
| `install recover` | `INSTALL_ID`, `--action resume\|rollback` | Reports completed and remaining operations; resumes or inverses only operations whose recorded preconditions still hold. No blind restoration of whole files. |

There are no config/install MCP tools. Fabric MCP receives an immutable config
snapshot at server startup; changes apply to the next process/explicit request,
not a polling watcher. CLI owns parser/rendering and hook verbs. Config returns
structured results, not exit codes or strings pretending to be CLI output.

### Build identity and bootstrap

The sole binary does not compile itself during ordinary startup. CI or a
contributor runs `cargo build --release --locked` from `tools/front-door`;
a new Rust workspace `build.rs` computes the source stamp through a shared
pure records digest module also used by install checking. Build scripts may
use allowed workspace build-dependencies; none introduces a second executable
or OS process owner. Cargo is the build tool, not a runtime dependency.

The v1 source digest hashes a sorted list of workspace-relative POSIX names:
workspace/member manifests, Cargo.lock, rust-toolchain.toml, `.cargo/config.toml`
when present, every member's source and build scripts, and bundled assets.
Frame each input with big-endian u64 path length, path bytes, big-endian u64
content length, exact contents. Prefix with `provenant-source-v1\0`. Hash all
members, including unlinked members. Reject symlinked/escaping inputs, duplicate
paths and a missing manifest/lockfile. Ignore target outputs and editor litter;
track the input manifest in the bundle. Product policies, templates and skills
are explicit bundle inputs even when stored outside the workspace. They are
hashed by this same manifest, not an undocumented include-file discovery walk.
Cargo rerun directives cover files and input directories so additions/deletions
also invalidate the stamp; ignore the old `PROVENANT_FD_SOURCE_SHA256` override.

The bundle manifest records source digest, target triple, toolchain, asset
inventory and file digests; binary `--version --json` reports its embedded
source digest and record/config versions. A source match is identity, not a
signature or proof of a trusted publisher. `install plan` verifies manifest,
platform, bytes and executable mode, then supervisor runs bounded
`--version --json` and `help` smoke checks (30 seconds, capped output, no
provider access). An unstamped or mismatched prebuilt binary is refused.
After Cargo, CI or the contributor runs that release binary's
`install bundle --source CHECKOUT --binary TARGET/release/provenant --out DIR`.
The producer verifies the embedded source digest, stages `bundle.json`,
`provenant` and `assets/{skills,library,config,instructions}` from the explicit
input manifest, rejects symlinks/escapes, computes file digests/modes and
fsyncs before publishing the directory without replacement. `bundle.json` is
a versioned records model, not a hand-written CI manifest. Unit/integration
fixtures build a synthetic bundle through this same producer, then feed it to
plan/apply/check. Bootstrap/cut-over invokes the staged release binary's own
`install apply`; no Python builder or shell installer is required.

### Preserve user-owned harness files

Installed command: a regular managed copy at `<bin-dir>/provenant`. Immutable
release assets live under the new state root; receipts replace product-root
pointers. Skills and `library/` skills become per-entry links to release assets;
never link an entire harness skills directory. Existing user instructions are
not seeded or overwritten. Where a harness supports instruction paths, append
only the exact owned asset references. Otherwise report the optional manual
instruction include rather than replace `CLAUDE.md`, `AGENTS.md` or `GEMINI.md`.

The registry below specifies external paths and container shapes, subject to
recorded fixtures from the actual client versions at build time. Harness
home/config overrides are explicit typed install configuration, not an
ambient list of legacy environment fallbacks. The user-only TOML section
`[install.targets.<harness>]` has optional absolute `home`, `mcp_config` and
`hook_config` paths; `[install]` may set `bin_dir`. CLI `--bin-dir` wins for
that target. Explicit configured paths win over captured documented harness
environment (`CLAUDE_CONFIG_DIR` and `CODEX_HOME`), which wins over the table's
defaults; other old override variables are removed. Child defaults derive from
the selected home. A target whose harness has no hook file rejects
`hook_config`. Projects/requests cannot redirect these paths. The plan and
receipt record each resolved path and origin; apply/recovery use those pinned
paths even if the environment changes.

| Harness | Default registration file and owned entry | Hooks |
| --- | --- | --- |
| Claude Code | `~/.claude.json`: `mcpServers.fabric`, `type="stdio"`, command string, args array, env map | `~/.claude/settings.json`: only owned command entries within `hooks.<event>[]` |
| Codex | `~/.codex/config.toml`: `[mcp_servers.fabric]`, `command`, `args`, nested `env` | Register only a fixture-proven hook supported by the target Codex version; otherwise `unsupported`, no invented Claude-shaped hook or `notify` substitution |
| OpenCode | `~/.config/opencode/opencode.jsonc`: `mcp.fabric`, `type="local"`, command array, environment map | No hook registration in the existing owner; report unsupported |
| Agy | `~/.gemini/config/mcp_config.json`: `mcpServers.fabric` | Unsupported until its actual hook contract is recorded |
| Cursor | `~/.cursor/mcp.json`: `mcpServers.fabric` | Unsupported until its actual hook contract is recorded |
| Kiro | `~/.kiro/settings/mcp.json`: `mcpServers.fabric` | Unsupported until its actual hook contract is recorded |

Each registration executes the absolute stable binary with
`fabric mcp --client NAME --state-dir ABS`; no checkout path, Python, Node or
secret environment values. JSON-based clients receive a command string plus
args, OpenCode receives the combined array, TOML receives command/args tables.
The Fabric/CLI freeze must accept these arguments. Hooks execute the same
binary's `hook <kind>` verbs with shell quoting supplied by the CLI owner;
policy, event names, matchers, stdin/stdout and exit semantics come from its
fixture-backed hook registry. `--hooks on` for an unsupported harness refuses
before any writes. A selected Git pre-push hook, if retained by CLI, likewise
requires an explicit owned entry and Git-resolved hook path; never replace a
foreign executable or change `core.hooksPath` to take it over.

Use toml_edit document mutation for Codex and Provenant TOML; retain unrelated
comments, ordering, tables and values. JSON/JSONC uses a small token/span
editor: validate bounded input, locate the owned leaf/container, splice only
that entry (or insert a missing ancestor), and reparse the result. Preserve
all unrelated bytes, including comments and trailing commas. Never strip
comments and serialise the entire user document. Reject duplicate members,
ambiguous container shapes and malformed files unchanged. Keep JSONC syntax
handling in records, not core. No third-party parser dependency is added.

A new entry may occupy an absent slot. A changed/foreign occupied slot fails,
even if it is called `fabric`. First cut-over may explicitly adopt an existing
entry with `--adopt-existing`: print its exact path/selector and safe change,
then record the adopted selector/link as owned. This is confined to the supplied
install targets, never a search/read of old state or legacy receipts. Skills
require a precise existing link target match or remain conflicts; adoption
never deletes a directory or rewrites arbitrary files. At first cut-over the
same explicit adoption also covers the pre-existing stable **regular binary**:
the plan shows its path and full file digest, requires the known old managed
binary marker, and pins that exact file identity. Foreign or symlinked
executables conflict. Before replacement, retain its exact bytes/mode in the
private new install transaction directory and fsync them; journal the recovery
path/digest. Publish only if the identity still matches. Rollback restores
those saved executable bytes only while the current target still matches our
published binary; otherwise it reports a conflict. No old installation receipt
or root is opened. Test the old binary fixture, interruption and concurrent
replacement. Subsequent operations
compare the last installed owned value/link, not a filename substring.
Removal deletes only those values that still match the receipt, preserving
other server entries, user hooks and array order. Retired owned entries follow
the same rule. The installer never edits provider authentication or permission
policy. Credential references are opaque paths; it never opens credential
files, including SSH/AWS/Kubernetes/secret stores. Adoption of an entry carrying
inline secret environment values is refused: the user replaces it manually,
so the recovery journal never becomes a credential store.

All changes are planned before mutation. Human output previews paths and owned operations before publication; JSON
emits one final result with planned/applied changes and partial-state details.
Include before/after values generated by Provenant; redact secret-bearing
fields and never dump a whole user config or credential. A broken report stream
before publishing is an error; a failure after publishing reports partial
state on any remaining usable diagnostic channel and leaves recovery metadata.

Supervisor supplies a lock, bounded no-follow snapshots, metadata identity,
same-directory staging, fsync(file), checked atomic replace/no-replace and
fsync(parent). User config symlinks may be followed only to a regular target
bound by a checked snapshot; preserve the link and recheck its chain before
publishing. Never follow target-directory links opportunistically. A check
followed by ordinary rename is insufficient: preserve and inspect the displaced
file using supervisor's checked exchange primitive. On a concurrent edit,
restore only if the installed identity is still ours; otherwise preserve both
versions in a private sibling recovery directory and report conflict. Do not
lose the editor's version. The journal records recovery paths/digests, not
copies of unowned values; a displaced original is protected like the original
file. Tests must prove this behaviour on Linux and macOS.

Initial state, configuration edits, project bindings, receipts, journals,
skill-link replacements and binary replacement are individually atomic and
fsync'd. Record operation intent durably before the first external write;
mark each operation complete only after publication and parent fsync. Publish
`current.json` last. There is no cross-file or cross-Mac atomicity claim.
Recovery is idempotent, locks out another installer, and checks each operation
against its saved identity/digest before resuming or undoing it. A failed write
or ENOSPC is never silently successful. Never depend on panic/Drop cleanup.

## APIs and ownership boundaries

The following are proposed public interfaces, not compatibility wrappers:

| Crate | API other areas call |
| --- | --- |
| core | `HostId`, `ProjectId`, `RunId`, `TaskId`, `AttemptId`, `InstallId`, `ImportId`; `RootInputs -> Result<Roots>`; `RelativePath::parse`; explicit `WallTime`, `MonoTime`, `DurationMicros`; no ambient discovery or I/O |
| records | `decode<T>(reader, Limits)`, `encode<T>`; `CanonicalV1::bytes`, `record_digest`, `file_digest`, `source_digest`; `PathWire`; `parse_frontmatter`, `parse_agent_yaml`, JSONC token/spans; versioned common envelopes and state/install/profile/import records |
| config | `load(roots, workspace, reader) -> ConfigLayers`; pure `resolve(layers, RequestOverrides, PolicyMinima) -> ResolvedConfig` with field provenance and `ProfileStamp`; `plan_install(Bundle, Targets, Snapshots) -> InstallPlan`; `apply/recover(plan, SafeFs, reporter) -> InstallOutcome`; `check_install`; format editors return proposals without effects |

Config uses supervisor's **safe filesystem/lock API only**, as R2 permits.
The binary composes smoke processes through supervisor; config does not import
process orchestration. Records defines syntax/common record structures;
run/Fabric/workflow own their domain records, transitions and semantic
validation. Do not retain lifecycle, cooldown, storage-deletion or review-gate
logic in records merely because an earlier inert module put it there.

Needed from other areas:

- Supervisor: bounded reads, canonical directory binding, no-follow handles,
  metadata snapshots, entropy/clock capture at the edge, checked atomic
  publication/exchange, fsync and install locks; bounded smoke subprocesses
  with cleanup/custody. Config cannot duplicate these with raw libc or Command.
- Confinement: validated credential/protected-path classification at the
  composition boundary and authoritative policy minima. Project config is
  data, never an expansion of authority.
- Routing/providers/quota/telemetry/workflow: typed TOML sections, defaults,
  validation and explicit units. No crate imports cli; config deserialises
  sections into config-owned types to avoid cycles, and domains validate their
  semantics after resolution. The owning build PR updates the R2 table only
  if responsibility/edges actually change.
- CLI: frozen verbs, result rendering and hook registry, plus a cheap
  version/help smoke interface. Fabric: `fabric mcp` startup arguments,
  project registration/discovery and the bounded legacy message/task importer.
  Telemetry: the legacy ledger importer. Import execution belongs to those
  domain owners, not a new config-to-Fabric/telemetry dependency.

## Import, fixtures and verification

### Read-only import and two-Mac cut-over

R1's only legacy reads are explicit one-shot imports of Fabric messages/tasks
and the telemetry ledger. Proposed domain commands are
`fabric import --from ABS --dry-run` and `telemetry import --from ABS --dry-run`,
with `--apply` instead of `--dry-run` for publication and the common JSON/exit
contract. CLI/Fabric/telemetry owners freeze the final spelling. No importer
runs at startup or install, scans a home directory, reads transcripts/config,
or reconstructs active processes, claims, leases, sessions or quota state.

Import opens only the named allowed store read-only, with no schema updates,
WAL checkpoint or creation of source journal/SHM files. For SQLite require a
quiescent self-contained source or an operator-supplied consistent snapshot;
if a live WAL needs writable reader sidecars, refuse rather than modify it or
use `immutable=1` against a changing database. Domain importers validate rows
under bounds and stage destination publication transactionally. Namespace old
IDs by source fingerprint; retain source identity/time as provenance rather
than silently recanonicalising host aliases. Completed receipts deduplicate
`(kind, source fingerprint, old row ID)`; changed sources require a new import.
Reject invalid rows before publication and report counts. Success on one store
never asserts success on the other. Config supplies roots, codecs and receipts.
Old runs and output stay where they were, unreferenced by new live state.

The chairs perform cut-over only after fake-provider dispatch, Fabric MCP and
all retained hooks pass from isolated roots on **both** Macs:

1. Build the same accepted source revision for each target; verify bundle
   digests. Each host gets its own fresh root/HostId. Do not copy absolute
   machine paths or a state directory between Macs.
2. Quiesce old dispatches/MCP clients without killing healthy providers; retain
   the old install and state. On each Mac run the staged binary's `install plan`
   with explicit harnesses and any deliberate adoption; resolve conflicts
   before applying. No lane runs this on the stable install.
3. Apply on each Mac, publish the stable binary after assets/registrations are
   ready, and finish the receipt last. Report each host separately: a failure
   on Mac two is a partial rollout, not a fictional distributed transaction.
4. Restart clients, verify version/install check, one fake dispatch, MCP
   handshake and retained hook events. Then run only deliberately requested
   imports. No live provider run is implied by these checks.
5. Roll back registrations with checked inverses if needed and reinstall
   `pre-rust-rewrite` as the rewrite plan specifies. Old state remains intact;
   new state is retained for diagnosis, never copied back into old stores.
   Resolve intervening user edits manually rather than restore whole configs.

### External contracts and recorded fixtures

No provider CLI wire is implemented here; providers owns recorded argv/env and
stream/result fixtures. No new fixture may be an old Python/TS result used as
a differential judge. The build must record sanitised real harness behaviour
before enabling each registration/hook contract; existing scratch fixtures are
useful failure cases but are not proof of a current external protocol.

Place recordings in `tools/front-door/tests/fixtures/contracts/` with a small
metadata JSON giving client/version, platform, capture procedure, redactions
and expected parsed fields. Capture only allowlisted structure from isolated
homes and fake MCP executables; never copy a real credential-bearing config.

| Contract | Required recordings/assertions |
| --- | --- |
| Six harness config formats | Each client's accepted local MCP registration and observed stdio launch argv/env; Codex TOML comments/inline tables, OpenCode JSONC comments/trailing comma, unrelated entries and config symlink. Unsupported versions refuse; no claim all six are already recorded. |
| Claude hooks and any other enabled hook | Real client event stdin, allowed/no-op/deny stdout and exit result; client config acceptance. CLI supplies exact PreToolUse/SessionStart and any retained other event forms. Absence of a Codex fixture means no Codex hook install. |
| MCP | Real `initialize`, `notifications/initialized`, `tools/list`, shutdown/EOF and cancellation recordings owned by fabric-mcp; prove installed command/args starts that server and stdout contains only JSON-RPC. |
| Agent Skills | Parse every tracked `SKILL.md` frontmatter and `agents/*.yaml` sidecar via repository fixtures. Current frontmatter is name/description; seven tracked OpenAI sidecars use interface metadata. Add hand-labelled subset acceptance/refusal cases. No PyYAML oracle. |
| Git | Recorded `rev-parse` forms for primary/linked/non-Git workspaces and resolved hooks path; run/confinement owns safe Git execution. No assumption `.git` is always a directory or repo identity equals a remote URL. |

Unit tests alongside modules prove ID/path/version validation, exact integer
limits (including unknown JSON fields), duplicate refusal, canonical bytes,
digest domain separation, YAML grammar/refusals/bounds, TOML precedence,
project strengthen-only rules, risk floors, profile stamp equivalence and
format-editor preservation. Use explicit clocks, entropy bytes and injected
snapshots; test invalid transitions in install journals and every typed error.

Binary integration tests run the real command tree in isolated homes/state,
with a fake bundle/provider/MCP peer: first install, no-op reinstall, source
and asset drift, foreign skills, adopted old registrations, malformed config,
all six formats, unsupported hooks, broken stdout, check/remove/recover,
read-only imports and two independent host roots. Byte-compare unowned file
spans and the legacy fixture tree before/after; test help/root without SQLite,
state creation or provider access. Use `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/..."))` for repo fixtures and canonicalise temporary
paths on macOS.

Process tests run in CI on Linux/macOS with FIFO/handshake barriers: concurrent
installers, a user edit between snapshot and exchange, config symlink swap,
crash at every journal/publication boundary, ENOSPC/fsync/rename failure and
resume/rollback. Assert original user bytes survive, one writer wins, no
unowned link is removed and no healthy provider receives a signal. Every
spawn goes through supervisor. No sleep-based synchronisation or CPython/V8
oracle. Locally run fmt, clippy and tests for touched crates; CI runs the
workspace, remaining subject-bearing Python/TS tests and macOS release build
per R9. This design-only PR requires link/whitespace and scope checks, not a
local full harness run.

## Deletion ledger and build order

Counts are physical lines at the source baseline above (`wc -l`), including
comments/tests. These are build-lane deletions, not changes in this design PR.
Every listed replacement lands with its old subject/tests removed in the same
PR; update surviving skill/hook/docs callers then, without forwarding adapters.

| Slice | Python source/helper | Lines |
| --- | --- | ---: |
| 1 | `scripts/lib/roots.py` | 33 |
| 1 | `skills/_shared/roots.py` | 45 |
| 1 | `scripts/host_identity.py` | 102 |
| 1 | `scripts/lib/jsonc.py` | 68 |
| 1 | `tools/front-door/crates/core/scripts/generate_unicode.py` | 29 |
| 2 | `skills/_shared/harness_profile.py` | 403 |
| 2 | `scripts/harness_profile.py` | 212 |
| 3 | `scripts/lib/front_door_build.py` | 249 |
| 3 | `scripts/lib/product_root_resolver.py` | 107 |
| 3 | `scripts/write-product-root-pointer.py` | 27 |
| 3 | `scripts/lib/stale_checkout.py` | 76 |
| 3 | `scripts/install-provenant-command.py` | 272 |
| 3 | `scripts/managed_installation_manifest.py` | 175 |
| 3 | `scripts/manage_installation.py` | 850 |
| 3 | `scripts/instance_installation.py` | 811 |
| 3 | `scripts/install-workflows` (Python) | 315 |
| 4 | `scripts/configure-fabric-mcp.py` | 826 |
| 4 | `scripts/configure-claude-hooks.py` | 134 |
| 4 | `scripts/configure-codex-skill-overrides.py` | 140 |
| 4 | `scripts/check-provenant-install.py` | 165 |

| Slice | Deleted Python test | Lines |
| --- | --- | ---: |
| 1 | `tests/test_host_identity.py` | 129 |
| 1 | `tests/test_product_root_single_resolver.py` | 59 |
| 2 | `tests/test_harness_profile.py` | 445 |
| 3 | `tests/test_front_door_build.py` | 342 |
| 3 | `tests/test_product_root_resolver.py` | 151 |
| 3 | `tests/test_split_root_contract.py` | 147 |
| 3 | `tests/test_stale.py` | 204 |
| 3 | `tests/test_instance_installation.py` | 931 |
| 3 | `tests/test_install_provenant_command.py` | 272 |
| 3 | `tests/test_install_skills.py` | 1,311 |
| 4 | `tests/test_configure_fabric_mcp.py` | 1,310 |
| 4 | `tests/test_check_provenant_install.py` | 358 |
| 4 | `tests/test_install_harness.py` | 2,427 |

Python total: **13,125 lines**. Shell entrypoints removed in the corresponding
slices: `scripts/provenant-profile` **5**, `scripts/install-skills` **87**,
`scripts/install-harness` **702**; shell total **794**. The installation test
module also tests collector/workflow/other hook subjects: those domain lanes
must land their replacement tests or retire their subject before slice 4
removes it; do not carry unrelated old tests into Rust config.

No TS file is wholly owned/deleted by this area. The dependent lanes delete
these mixed files with their own consumers, without double-counting them here:
Fabric's `runtime/fabric/src/identity.ts` **196** and `run-root.ts` **96**;
supervisor/Fabric's `process-identity.ts` **392** and associated TS tests.
Likewise run owns `skills/orchestrate/scripts/layout.py` **245** and
`tests/test_exec_layout.py` **158**, `attempt_evidence.py` **107** and
`tests/test_attempt_evidence.py` **83**; run/supervisor owns
`attempt_storage.py` **176**. Fabric/routing owns `fabric_records.py` **647**
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
`config/harness-knobs.json` (118 lines), replacing its executable registry with
typed settings/preset defaults. Domain owners replace the remaining policy
files in their TOML-default PRs: routing `model-routing.json` (1,090),
`model-preferences.json` (17), `adapter-compatibility.yaml` (306); run/Fabric
`graph-defaults.json` (8); telemetry `telemetry-prices.json` (319); workflow
`delivery-profiles.json` (109), `security-evidence.json` (42), `risk-policy.json`
(69). They report those data-file line changes separately. No owned YAML config survives
its domain's TOML replacement PR.

Replace/prune the inert crate source trees: core **6,624**, records **3,181**,
config **8,197** Rust lines (18,002 total). This includes all `pending`,
`oracle`, `test_support`, core `py/*`/`js.rs` and their tests. Keep only useful
new-design code; delete misplaced policy copies rather than preserve old API
shapes. The language shims/oracle runner alone account for 4,084 Rust lines;
the Python generator's 29 lines are already counted above. Planning estimate:
about **6,000 Rust lines including replacement tests**, replacing these 18,002
and deleting 13,125 Python/zero TS lines: expected Python+TS+Rust net
**-25,127** (including shell, **-25,921**). This is an estimate, not a build
quota; each PR reports its actual `Net lines:` and explains growth. Domain
moves count once in their landing PR, not twice in subsystem estimates.

Land these PR-sized slices in dependency order:

1. **Values and codecs.** Land the narrow core and records APIs, versioned
   root/layout records, canonical/digest contract and YAML/JSONC readers with
   their tests. Delete shared root/host/JSONC Python owners and emulation;
   remove dependent old tests whose subject is gone. Consumers awaiting their
   own rewrite wait; no Python adapters. Keep workspace Rust green by updating
   Rust callers to the new values. Supervisor's real filesystem API follows
   before any config mutation lands.
2. **Typed config and profiles.** Land strict TOML loading/resolution and the
   config subtree, replacing `config/harness-knobs.json` and the profile scripts/tests. Land domain default
   TOML sections with their owning lanes, deleting their JSON/YAML config in
   those PRs; do not implement multiple live config formats. Expose resolved
   values to run/routing/workflow before those lanes build their owners.
3. **Bundle and managed files.** Land source stamping, bundle verification,
   install journal/recovery, skill reconciliation and staged binary publication;
   delete builder, pointer, instance/manifest/skill/workflow installers and
   their tests. This is isolated-root build/test work, never a stable install.
4. **Harness registration.** Land fixture-backed TOML/JSON/JSONC editors,
   MCP/hook registry integration and install plan/apply/check/remove/recover;
   delete remaining installation entrypoints/tests and update docs/skills.
   Domain lanes replace shared test subjects before the aggregate module goes.
5. **Domain imports and cut-over gate.** Fabric and telemetry land their
   read-only importers and delete their old stores' implementation/tests;
   record no-source-write tests. Chairs then run the two-Mac acceptance and
   cut-over above. Installation is a final authorised release operation, not
   a design/build lane action.

Each build PR remains open for native diff review, chair/cross-family review
and exact-head CI; lanes do not merge or arm auto-merge.
