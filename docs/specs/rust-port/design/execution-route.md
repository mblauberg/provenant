# Execution: routing, providers and quota

Status: proposed for chair acceptance under [PORTING v7](../PORTING.md).
Author route: `codex/gpt-6.1-sol@high` via Fabric.
The [cross-area decisions](README.md#cross-area-decisions) bind this design.

Provider step 1 status: Agy `--print-timeout` handling remains a gap. The current
Rust `finish(ExitObservation)` has no artifacts/stderr input, so it cannot
recognise the timeout diagnostic and preserve answer text as partial as the
current product does. A qualifying activation recording must establish that
stderr/exit/answer combination and the finalisation input before Agy activation;
no timeout inference is implemented from exit status alone.

`routing` resolves explicit model choices and named routes; `providers` prepares
CLI invocations and interprets provider evidence; `quota` schedules current
allowance reads and supplies admission advice. Run owns the registry, atomic
admission/operation insertion, attempts, retries and publication. Supervisor owns
process custody and signalling; confinement owns disclosure and access grants.
These libraries add no daemon, third-party dependency or second execution registry.

## Evidence and feature decisions

Source/count baseline: `1b3d48e4c549811ce3636edc30b047f36fd24ada`.
The council's 24 October audit includes Claude and Codex transcript stores,
Codex JavaScript exec calls and MCP calls. Its reproduction scripts are
`route_{usage,tally,exec}.py` in the council's private scratch directory
(not committed).
Counts describe observed requests, not successful execution or global usage.
Transcripts and run recordings are research inputs only, never runtime state.

| Evidence | Decision |
| --- | --- |
| 1,116 `fabric_submit` calls, 1,170 tasks; councils 0, rotate 0, model lists 2, fallback 2, pools 0 | Keep explicit models, efforts and bounded explicit fallback. A council is a caller-supplied list of distinct exact models, expanded into ordinary tasks by run/Fabric; no council selector or filling algorithm. |
| `strong` 73, `bulk` 24, `junior` 1 | Historical tier names, including pre-rename names on 14–15 October (`model_route_pools.py:83`); no tier aliases. Named routes are one small TOML table. |
| CLI `--council` 3 and `--pool` 2 in two sessions; `route resolve` 8 in one session; `select` once as a bug reproduction | Drop pools, weights, rotation, the selection store, `--commit-selection`, preferences and inferred class/size policy. Keep pure `route list\|resolve`. |
| Real probe, cooldown, account-pin and endpoint use 0 | Drop public capability/cooldown verbs, canary inference, account pins and endpoint profiles. Required adapter preflight and quota reads remain internal effects. |
| Confidential true 1,155 times, never false | Confidentiality is always on. No flag or unsafe fallback. |
| Kiro dispatches 0; no v3 recording | Drop Kiro, alongside dormant Copilot/Pi/Herdr execution. Keep Claude, Codex, Cursor, Agy and OpenCode v2 only when fixture-backed and confined. |
| Council observed the running `dev.provenant.quota-menubar` LaunchAgent | Keep `quota show\|tick`. [QuotaModel.swift:123](../../../../apps/quota-menubar/Sources/QuotaMenuBar/QuotaModel.swift#L123) calls `quota tick --trigger TRIGGER --power POWER --json`; the [app README](../../../../apps/quota-menubar/README.md) defines its LaunchAgent. |

Also drop hidden model classification, shadow stages, the routing-context hook,
`extract`/Ajv, old-tier migration, typo guessing, manual credit/history/forecast
commands and OpenCode's speculative spend estimator. Preserve current quota
windows/reset advice and refusal backoff, without cooldown mutation commands.
Provider quota pools are measured allowance scopes, not routing pools.
No old transcript quota/identity reads, general home mirroring or Python shim
survives. Confinement retains only a bounded safe Agy home recipe.

## Resolution and command contract

`RouteRequest` has exactly one `Model(ADAPTER/ID)` or `NamedRoute(NAME)`,
optional effort and `Fallback::None | Models(BoundedModels<2>)`, default none.
IDs retain the full provider/model path after the first adapter separator.
A route name expands to one exact model and optional effort from TOML; it does
not choose a tier, pool, account or native seat. No inferred adapter, model
upgrade or hidden downgrade. An explicit unknown ID remains unverified and can
run only if activation, disclosure and confinement are independently established.

Resolve once against immutable catalogue, provider availability and quota
snapshots. Validate activation/executable and access, filter confidentiality and
eligibility, apply recorded effort support, then return one `ResolvedRoute` with
at most two ordered explicit alternatives and exclusion reasons. Run never
reconstructs candidates. Read-only fallback needs explicit opt-in and a typed
transient failure within run's retry budget; every alternative passes the same
checks. Writers never replay automatically. Unknown quota is not exhausted;
stale data cannot create a fresh refusal. Unsupported effort fails explicitly;
any permitted clamp is recorded with requested/applied/observed values separate.
No inferred effort default. OpenCode transports effort as `model#variant`.

There is no `NativeSeat` or host-adapter avoidance. Fabric supplies identity and
placement; native subagent selection is outside this dispatch API. Every council
member is an explicit distinct model task. Workflow decides whether verified
family facts satisfy its review policy; distinct model IDs alone prove no
cross-family diversity. **Never enable fast or priority service.** Codex sends
`service_tier="default"` and rejects conflicting inherited overrides.

The CLI freeze is `route list|resolve` and `quota show|tick`. All use cli's
`{schema_version,command,status,data,error}` envelope, 1 MiB cap and exits from
the cross-area table: 0 ok, 1 failure/refusal/findings, 2 invalid input/version,
3 setup/I/O/contention, 124 wait deadline, 130 cancelled. There is no exit 4.
Partial coverage and input-required belong in `data`; a satisfied wait exits 0.
Redacted diagnostics go to stderr. No library chooses exits or renders JSON.

| Command | Inputs and result |
| --- | --- |
| `route list` | Optional `--adapter ADAPTER`; catalogue revision, exact models/efforts, named routes and cached availability with reasons/age. No probe. |
| `route resolve` | Exactly one `--model ADAPTER/ID` or `--route NAME`; optional `--effort VALUE`, `--fallback ADAPTER/ID[,ADAPTER/ID]` (omitted means none). Returns `ResolvedRoute`, alternatives and reasons; pure, no admission or state mutation. Dispatch uses the same route fields. |
| `quota show` | Optional `--adapter ADAPTER`; cached windows/reset/source/age, coverage and `report_path`. No refresh. |
| `quota tick` | Optional `--adapter ADAPTER`; `--trigger timer\|panel\|manual\|wake\|power` (default manual), `--power ac\|battery\|low` (default ac). Refresh due sources, publish the current report, return per-source results and next due without sleeping. |

Requests are `RouteListQuery { adapter }`, `RouteRequest`, `QuotaQuery { adapter }`
and `QuotaTick { adapter, trigger, power }`; cli's reconciled table freezes the flags.
Route preview uses a read-only context; dispatch supplies its separately
validated access/confinement context to the same resolver.
Every verb accepts `--json`. No account, confidential, native, task-class,
endpoint, pool, council, probe, cooldown or selection-commit flag remains.
Tick reports partial stale/unknown coverage in data, exiting 0 for a completed
partial report; a wholly failed refresh uses the typed failure mapping above.
The Swift consumer moves to the new root/report and envelope at cut-over,
removing its old-CLI fallback. No new MCP tool is owned here: Fabric dispatch
and status call the same APIs, with bounded waits from the cross-area table.

## Records, state and writes

Config owns one user `S/config.toml`; bundled catalogue and adapter data are
TOML assets. `[routing.routes]` maps names to `{ model = "ADAPTER/ID", effort =
"VALUE" }` (effort optional). `[quota]` holds refresh scheduling settings;
adapter activation uses typed config keys over the bundled adapter table.
No profile layering, project overlays, per-knob environment variables,
preferences or extra config files. Unknown owned fields/versions fail closed.
`S` is the fresh host state root from the cross-area table, never an old root.

| Path relative to S | Version 1 contents |
| --- | --- |
| `routing/availability/<adapter>.json` | `AvailabilityRecord`: executable/version, activation and fixture evidence, exact observed IDs, catalogue revision, account label, observation/expiry and last preflight error. |
| `quota/<account-key>/<adapter>.json` | Last good windows, source/observation time, reset/refusal scope, refresh revision, claim/deadline and backoff. Account keys are opaque and host-local. |
| `quota/report.json` | Current account rows, coverage, generated time, refreshing IDs and next due. No ledger/history. |

Providers own account identity: each adapter detects its own account label;
routing owns eligibility; run records the resolved label. Labels are non-secret,
never emails, token substrings or raw auth paths. Quota scopes readings by the
providers-supplied identity; config neither detects nor pins accounts.
Account detection failures remain unknown/auth-required, never guessed identities.

### Account identity and placement (IDENT)

The provider identity owner shares quota-live's Claude fingerprint: the first
16 hex digits of SHA-256 over the trimmed, lowercase email. A rotating
`~/.claude` is identified by that fingerprint, never by the directory name.
Codex hashes `tokens.account_id` from its existing `auth.json` sign-in data;
Cursor hashes the subject claim from `cursorAuth/accessToken` in its existing
read-only `state.vscdb`. These opaque IDs retain case. Their keys have `codex-`
and `cursor-` prefixes. Neither tokens nor token fragments are fingerprinted.
An unreadable identity produces `unknown`, including explicitly configured
default entries. Quota capture checks the identity before and after the probe
and refuses a changed attribution. Configured names remain display aliases.

`providers::identity::Accounts` registers detected keys at canonical config
directories. Routing normally selects the account from provider availability.
The optional per-task `account` field in a dispatch manifest or Fabric submission
is an override, not a new command-line flag or a named-route account pin. It
filters the requested model's adapter, or the named pool's adapters, before
quota/readiness resolution, preserving other adapters' explicit fallbacks. It
never switches accounts because of quota; #1115 remains separate.

Provider preparation validates the selected registration against the resolved
route. Claude's `refresh_for_account` brackets credential loading with status
probes and binds the returned token to that fingerprint and directory. A token
from another registration is refused, as is a bound token without its
registration. Keyed Claude/Codex routes require a registration.
`CLAUDE_CONFIG_DIR` selects the directory;
only that session's transcript paths receive write grants. Codex retains clean,
attempt-private `CODEX_HOME`, linking only the selected registration's
`auth.json`, with exact read/refresh-write grants to that file. It does not grant
the whole config directory. These narrower grants and private Codex state follow
the execution/confinement design over #1114's broader directory wording.
Lane rows expose the applied account, or `unknown` when none was applied.

Current acceptance boundary: production `cli::run::Registry::prepare` still
refuses launches at the existing unwired launch-specification seam. The typed
registration/preparation APIs and concurrent Codex stubs are covered, but durable
registration discovery, launch-time Codex identity revalidation and end-to-end
production placement are not complete. The concurrency test asserts generated
grants and uses unconfined stubs; it does not establish OS-enforced isolation.
#1114 stays open until that composition work and its confinement acceptance run.

#1062 also remains open. Static inspection of the official Agy 1.4.7 macOS
binary and matching call sites in the installed executable found Keychain
service `gemini` and product-constant account arguments (`antigravity`, with a
`jetski-cli` alternative). Those attributes do not establish a signed-in Google
identity. The normal backend's complete caller chain was not established; no
live Keychain or secret was read. A metadata-derived per-user fingerprint has
therefore not been implemented. Existing `google-<12 hex>` log attribution
remains in quota-live; a supported non-secret provider identity source is needed
before closing the cooldown/routing work. Evidence: `LoadStoredToken` at
`0x101aaf580`, `NewCLITokenStorage` at `0x102692520`, and `Daemon.SeedToken` at
`0x102c2e400` in the inspected darwin-arm64 build. The service string is at file
offset `0x33aa5e5`. These are static executable observations, not a claim that
runtime account switching was tested.

Stored snapshots are typed, versioned serde records. Edge-built
`VerifiedLineage`, `ReviewAvailability` and `AdmissionPermit` facts implement
`Serialize` only. Persist separate `AvailabilityRecord`, `FamilyEvidenceRecord`
and run-owned `PermitRecord` projections (`Serialize, Deserialize`), never nested
fact types. Permit records bind operation/attempt, quota snapshot revision and
expiry. On recovery the owning edge validates stored evidence/revision/expiry
before reconstructing a fact; stored bytes cannot assert fresh availability or
admission. Records owns shared value types and bounded codecs; domain crates
own validation. Provider envelopes may have extra fields but required
fields, duplicate keys and sizes are checked. Integers stay i64/u64, percentages
use basis points and money uses checked scaled integers; no integer through
f64. Inject core clocks; use jiff for reset interpretation and reject ambiguous
local times.

Quota returns typed update intents, applied by the CLI/run composition through
supervisor's safe filesystem API: bounded lock, re-read revision, atomic replace,
file/parent fsync; 0700 directories and 0600 files. Reserve a refresh claim with
token/deadline, release the lock, execute, then commit only the matching revision.
No lock spans a provider wait. Expired claims cannot leave permanent refreshing
rows; stale completion cannot overwrite newer data. Keep last good data on
failure and label its age. Failed writes are typed errors, never provider stops.
There is no route selection store or rotation transaction.

Quota statuses are available, credit-only, exhausted, stale, unknown,
auth-required and unsupported. Refusals remain model/account-scoped unless wire
evidence proves wider scope; independent allowance pools do not exhaust each
other. A newer comparable observation or reset expires only that scoped refusal;
unknown data preserves it. Unknown reset has a one-hour backoff. No pin bypass.
OpenCode `provider.quota` decodes its structured response body (JSON or a JSON
string); `FreeUsageLimitError` supplies the shared `opencode-free` pool and its
provider message, without searching stderr. The admission rule matches this
pool across models only for the same account identity and a supplied pool
binding. Production launch still needs to persist these events and supply those
bindings. The quota tick's macOS Claude refresh calls the shared provider helper
from the binary composition edge; quota itself cannot import providers because
providers already depends on quota. The credential-file path on other platforms
remains separate because that helper currently reads Keychain only.

Tick TTLs (timer/panel seconds): Claude 300/120, Codex 900/300, Cursor 1800/600,
Agy 1200/900. Manual has a 30 s floor and respects backoff. Battery doubles
intervals; low power quadruples them and skips Agy. Network/429 backoff is
120–3600 s, auth 3600 s, missing CLI 86400 s; Retry-After is a lower bound capped
at 86400 s. At most three cheap probes concurrently, Agy alone; each has a
deadline and 1 MiB response bound. Tick's wall budget is at most 55 s.

## Shared APIs and provider registration

The API table matches run's route, admission and launch types. Methods run on
scoped handles carrying immutable inputs and clocks. The provider signatures
below abbreviate `Result<T, ProviderError>`; failures are always typed.

| Owner / consumer | Contract |
| --- | --- |
| routing / run | `resolve(&RouteRequest, &RoutingContext) -> Result<ResolvedRoute, RoutingError>`; primary plus at most two explicit alternatives, effort, eligibility, readiness and confidentiality. |
| quota / run | `admit(&ResolvedRoute, &OperationId) -> Result<AdmissionPermit, QuotaError>`; provider/account readiness only. Run stores its `PermitRecord` projection and consumes the validated permit for the exact attempt with its own resource reservation; no second admission ledger. |
| providers / run | `prepare(PrepareInput) -> LaunchSpec`; incremental `observe(bytes) -> ProviderEvent` and `finish(ExitObservation, artifacts) -> ProviderOutcome`; exact continuation/session support, no context-ceiling input. |
| providers / workflow composition edge | `validate_provenance(&RouteProvenance) -> VerifiedLineage`; exact observed ID and catalogue revision key every fact. Adapter preflight validates activation, executable, fixture and confinement readiness, account label, source/time/expiry and unknown/unavailable reasons. |
| routing / workflow composition edge | `review_availability(&RoutingSnapshot, &ReviewDemand) -> ReviewAvailability`; cached tier/family coverage, expiry and quota/config/capability/family-slot blockers. No probes under a receipt lock. |

`VerifiedLineage` contains the exact observed model ID, catalogue revision,
`family: Option<FamilyId>`, evidence reference and provenance. Only provider
protocol evidence verified against that catalogue revision establishes a family;
never infer it from model spelling, a broker name, requested ID or seat.
Absent/mismatched evidence yields unknown and cannot satisfy cross-family review.
Workflow may use ChairAttested provenance for same-family legs only. Facts are
built at the composition edge from providers-verified evidence, never deserialised
from caller claims. Availability reports are bounded, paginated projections of validated cached
`AvailabilityRecord`s; list/resolve never starts a process to populate them.

One `Adapter` trait in providers exposes account detection, preflight
planning/decoding, `prepare`, stream state/`observe`, `finish`, continuation
and verified lineage.
One `QuotaSource` trait exposes `probe(&AccountScope, &QuotaProbeInput) ->
Result<QuotaProbePlan, QuotaError>` and `decode(&QuotaProbePlan, &ProbeCapture)
-> Result<QuotaSnapshot, QuotaError>`. `QuotaSource`, `QuotaProbeInput`,
`QuotaProbePlan`, `ProbeCapture`, `QuotaSnapshot` and `QuotaError` live in quota;
`ResolvedRoute` is a records data type. Records owns no adapter traits.
Confinement constructs validated `ConfinedPlan` values without a
records-to-confinement dependency. Providers implements both traits in one
module per provider and exports one static registry of `{ adapter, quota_source? }` entries;
the binary injects providers' registry as `&[&dyn QuotaSource]` into quota. No per-crate
adapter switches or quota-to-providers dependency. Adding an adapter changes
one provider module, one registry entry and one bundled TOML row with its
fixture/activation requirements. Config validates policy; registration cannot
bypass it. Quota owns scheduling/admission, not adapter wire decoding.

`LaunchSpec` holds OsString argv (canonical executable first), cwd, the complete
child environment, stdin, setup steps, the provider confinement profile and the
decoder state, in memory.
Persist only a redacted launch summary. Bound argv prompts to 64 KiB; prompt
retention is run's confined artifact contract. `Continuation` binds exact
adapter/session/account/host/model and sent controls; reject incompatible grants
or identity changes. Confinement freezes Codex clean/inherited policy and denies
unconfined hooks/MCP/plugins for read-only execution or fails closed. It supplies
Agy's private home recipe and conflict-safe copy-back; providers adds no syscall.

Observation decoders use documented event positions, never recursively search
tool/subagent output for answering identity. Emit assistant text, usage,
session/model evidence, refusal/reset, terminal or malformed-protocol facts.
Structured errors outrank claimed success; answer prose mentioning quota is not
a refusal. Recovered transport retries clear transient evidence. Terminal answer
and process exit are separate facts. OpenCode's exit-after-output exception
requires recorded terminal evidence; Agy timeout text remains partial. Providers
never signals a process or publishes run completion.

Core/config supply roots, typed IDs, `ActorId`, `CancelToken`, `WorkScope`,
SHA-1/SHA-256 `GitOid`, clocks as values and immutable policy; confinement supplies
`ConfinedPlan`. Supervisor executes all launch/preflight/quota effects, locks
and safe reads; these crates never call `Command::new` or own raw handles.
Run owns custody handoff, continuation, retries, home finalisation and publication.
Telemetry reads run's terminal facts; quota does not read/import telemetry.
Workflow receives family/availability facts through the composition edge.

## Credentials and external contracts

**R7/R8 invariant:** quota credentials are read into memory only, never logged,
written, captured to a file, placed on argv or put in a child environment;
drop token/raw-response buffers after the probe. The owning provider's
`QuotaSource` implements extraction/decoding, quota schedules it, and supervisor
performs bounded I/O/process effects. No generic secret resolver or new source.

| Source retained from current product | Owner and execution rule |
| --- | --- |
| Claude selected config directory's `.credentials.json` (`claudeAiOauth.accessToken`), otherwise macOS keychain `Claude Code-credentials` (existing config-path hash suffix for non-default directories) | `providers::claude`; `security find-generic-password -s ITEM -w` through supervisor, with **memory-only stdout and stderr capture**, bypassing ordinary attempt capture/spooling even on error/overflow/cancel. File bytes use a bounded memory-only read. |
| Cursor `User/globalStorage/state.vscdb`, `ItemTable` key `cursorAuth/accessToken` | `providers::cursor`; read-only fixture-shaped query into memory, no database copy, auth-store export or raw row in diagnostics. |

Runtime access requires recorded user authority identifying actor, exact source,
purpose and expiry, checked before reading; retain only its non-secret authority
reference with the probe result. This design permission preserves the existing
sources; it grants no live credential read in this revision. Tests use fake
`security` and fixture databases, never the real keychain or credential files.
No Agy keychain, additional account store or endpoint token source is added.
Codex and Agy quota use their CLI protocols, not direct auth-store reads.

Claude/Cursor HTTPS uses fixed allowlisted URLs
`https://api.anthropic.com/api/oauth/usage` and
`https://cursor.com/api/usage-summary`: system `curl --config -`, header only on
stdin, no redirects, verbose or trace, bounded time/body and system trust/proxy
behaviour (ADR 0028). Responses containing credentials use memory-only capture;
only decoded non-secret quota facts may be stored. No raw request/response log.

External argv comes from the existing adapters, not a new command convention.
Square brackets below denote optional segments, not literal argv. Confinement
adds only its verified permission/config arguments; no raw caller argv.

| Adapter | Current wire form and controls to fixture |
| --- | --- |
| Codex (enabled) | `codex exec [resume ID] --json --skip-git-repo-check --ignore-user-config --ignore-rules --dangerously-bypass-approvals-and-sandbox -c service_tier="default" -m MODEL [-c model_reasoning_effort="EFFORT"] -`; prompt on stdin. Environment `HOME USER PATH TMPDIR LANG CODEX_HOME SHELL`, plus `CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR` for read-only work; read-only `PATH=~/.cargo/bin:/usr/bin:/bin`, writer `PATH=/usr/bin:/bin`; `CODEX_HOME` is the attempt's private state, where setup links `auth.json` to the selected registered identity's `auth.json` (the default `~/.codex/auth.json` when unselected). Codex's own seatbelt cannot nest inside ours, so it is off and the outer profile is the boundary. No fast/priority tier or context-ceiling override. |
| Claude (enabled) | `claude -p --safe-mode --strict-mcp-config --disable-slash-commands --permission-prompts none --output-format stream-json --verbose (--session-id NEW\|--resume ID) --permission-mode dontAsk\|acceptEdits --tools Read,Grep,Glob[,Edit,Write] --model MODEL [--effort EFFORT]`; prompt on stdin. Environment `HOME USER PATH TMPDIR LANG CLAUDE_CODE_TMPDIR DISABLE_AUTOUPDATER CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC CLAUDE_CODE_OAUTH_TOKEN` only, plus `CLAUDE_CONFIG_DIR` when an account is selected; the access token is read and bound to that selected identity by the host. No shell tool, no keychain access, no exec beyond the CLI. |
| Cursor | `cursor-agent -p --output-format stream-json --trust --approve-mcps --mode ask --sandbox enabled [--resume ID] [--model MODEL] PROMPT`; prompt on argv and open non-TTY stdin. Existing writer `--force` needs its own confined recording; read-only acceptance does not enable it. |
| Agy | `agy --output-format stream-json --disable-slash-commands --print-timeout Ns [--sandbox] [--conversation ID] [--model MODEL] [--effort EFFORT] [--add-dir DIR] --print PROMPT`; boundary text prefixes argv prompt, stdin null. Existing writer `--dangerously-skip-permissions` requires its own confined recording. |
| OpenCode v2 (prepared, disabled) | `opencode run --standalone --format json --log-level error [-s ID] -m PROVIDER/MODEL[#VARIANT]`; prompt on stdin, no selected authentication (keyless models only). `HOME` is the attempt's private state, so data, config, caches and every home-relative lookup stay inside it; `PATH=/bin`, so no Git runs. `OPENCODE_CONFIG_CONTENT` denies bash and webfetch, allows edit only for writers, and sets no MCP servers, plugins, sharing or autoupdate; project config, Claude Code files and the file watcher are disabled. The standalone server is a forked copy of the CLI reached over loopback. No v1/shared-server fallback. |

Read-only Codex recipes grant read and exec of `~/.cargo/bin` and
`~/.rustup/toolchains`, plus read of `~/.rustup/settings.toml` and
`~/.cargo/registry`. Setup creates a private `CARGO_HOME` inside the attempt's
`tmp/`, linking only its `registry` to the existing cache. `CARGO_TARGET_DIR`
and executable test output stay inside that private temporary directory.
`RUSTUP_HOME` selects the existing toolchains. No read or write of
`~/.cargo/credentials*`, no whole-Cargo-home grant, and no toolchain writes are
added. These recipes require the named toolchain/cache directories to exist;
missing required anchors refuse prelaunch.

OpenCode's discovery recipe grants literal directory reads of `.agents`,
`.claude` and `.codex` along cwd ancestry. These reads never grant their child
files or enable OpenCode without activation evidence.

### Live activation

`providers::prepare` returns a `LaunchSpec`: exact argv (`argv[0]` the canonical
executable), the complete child environment, stdin, cwd, setup steps (a 0700
directory or one symbolic link) and the provider confinement profile. The
launcher performs setup, holds `required_anchors(plan, profile, worktree)`,
compiles the plan with the profile and runs `ConfinedLaunch::command(argv)`.
A mode runs only when `tests/fixtures/providers/activation.json` enables it;
each adapter's `confinement.json` holds the probed observations per access mode.

`cargo run -p provenant-providers --features recording --example record --
ADAPTER read_only|worktree_write --model M --root EMPTY_DIR [--write]` builds a
probe repository (and a worktree for writers), runs the CLI fresh then resume
under the compiled profile with a clean environment, then probes every claimed
observation under the same rules: declared and undeclared reads and writes,
secret and protected paths, provider endpoint versus other network, declared
and undeclared exec, fork, authentication and runtime reads, state writes and,
for writers, worktree, index, HEAD, objects, selected ref, config and other-ref
writes. Writers must also create a file through the model's own edit tool.
`--write` stores redacted streams as `<adapter>/live-<access>-<session>.jsonl`,
their manifest entries in `<adapter>/recordings.json` and the observations in
`confinement.json`. Session ids, paths under `HOME` or the root, emails,
identifiers and token-shaped strings are substituted. Credentials are opened
only by the CLIs, except Claude's access token (below); probes read zero bytes. A test fails if an enabled mode cites
evidence from another access or session mode, so read-only evidence never
enables a writer.

Decisions recorded with the first activation (Codex 0.159.0, Claude 2.1.310,
macOS 27.0.1 arm64, 2026-10-24):

- Confinement carries a fixed runtime baseline (global metadata reads,
  preferences, trust, DNS and directory services, `/dev` nodes, zoneinfo).
  Secret and protected denials still override it.
- Exec roots grant exec only, never reads. Readable install files come from
  the CLI's own package (`package_root`: the parent of its `bin/`); a shared
  prefix such as `/opt/homebrew` or `/usr/local` is refused, so its `etc/` and
  `var/` stay unreadable.
- Codex: refreshing its token rewrites `~/.codex/auth.json` in place through
  the link, so its authentication profile carries that one exact write. It
  reads and execs its own package (code-mode host, bundled `rg` and `zsh`);
  its shell tool may also run programs in `/bin`, `/usr/bin` and
  `/usr/libexec`. Residual risk: that tool can read Codex's own `auth.json`.
  It cannot read other credentials: the login keychain file is not readable,
  and a probe showed `security` finds nothing in a keychain the profile cannot
  read.
- Claude: the keychain file is not readable. Before launch the host calls
  `claude::refresh_if_expiring` for the config directory it was given (the
  default `~/.claude` reads `Claude Code-credentials` for `USER`; another
  directory reads that service with its path-digest suffix and sets
  `CLAUDE_CONFIG_DIR`, so accounts never cross). Stdout is captured in memory
  and never logged or stored; only `accessToken` and `expiresAt` are kept, and
  the token is passed as `CLAUDE_CODE_OAUTH_TOKEN`. If it expires within 15
  minutes the host first runs the CLI once outside the sandbox (`claude -p x
  --model provenant-refresh-probe --max-turns 1`: the CLI refreshes its stored
  token, then the API rejects the model, so no usage is spent) and reads
  again, accepting a token with at least a minute left. That refresh is not
  confined, so it runs with every customisation off (`--safe-mode
  --strict-mcp-config --settings {"disableAllHooks":true}
  --disable-slash-commands --no-session-persistence`, not `--bare`, which
  skips the keychain) in an empty scratch directory. A probe with a
  `SessionStart` hook in the working directory's `.claude/settings.json` fired
  without these flags and did not fire with them. The model still gets no
  shell, only file tools.
  `~/.claude.json` is not readable (it can hold MCP secrets and the launch
  does not need it). The only writable state is this session's transcript
  under `~/.claude/projects`.
- Expired Claude sign-in (#1260): the sandbox never refreshes or writes the
  keychain; the host refreshes first. Not reproduced on 2026-10-24 (the token
  was valid). A token the API still rejects decodes as `authentication_failed`,
  so the outcome is `Refused` (`claude/live-auth-required.jsonl`) and the user
  signs in again outside the sandbox.
- Codex continuation needs no model evidence because its exec stream carries
  none; the resume argv pins the same `-m`. Adapters that report a model must
  have reported the requested one.
- Provider endpoints are `*:443`: Seatbelt cannot filter by host name.
- OpenCode: the standalone server is a child process on a loopback port, so
  its profile adds `loopback_server` (bind, accept and connect on `localhost`
  only) and must carry a `LoopbackServer` observation. That grant also reaches
  other local services; accepted because bash, webfetch, MCP and plugins are
  off. The confined launch answered on 2026-10-24 (v2.0.32,
  `opencode/glm-5.3-flash-free`), but its stream ended after the text event
  without `step_finish`, which decodes as `Partial`; the same happened
  unconfined whenever Git was off the search path. The free quota then ran
  out, so no mode is enabled and no capture was stored. Next step: confirm
  whether `step_finish` needs Git (snapshots); if it does, grant one Git
  executable and its libraries rather than decode a stream without a terminal
  event as success.
- Agy and Cursor keep refusing (`launch::unrecorded`) until they have a launch
  form and recordings. Preflight and availability records are still
  unimplemented, so routing reports enabled adapters as `UnknownAvailability`
  until the run lane supplies them.

Each capture manifest records source provenance, CLI version/platform, redacted
argv, fresh/resume and access mode, exit and redaction rules. It must prove the
exact selector/effort/default service controls and confinement form being enabled.
Keep session/answering-model positions distinct from tool/subagent models;
retain success, refusal, recovered retry, malformed/partial and terminal/exit
cases. Listing seeds in `tests/fixtures/model-listings/` support listing only;
`fabric-v1/provider-*.json` is synthetic. Quota recordings need windows/resets,
missing data, 401/429 and Retry-After; never auth-store bytes. Fake inputs cover
negative cases missing from recordings without claiming real CLI acceptance.

## Tests and acceptance (R9)

Unit tests inject clocks, immutable config and evidence. Cover exact selectors,
route expansion, bounded explicit fallbacks, always-on confidentiality, effort
support, family evidence/revision mismatch, availability age and account scope.
Replay recordings through actual decoders for nested identity, recovered retries,
terminal/exit combinations and bounded malformed input. Quota tests distinguish
unknown/zero/stale, independent allowance pools, resets/DST, Retry-After and
revision/claim expiry. Delete Python/V8 coercion, differential and old-output
judges; types replace redundant validators. Use an in-house snapshot helper
(about 80 lines), without `insta`.

Every binary integration/process test sets `HOME`, `PROVENANT_STATE_DIR`,
`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
`XDG_STATE_HOME`, `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` to temporary directories.
Inject temporary Cursor and Agy homes (including macOS Application Support and
Agy's shim/state directory); forbid passwd-derived real-home/keychain fallback.
Put fake `curl`, `security` and all provider CLIs on PATH. Absolute system-tool
paths are injected too, so `/usr/bin/security` cannot escape the fake. No test
reads/writes the old roots, stable install or real harness config.

Drive real CLI/MCP rendering: no process on help/list/show/resolve, exact
envelope/exits/size cap, tick partial coverage and revision races, failure keeping
last good data. Fake curl/security assert token absence from argv, environment,
files and diagnostics, including overflow/cancel/error; only curl stdin receives
the header. Fixture databases cover read-only auth extraction and expired tokens.
Update Swift report decoding/fixtures with the new report path and envelope.

Linux/macOS handshake/FIFO fixtures, never sleeps, cover late probe completion,
competing refresh writers, ENOSPC, output overflow and home/copy-back races.
Supervisor proves custody/cancellation; run proves one writer, atomic admission
and terminal publication. These crates prove routing policy and wire decoding.
Bookkeeping failure cannot signal a healthy provider or lose terminal evidence.
Compare canonicalised macOS temporary paths. Local checks are fast targeted
checks; CI runs workspace/process and macOS release checks. No live smoke.

## Deletion ledger

Counts are `wc -l` at the source baseline, including comments and blanks.
Whole-file removals below are counted only by this area; deletion lands in
the last-importer step below, including coupled run/Fabric PRs. No adapter bridge.
Mixed files are assigned separately so two areas do not claim the same saving.

| Python source removed | Lines |
| --- | ---: |
| `scripts/model_route.py` | 1,614 |
| `scripts/model_route_catalog.py` | 279 |
| `scripts/model_route_cli.py` | 471 |
| `scripts/model_route_live.py` | 404 |
| `scripts/model_route_pools.py` | 702 |
| `scripts/model_route_preferences.py` | 567 |
| `scripts/model_route_probe.py` | 404 |
| `scripts/task_classifier.py` | 190 |
| `scripts/utility.py` | 241 |
| `skills/orchestrate/scripts/cf_dispatch.py` | 969 |
| `skills/orchestrate/scripts/fabric_records.py` | 647 |
| `scripts/provider_identity.py` | 133 |
| `skills/orchestrate/scripts/capabilities.py` | 374 |
| `skills/orchestrate/scripts/exec_routing.py` | 370 |
| `skills/orchestrate/scripts/provider_output.py` | 548 |
| `skills/orchestrate/scripts/adapters/__init__.py` | 17 |
| `skills/orchestrate/scripts/adapters/agy.py` | 31 |
| `skills/orchestrate/scripts/adapters/claude.py` | 65 |
| `skills/orchestrate/scripts/adapters/codex.py` | 313 |
| `skills/orchestrate/scripts/adapters/copilot.py` | 34 |
| `skills/orchestrate/scripts/adapters/cursor.py` | 22 |
| `skills/orchestrate/scripts/adapters/kiro.py` | 48 |
| `skills/orchestrate/scripts/adapters/opencode.py` | 112 |
| `scripts/provider_quota.py` | 1,611 |
| `scripts/quota_schedule.py` | 326 |
| `scripts/quota_display.py` | 399 |
| `scripts/quota_providers/agy.py` | 162 |
| `scripts/quota_providers/claude.py` | 193 |
| `scripts/quota_providers/codex.py` | 174 |
| `scripts/quota_providers/cursor.py` | 78 |
| `scripts/quota_providers/opencode.py` | 182 |

| Tests and Python helpers removed | Lines |
| --- | ---: |
| `tests/test_model_route.py` | 4,790 |
| `tests/test_model_route_pools.py` | 986 |
| `tests/test_task_classifier.py` | 107 |
| `tests/test_cf_dispatch.py` | 2,905 |
| `tests/cf_dispatch_support.py` | 31 |
| `tests/test_utility.py` | 154 |
| `tests/test_fabric_records.py` | 816 |
| `tests/test_capabilities.py` | 404 |
| `tests/test_actual_effort.py` | 284 |
| `tests/test_provider_quota.py` | 2,188 |
| `tests/test_quota_schedule.py` | 195 |
| `tests/test_quota_display.py` | 580 |
| `tests/support/quota.py` | 54 |
| `apps/quota-menubar/Fixtures/make_fixtures.py` | 103 |
| `tools/front-door/crates/quota/src/tests/oracle.py` | 84 |

Whole-file Python total: **25,361 lines removed**. Execution-route also deletes
`scripts/utility-schema.cjs` (16 JavaScript lines) with utility below.
Fabric owns/counts
`catalogue.ts`, `pools.ts` and `catalogue.test.ts` (642); CLI owns/counts
`routing_context_hook.py` (85). These 727 lines are excluded here.

| Data, schemas, help and docs removed (not code savings) | Lines |
| --- | ---: |
| `config/model-routing.json` | 1,090 |
| `config/model-preferences.json` | 17 |
| `schemas/model-routing.schema.json` | 129 |
| `schemas/model-preferences.schema.json` | 97 |
| `schemas/quota-snapshot.schema.json` | 590 |
| `schemas/adapter-receipt.schema.json` | 881 |
| `tools/front-door/src/help/route.txt` | 6 |
| `tools/front-door/src/help/routes.txt` | 6 |
| `tools/front-door/src/help/quota.txt` | 6 |
| `tools/front-door/src/help/capabilities.txt` | 4 |
| `skills/orchestrate/references/routing-and-tiers.md` | 301 |
| `skills/orchestrate/references/tiered-council.md` | 72 |

Also remove `scripts/model-route` (15), `scripts/provenant-quota` (5),
`config/adapter-compatibility.yaml` (306; replaced by bundled TOML), and seven
synthetic `tests/fixtures/fabric-v1/provider-*.json` (455 total).
Keep real listing/stream recordings; they are not deletion savings.

Remove these old golden judges with their subject: quota in build step 2,
route/routes/capabilities/extract in step 3. Shared golden runner/manifest/tests
belong to CLI's final golden retirement; remove these registrations there.

| Golden fixture removed | Lines |
| --- | ---: |
| `tests/golden/cases/route/route-case-01.json` | 12 |
| `tests/golden/cases/route/route-case-02.json` | 12 |
| `tests/golden/cases/route/route-resolve.json` | 13 |
| `tests/golden/cases/routes/routes-case-01.json` | 12 |
| `tests/golden/cases/routes/routes-case-02.json` | 12 |
| `tests/golden/cases/routes/routes-catalog.json` | 12 |
| `tests/golden/cases/capabilities/capabilities-case-01.json` | 12 |
| `tests/golden/cases/capabilities/capabilities-case-02.json` | 12 |
| `tests/golden/cases/quota/quota-case-01.json` | 12 |
| `tests/golden/cases/quota/quota-case-02.json` | 12 |
| `tests/golden/cases/extract/extract-case-01.json` | 12 |
| `tests/golden/cases/extract/extract-case-02.json` | 14 |

| Shared file | Lines at baseline | Deletion boundary |
| --- | ---: | --- |
| `skills/orchestrate/scripts/provider_exec.py` | 2,760 | Run/supervisor deletes the whole file once its lifecycle lands; adapter builders/observations above replace only the provider portion. |
| `tests/test_provider_exec.py` | 9,272 | Run/supervisor removes this suite with that subject, after new custody/process tests exist. |
| `skills/orchestrate/scripts/agy_home.py` | 268 | Confinement/run deletes the file after the safe home recipe and supervisor shim replacement land. Providers supplies adapter policy, not syscalls. |
| `runtime/fabric/src/execution-input.ts` | 610 | Fabric/run replaces the mixed file. Delete route selector helpers and `routeArguments` here; do not count the entire file twice. |

This area owns whole-file deletion of `cf_dispatch.py`, `fabric_records.py` and
their tests, after run/Fabric replace their final callers with typed APIs in a
coupled deletion PR. `provider_identity.py` moves to providers account detection
and is counted above. Run owns surviving `test_dispatch_run` cases; CLI owns
installation/front-door assertions and routing-hook registration removal.
Shared fake-CLI/module helpers survive until their final caller is removed.
CLI removes the output-digest hook and its test before utility's deletion.

The three existing crates contain **18,275 Rust lines** (routing 8,230,
providers 4,519, quota 5,526), including inline tests. Replace these owners,
retaining useful algorithms but discarding all old wire/format emulation.
Whole-file emulation deletions include routing `casefold.rs` (2,329),
`pending.rs` (424), `ordered_json.rs` (121), `oracle.rs` (116); providers
`pending.rs` (171), `ordered_json.rs` (278), `oracle.rs` (135),
`dispatch_contract.rs` (385), `exec_contract.rs` (192); quota `pending.rs`
(1,048), `pending/python_unicode.rs` (749), `oracle.rs` (114) and the obsolete
`policy_tests.rs` (1,553). Remove every inline differential/oracle test and
Python formatting helper in the rewritten modules as well. The quota Python
oracle's 84 lines are already in the Python table, not this Rust count.

Planning ceiling: **4,000 new Rust lines**, including parsers, policy and
unit/binary integration tests. Removal totals
**25,361 Python + 18,275 existing Rust = 43,636 lines**. At the ceiling,
expected net Python/TS/Rust change is **−39,636 lines or lower**. This excludes
the 16 JavaScript lines, mixed-owner removals, data/schemas, shell, help/docs
and new real recordings.
Each build PR reports measured `Net lines:`; the ceiling never weakens fixture,
confinement or custody acceptance.

## Build order

0. **Recording PR before implementation.** Promote qualifying existing on-disk
   recordings with provenance/redaction manifests and an activation matrix.
   Missing recordings keep the corresponding adapter/mode disabled; no live
   provider run. This is a concrete prerequisite for step 1.
1. **Provider interfaces and registry.** After core/records/config/confinement
   and quota's trait/data API, land the providers-owned `Adapter`, static
   registry, five provider modules and fixture-backed parsers/account detection.
   Delete providers' Rust emulation and only old leaves/tests whose importers
   are all retired in this area; shared Python waits for the matrix gate.
   Disabled wire forms have no pending implementation. Confinement
   and supervisor land memory-only secret capture before quota auth is enabled.
2. **Quota.** Land scheduling/admission, current snapshots/report intents and
   fake-curl/security/database tests. Delete quota Rust emulation/oracles, old
   golden judges and only Python leaves/tests with no outside importer.
   `provider_quota.py` and its dependencies wait for the matrix gate.
   Wire `quota show|tick` and update Swift report/fixtures together. A tick drops the snapshot of an account that detection no longer reports; there is no forget operation. Runtime secret reads still require
   recorded user authority; fixtures never grant it.
3. **Routing.** Land explicit model/named-route resolution, at most two explicit
   alternatives, family/availability facts and route CLI. Delete routing Rust
   emulation, old golden judges and only leaves/config/schemas/help/docs whose
   importers/callers are all retired in this area. Shared routing Python waits
   for the matrix gate. No preferences, rotation or pool engine.
   Execution-route deletes `utility.py`, `test_utility.py` and `utility-schema.cjs`
   in step 3, which removes `task_classifier`'s utility calls, gated on their last
   importer. This includes CLI removing utility verbs and the output-digest hook.
   CLI removes the routing hook and its registration; Fabric deletes
   its own catalogue/pools, without counting them here.
4. **Composition and final callers.** Run/Fabric consume the shared APIs and own
   atomic admission, attempt records, retries and publication. Couple deletion
   of `cf_dispatch.py`/`fabric_records.py` and their tests/helpers with replacement
   of their final importers; this area counts those files once. Run owns the
   run-directory contract cut and `git_evidence_packet`; its `AttemptReader::attempt` facts replace
   workflow receipt readers. No new scaffold or import is added here. Finish
   isolated fake-provider dispatch/MCP acceptance and remove shared leaves only
   when their last old caller is gone.

Steps 1–3 land Rust primitives; they do not delete a still-imported Python leaf.
R2+R3, R4 and R5 below name the numbered steps in execution-run's build order:
**Durable dispatch and admission + Bounded batches**, **Controls and recovery
library**, and **Facade, run-directory cut and shared leaves**. Execution-route
alone counts/deletes the following leaves and subject tests in those coupled
PRs; outside owners remove their callers first. Recheck direct, dynamic and CLI
callers before deletion; a surviving importer moves the deletion later.

| Leaf | Importers/callers and deletion gate |
| --- | --- |
| `provider_output.py` | `dispatch_run`/`dispatch_preflight` (R2+R3), `provider_exec` and `provider_observer` (R4+R5), plus `cf_dispatch`/`fabric_records`' transitive provider use. Delete in R5 with the last facade/provider caller. |
| `provider_identity.py` | `provider_quota`, `quota_providers/*`, `telemetry.py` and `fabric_records`. Delete in R5 after telemetry and quota remove their imports and Fabric retires `fabric_records`. |
| `exec_routing.py` | `dispatch_run`/`dispatch_preflight`/`batch_run` (R2+R3), `provider_exec` (R4+R5), `provider_output`, `utility.py` (route step 3) and `fabric_records`. Delete in R5 after all retire. |
| `provider_quota.py` | `exec_routing`, `provider_exec`, `model_route_live`, `telemetry.py` and quota-menubar's fixture generator. Delete in R5 after these and old quota consumers retire; quota leaves stay until this gate. |
| `capabilities.py` | `dispatch_run`/`dispatch_preflight`/`batch_run` (R2+R3), `provider_exec`, `provider_confinement`, Codex's old adapter and `cf_dispatch`. Delete in R5 with the last facade/provider caller. |
| `model_route_live.py`, `model_route_probe.py` | `model_route.py` exposes both to `provider_exec` and `exec_routing`; preflight, `utility.py`, `cf_dispatch` and `fabric_records` use that router directly or dynamically. Delete in R5 with the last router caller, not route step 3. |
| `utility.py`, `test_utility.py`, `utility-schema.cjs` | Execution-route step 3 removes `task_classifier`'s utility calls and deletes all three, after CLI removes utility verbs, the output-digest hook and all remaining direct callers/tests. No run/workflow deletion claim. |
| `cf_dispatch.py`, `fabric_records.py`, their tests and `cf_dispatch_support.py` | R2+R3 removes dispatch/preflight/batch callers; R5 replaces Fabric's facade and last direct callers. Delete in R5; subject helpers retire with their last test. |

Each build PR lands Rust and deletes its subjects/tests together; no Python→Rust
bridge, selector or old-output oracle. Chairs arrange cross-family review and
accept/merge on CI. This lane updates the existing design PR and does not merge
or arm auto-merge.

## Lane D quota freshness and Claude expiry (2026-10-24)

Quota reports retain last-known windows and their observation age. Any failed
refresh makes the retained snapshot stale immediately, even inside its ordinary
poll interval. Human output prints each account, `STALE`, `last known`, age,
window percentages and a sanitised failure code. JSON retains the original
snapshot and exposes current freshness at the account row, rather than rewriting
the historical observation. A previously recorded plan name is not evidence of
the current subscription after an account upgrade.

The installed Claude CLI's `auth --help` advertises login, logout and status;
`auth status --help` advertises JSON/text status only. Status success is not
proof that usage was refreshed. The one renewal Provenant runs is the
zero-cost refresh before a Claude launch (see Live activation); it never copies
credentials between `.claude-<label>` identities, writes credentials itself,
or widens a sandbox's keychain access. Expired identities need renewal through that selected
Claude identity outside the utility sandbox before a supported quota probe can
replace the last-known reading. Current Rust quota sources remain disabled
without their activation recording and credential authority.

Claude's structured result field `api_error_status = 401` is authentication
failure, including an expired OAuth token inside confinement. It now becomes
`AuthRequired` rather than a generic error or successful answer. Provider text
mentioning expiry is ordinary text and cannot assert an authentication failure.
This classification permits an explicitly authorised cross-adapter retry without
weakening confinement. Utility execution and hosts-doctor composition still need
their run/Fabric owners; a credential's existence alone never proves it can be
refreshed in an SSH session with a locked or unavailable keychain.

The cut-over request also restores model-backed `digest`, `classify` and
`extract`, superseding their earlier removal decision. Restoration is still
blocked on execution composition: current Run exposes persistence/read APIs
only, every provider's `prepare`/`preflight` returns `Disabled`, and both launch
plan types are uninhabited. Confinement's `utility` module is a closed Git
boundary, not an inference runner. Bypassing Run through a CLI-owned process
would lose custody/confinement guarantees. No placeholder command or unused
retry seam is added.

The utility execution owner must consume an ordered, explicit cross-adapter
candidate list and retry only `auth_required`, `usage_limited` or `unavailable`:
Claude utility model, Codex `gpt-6-luna` at low effort, an eligible recorded
OpenCode free model for non-confidential input, then a recorded Agy Gemini Flash
model. Do not retry the same Claude route as a junior fallback. Each candidate
needs its own disclosure, availability and confinement checks; confidential
input must never reach a free model without an explicit non-training guarantee.
A sandboxed 401 takes this route-around path rather than receiving wider auth
write/keychain access. Exact selectors and controls still require activation
evidence; a requested model name cannot fabricate that evidence.
