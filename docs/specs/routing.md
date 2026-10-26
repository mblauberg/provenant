# Routing, providers and quota

## Purpose and ownership

This spec owns how a dispatch becomes one exact provider model: route
resolution, the provider adapters that build and decode launches, and the
quota reads and refusals that decide whether a model is ready. The
[cross-cutting decisions](README.md#cross-cutting-decisions) bind it.

| Crate | Owns |
| --- | --- |
| `routing` | The catalogue, pools, request validation and `resolve`. |
| `providers` | One module per adapter: account detection, launch preparation, stream decoding, continuation and verified lineage. |
| `quota` | Refresh scheduling, persisted windows and refusals, and the admission check. |

Run owns the registry, admission, attempts, retries and publication
([run](run.md)). Supervisor owns process custody and signalling, and
confinement owns disclosure and access grants ([supervisor](supervisor.md)).
These crates add no daemon, no second execution registry and no process
spawning of their own: supervisor executes every launch, preflight and quota
effect.

## Boundaries

- Routing never starts a process. `route list` reads the bundled catalogue
  and `[routing]` config; `route resolve` also reads cached quota snapshots.
- Providers never signal a process or publish run completion. Terminal answer
  and process exit are separate facts.
- Quota never imports providers. The binary injects the providers' registry of
  quota sources into quota.
- Telemetry reads run's terminal facts; quota neither reads nor imports
  telemetry.
- Workflow receives family and availability facts through the composition
  edge, never from caller claims.
- Adding an adapter changes one provider module, one registry entry and one
  bundled TOML row. Config validates policy; registration cannot bypass it.
- Quota provider pools are measured allowance scopes, not routing pools.

## Contract

### Route request and resolution

A request names exactly one `--model ADAPTER/ID` or `--route NAME`, with an
optional effort and an optional fallback of at most two explicit models
(default none). IDs keep the full provider path after the first adapter
separator. No adapter, model upgrade, effort default or downgrade is inferred.
An unknown explicit ID is unverified: it runs only if executable availability,
disclosure and confinement are independently established.

Named routes come from the bundled catalogue or `config.toml` `[routing]`:

- A personal route maps a name to one exact model and optional effort.
- A pool route draws among ready members that allow the effective effort. The
  weight of a member is multiplied by its provider's pacing factor, and the
  draw is seeded from the operation ID and task index, so a retry picks the
  same model without stored rotation state. Weight 0 marks a member that is
  fallback only, tried in list order.
- Pacing is per provider: `pace` scales quota headroom against the remaining
  window and clamps the factor to 0.25 to 2.0; `spend` and `max` stay at 1.0.
  Free-tier providers use `max`.
- The bundled pools are `utility`, `junior`, `senior`, `principal`, `writing`
  and `design`. Their members, weights and efforts are catalogue data; read them
  with `provenant route list`. The names are exact route names, not aliases.
- The receipt records each candidate's weight, factor, readiness, the roll and
  the pick. When no member is ready, resolve reports the first eligible
  candidate with its readiness blocker.

Resolution runs once against immutable catalogue, availability and quota
snapshots: validate the executable and access, filter on confidentiality and
eligibility, apply recorded effort support, then return one resolved route with
at most two ordered alternatives and exclusion reasons. Run never rebuilds
candidates.

- Fallback is read-only only. It needs explicit opt-in and a typed transient
  failure within run's retry budget, and every alternative passes the same
  checks. Writers never replay automatically.
- Unknown quota is not exhausted, and stale data cannot create a fresh refusal.
- Unsupported effort fails explicitly. A permitted clamp records the requested,
  applied and observed values separately. OpenCode carries effort as
  `model#variant`.
- Confidentiality: a confidential request admits only models whose catalogue row
  says they do not train on prompts. The test is training, not price. Free-tier
  models are excluded unless the request sets `--allow-free`.
- Fast or priority service is never enabled. Codex sends
  `service_tier="default"` and rejects conflicting inherited overrides.
- There is no native-seat or host avoidance. A council is the caller
  submitting distinct exact-model tasks and synthesising in the chair. Distinct
  model IDs alone prove no cross-family diversity; workflow decides that from
  verified family facts.

### Tier policy

This is routing preference, not authority:

- Bounded work with an objective gate goes first to a free non-training model,
  then to subscription juniors, with paid pools last. Multi-file or ambiguous
  work goes to senior; cross-cutting, crucial or terminal work to principal;
  scouting and mechanical edits to junior.
- A failed gate escalates one tier after at most one repair attempt.
- The resolved model is always recorded in the receipt. A departure from tier
  preference never waives authority, confinement, confidentiality or assurance
  gates.

The orchestrate skill's routing references own the working guidance.

### Commands

All commands accept `--json`, use the envelope, 1 MiB cap and exit codes in
[cli](cli.md), and write redacted diagnostics to stderr. No library chooses
exits or renders JSON. `provenant help route` and `provenant help quota` own
the flag lists.

| Command | Behaviour |
| --- | --- |
| `route list` | Catalogue revision, exact models and efforts, pools with configured weight and pacing policy. No probe; dispatch applies pacing and decides readiness. |
| `route resolve` | Pure preview of one `--model` or `--route`, with `--effort`, `--fallback`, `--confidential` and `--allow-free`. No admission or state mutation. Dispatch uses the same fields. |
| `route set`, `remove`, `reset`, `pacing` | Edit user pool overrides in `config.toml`: add or update a member (weight 0 to 100, effort), remove one, restore a bundled pool, set a provider's pacing policy. |
| `quota show` | Cached windows, reset, source, age and coverage. No refresh. |
| `quota display` | Set the menu bar headline window and icon account, then show quota. With no options nothing changes. |
| `quota tick` | Refresh due sources, publish the report, return per-source results and next due without sleeping. `--trigger timer\|panel\|manual\|wake\|power`, `--power ac\|battery\|low`, optional `--adapter`. |
| `accounts register`, `list`, `set` | Register a provider account directory after validating sign-in; list non-secret records; set how an account appears in the menu bar. |

The quota-menubar LaunchAgent calls `quota tick` and reads the published
report ([app README](../../apps/quota-menubar/README.md)). Tick reports partial
stale or unknown coverage in `data` and exits 0 for a completed partial report;
a wholly failed refresh uses the typed failure mapping. No MCP tool is owned
here: Fabric dispatch and status call the same APIs, with the bounded waits in
the cross-cutting table.

### Records and state

Config owns one user `config.toml` under the state root; the catalogue and
adapter data are bundled TOML assets. Unknown owned fields or versions fail
closed. Every adapter with a launch form is enabled by default.

| Path under the state root | Contents |
| --- | --- |
| `quota/<account-key>/<adapter>.json` | Last good windows, source and observation time, scoped refusals, refresh revision, claim and deadline, backoff. Account keys are opaque and host-local. |
| `quota/report.json` | Current account rows, coverage, generated time, refreshing IDs, next due. No ledger or history. |

Stored snapshots are typed, versioned serde records. Facts built only at the
edge (verified lineage, review availability) are `Serialize` only. Persist
a separate family-evidence projection and revalidate it on recovery; stored
bytes never assert fresh availability or admission.
Percentages are basis points, money is checked scaled integers, and no integer
passes through `f64`. Clocks are injected; reset times use `jiff`, and
ambiguous local times are rejected.

Writes are typed update intents applied through supervisor's safe filesystem
API: bounded lock, re-read revision, atomic replace, file and parent fsync,
0700 directories and 0600 files. A refresh reserves a claim with a token and
deadline, releases the lock, runs, then commits only against the matching
revision. No lock spans a provider wait. Expired claims cannot leave a
permanent refreshing row, stale completion cannot overwrite newer data, and
failed writes are typed errors that never stop a provider.

### Accounts

Providers own account identity. Labels are non-secret and never emails, token
fragments or raw auth paths; an undetectable identity is `unknown`, never
guessed. Routing owns eligibility and run records the resolved label.

Registration and quota probes run in a fresh 0700 `probe-<id>` directory under
the host state root; directories left by owners that died are swept.
A probe directory that cannot be created is reported as a host state fault,
not an identity change.

- Claude: the first 16 hex digits of SHA-256 over the trimmed, lowercase email.
  A rotating `~/.claude` is identified by that fingerprint, not its directory.
- Codex hashes `tokens.account_id`; Cursor hashes the subject claim of its
  access token. Neither fingerprints token text. Keys carry `codex-` and
  `cursor-` prefixes.
- Agy fingerprints the signed-in Google email from its probe log, with an
  `agy-google-` prefix. An undetectable identity stays unavailable rather than
  inheriting a shared `default` cooldown.
- Quota capture checks identity before and after the probe and refuses a
  changed attribution.
- Registrations live in `config.toml` under
  `[domains.accounts.entries.<adapter>-<label>]` (adapter, display label,
  detected key, canonical directory). `accounts register LABEL --adapter
  claude|codex --directory PATH` validates sign-in first. A Claude label binds
  exactly to `~/.claude-<label>`. A Claude registration also stores an
  organisation fingerprint and refuses with "changed identity; re-register it"
  when stored and probed organisations are both present and differ; an absent
  probe organisation never refuses. Codex with no registration uses its
  effective `CODEX_HOME`, otherwise `~/.codex`.
- A per-task `account` field in a dispatch manifest or Fabric submission, or
  the single-lane `dispatch --account` flag, filters the requested adapter's
  accounts before quota and readiness. It is not a route pin and never
  switches accounts on quota.
- Each account is signed in by the user through the provider's own flow.
  Credentials are never copied between identity directories. A switch moves
  work to an account with its own headroom and never suppresses a limit signal.
  Accounts are added or retired by configuration only.
- Lane rows show the applied account label, or `unknown`.

### Quota

Statuses are available, credit-only, exhausted, stale, unknown, auth-required
and unsupported.

- Each reading records used share, reset time, source and observation time.
  Admission treats a reading as fresh only while its age is under the
  adapter's timer interval; older readings never drive placement.
- A failed refresh marks the retained snapshot stale at once. Human output
  prints each account with `STALE`, `last known`, age, window percentages and a
  sanitised failure code. JSON keeps the original snapshot and exposes current
  freshness on the account row. A recorded plan name is not evidence of the
  current subscription.
- Refusals are model- and account-scoped unless wire evidence proves wider
  scope, and independent allowance pools never exhaust each other. A newer
  comparable observation, or the reset time, expires only that scoped refusal;
  unknown data preserves it. An unknown reset backs off for one hour.
- Only a quota-limited outcome cools an account, keyed on provider, account and
  model. A rate limit is a short per-model cooldown and retry, not a failure.
  Authentication failures and policy refusals never trigger an account switch.
- OpenCode decodes its structured `provider.quota` body. A free-usage limit
  names the shared `opencode-free` pool and blocks other free models on the
  same account key until reset or the one-hour backoff.
- Codex decodes an `error` event reporting `429 Too Many Requests` (retries
  exhausted) as a rate limit, never a quota refusal; a `Reconnecting` message
  stays transient.
- Claude `api_error_status = 401` is `AuthRequired`. Provider prose that
  mentions expiry or quota is ordinary text and asserts nothing.
- Tick TTLs (timer and panel seconds): Claude 300 and 120, Codex 900 and 300,
  Cursor 1800 and 600, Agy 1200 and 900. Manual has a 30 s floor and respects
  backoff. Battery doubles intervals; low power quadruples them and skips Agy.
  Network and 429 backoff is 120 to 3600 s, auth 3600 s and a missing CLI
  86400 s, with `Retry-After` as a lower bound capped at 86400 s. At most three
  cheap probes run concurrently, Agy alone, each with a deadline and a 1 MiB
  response bound. A tick's wall budget is at most 55 s.
- Between like accounts, spend the one whose weekly reset is sooner. Paid pools
  are used last and paced to the remaining days.

### Provider launches

`providers::prepare` returns a launch spec: exact argv with the canonical
executable first, the complete child environment, stdin, cwd, setup steps and
the confinement profile. It is held in memory; only a redacted summary persists.
Argv prompts are bounded to 64 KiB. Continuation binds the exact adapter,
session, account, host, model and sent controls, and rejects incompatible grants
or identity changes. Run owns custody handoff, retries, home finalisation and
publication.

Decoders use documented event positions and never search tool or subagent
output for the answering identity. They emit assistant text, usage,
session and model evidence, refusal and reset, and terminal or malformed
facts. Structured errors outrank claimed success, and recovered transport
retries clear transient evidence. A family is established only by provider
protocol evidence verified against the catalogue revision; never from model
spelling, a broker name or the requested ID. Absent or mismatched evidence is
unknown and cannot satisfy cross-family review.

## External contracts

### Credentials

Quota credentials are held in memory only. They are never logged, written,
placed on argv or in a child environment, and buffers are dropped after the
probe. Runtime access records actor, exact source, purpose and expiry before
reading; only the non-secret authority reference is kept.

| Source | Rule |
| --- | --- |
| Claude `.credentials.json` in the selected config directory, otherwise the macOS Keychain item `Claude Code-credentials` (suffix from the config path hash for other directories) | Read through supervisor with memory-only capture that bypasses attempt spooling, even on error, overflow or cancel. Linked credential files are rejected. |
| Cursor `state.vscdb`, key `cursorAuth/accessToken` | Read-only query into memory. No copy, export or raw row in diagnostics. |

Codex and Agy quota use their own CLI protocols, not auth-store reads. Claude
and Cursor usage calls use system `curl --config -` with the header on stdin
only, fixed allowlisted URLs, no redirects or verbose output, bounded time and
body, and system trust and proxy settings (ADR 0028).

### Adapter launch forms

Confinement adds only verified permission and config arguments; there is no raw
caller argv. Each adapter's exact argv and environment are recorded in
`tools/front-door/tests/fixtures/providers/<adapter>/recordings.json` and the
provider tests. Intent that the fixtures cannot show:

| Adapter | Notes |
| --- | --- |
| Codex | `codex exec` with `--json` and the prompt on stdin. `CODEX_HOME` is attempt-private, with `auth.json` linked to the selected registration. Codex's own seatbelt is off because it cannot nest; the outer profile is the boundary. Continuation pins the same `-m`. |
| Claude | `claude -p` with strict MCP config, Provenant-only settings, no slash commands and an explicit tool list. The host passes the OAuth token through an inherited pipe named by `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, bounded to 512 bytes and closed before spawn; no token appears in the start environment. `CLAUDE_CONFIG_DIR` is set when an account is selected. Read-only lanes add the host-bound Git common directory with `--add-dir`; the outer profile keeps it unwritable. |
| Cursor | `cursor-agent -p` in stream-json mode with the prompt on argv. Only writers add `--force`. |
| Agy | `agy --print` with a print timeout and no `--sandbox` (headless mode denies tools it cannot prompt for, so writers auto-approve inside the confinement profile). HOME is a private directory holding only a link to the login keychain. A print-timeout diagnostic keeps answer text as partial. Agy runs without Fabric MCP, hooks or lane environment. |
| OpenCode | `opencode run --standalone --format json` with the prompt on stdin. HOME is attempt-private. Inline config denies shell and web fetch, allows edit for writers only, and sets no MCP servers or plugins. |

Provider endpoints are `*:443` because Seatbelt cannot filter by host name.

### Claude token and refresh

Every unconfined provider command during attempt preparation, including
account status, provider version, keychain reads and token refresh, runs in its
own fresh, empty private directory under the host configuration root. Creation
is exclusive and the directory is removed after the command, including on
error. Attempt scratch, temporary directories and nested child stores never
supply the host command's working directory.

The sandbox never refreshes or writes the keychain. Before launch the host
refreshes a token with under 15 minutes left by running the CLI once outside
the sandbox with a model name the API rejects, so no usage is spent, then
rereads and accepts at least a minute of life. That unconfined run disables
every customisation (safe mode, strict MCP config, hooks off, no slash
commands, no session persistence). It never copies credentials between
identities. Claude still holds the token in memory and has outbound HTTPS, so
this is not perfect isolation from a compromised provider. Shell-capable Claude
lanes set `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1`, pass the shell tool list
explicitly, and deny `/usr/bin/security`, `securityd` and keychain reads.

### Nested lanes, MCP and hooks

Every lane at every tier can use Fabric and the utility models, except Agy.

- The installer owns the stdio MCP generator; adapters reuse it. Claude writes
  two private JSON files in attempt `tmp/candidate-N` and loads only those;
  terminal cleanup removes them. Codex injects the same entry with `-c`.
  Hooks are the installer's shared registrations. User hook and MCP
  configuration is excluded.
- Each attempt gets a child run store at `RUN_STATE/children/<AttemptId>/`. The
  launch layer creates it and grants only that subtree, never `STATE/runs/` or
  the parent database. `PROVENANT_RUN_STATE_DIR` selects it for CLI run commands
  and the MCP; the original `--state-dir` still selects shared Fabric identity
  and config.
- Run supplies the attempt ID and depth. The adapter sets
  `PROVENANT_LANE_DEPTH` (root lanes are 1), `PROVENANT_PARENT_ATTEMPT_ID` and
  `PROVENANT_RUN_STATE_DIR` in both the provider environment and the MCP entry.
  Both also pin `PROVENANT_AGENT_LABEL=lane-<AttemptId>`, so the lane has
  its own actor and inbox. Under the host-set lane environment, Fabric
  refuses coordination mutations with `lane_coordination_refused`; reads and
  child-store run operations remain available. The shared SQLite write grant
  remains pending [lane store isolation](fabric.md#follow-up-lane-store-isolation).
  Submission refuses a child above `lanes.max_depth` (default 2), and a state
  path that is not `children/<AttemptId>` pairs below the original root. The
  limit is recursion control, not an authority boundary.
- `fabric_submit` from a lane only seals queued child records. A host-side worker
  in the parent guardian prepares and launches them, rechecks the parent
  attempt and depth, and bounds each child by the parent's sealed task. In v1
  no lane may propose a writer or delivering child: the host holds no sealed
  list of worktrees a lane may delegate, and a writer child could leave build
  scripts or tests that later run outside a sandbox. A child's network reach and
  read and write roots must sit within the parent's after canonicalisation, and
  a child cannot continue an attempt. The host launches only the request it
  validated and refuses one whose digest changed after validation or whose
  re-read request no longer matches the digest sealed at hand-off. Invalid
  proposals terminate as rejected before provider preparation. The host launches
  at most one attempt per child task and refuses extra attempt rows. A child
  launches for its parent's requester, sealed in the host-built holder context,
  never the actor its run record names. On start the host records that
  requester as the run's actor, the attempt's verified owner and its admission
  holder. The lane's operation keeps the lane's actor, which scopes its replay
  and listing. Child storage is writable by the submitting lane, so it is not
  independent evidence against that lane: the host's view refuses a run or
  attempt it launched once its record names another owner.
- The guardian stops its worker on observed provider exit. Parent cancellation,
  deadline or supervisor signal cancels only this worker's children; normal
  parent completion leaves launched children's custody intact. The host first
  stops every child it handed to a guardian, including one launched after its
  last scan, through the attempt-directory descriptor, scope and requester it
  held at hand-off, so renaming the attempt directory cannot redirect the stop. The
  child's record and launch summary do not decide the stop. Superseded
  generations retain their scopes for owner checks but drop their handles.
  At the cap of 64 held child handles, the host probes each custody lock
  through its held directory. An exclusive non-blocking lock on the same
  device and inode captured from the publication reservation proves guardian
  exit and releases the handle, retaining its scope for owner checks. Busy,
  missing, replaced or unobservable locks retain their handles. If none can
  be released, the host defers further child launches: the attempt stays Queued,
  unmarked and without a launch claim, and the next scan retries it. The final
  scan after normal parent completion refuses children still deferred, since no
  later scan will launch them.
  The cap leaves descriptor headroom below macOS's common 256 soft limit;
  lane-writable terminal records cannot authorise release. The host then stops
  accepted children that never reached a guardian, from their records. A failed
  scan read does not end the worker before that stop. The stop request itself
  still lands in the attempt directory, which the lane can write, so the lane
  can pre-empt it (for example with a directory named `cancel.json`) or undo it
  after the host writes it, and keep its child running.
- Git is resolved once on the host (`PROVENANT_GIT` override, otherwise the
  fixed list preferring package-manager Git), canonicalised, pinned in provider
  and MCP environments, and the only Git path lanes may execute.
- Writer-only grants, such as configured `[domains.run] writer_writes`
  directories, a per-worktree Cargo cache with a private `CARGO_HOME`, linked
  worktree commit files and Git helper execution, come from confinement's
  writer profile ([supervisor](supervisor.md)). Remote-tracking refs and shared
  Git config stay host-owned and read-only.
- Host-side delivery for a task with `deliver` is run's: fetch before launch,
  push the final commit only on verified writer success, compare the remote ref,
  set upstream, and keep a typed receipt. The host credential environment never
  reaches the provider or a record. Recovery never repeats delivery.

### Recordings

`cargo run -p provenant-providers --features recording --example record --
ADAPTER read_only|worktree_write --model M --root EMPTY_DIR [--write]` runs the
CLI fresh then resumed under the compiled profile and probes every claimed
observation: declared and undeclared reads and writes, secret and protected
paths, endpoint versus other network, exec and fork, state writes and, for
writers, worktree, index, HEAD, objects and ref writes. Output is redacted
before storage. Recordings and probes are replay and confinement evidence only;
they never gate execution. Manifests record CLI version, platform, redacted
argv, access and session mode, and must show the exact selector, effort and
service controls being enabled. Efforts without a recorded control stay unset.
Listing seeds in `tests/fixtures/model-listings/` support listing only.

## Failure and recovery

- A provider failure decodes to a typed outcome. A token the API still rejects is
  `Refused` with an authentication-required reason; the user signs in again
  outside the sandbox. A sandboxed 401 routes around through a permitted
  cross-adapter retry rather than widening keychain or auth-write access.
- Route health comes from receipts. Rate-limited and quota-limited outcomes are
  kept apart from real failures.
- Unknown or stale quota never refuses. A failed refresh keeps the last good
  data, labelled with its age.
- Bookkeeping failure cannot signal a healthy provider or lose terminal
  evidence. Claims expire; completions against a stale revision are dropped.
- A failed resolve reports exclusion reasons per candidate, and each alternative
  is revalidated before use.
- Agy `--print-timeout` handling keeps answer text as partial; a timeout is never
  inferred from exit status alone.
- Claude resume is refused until a provider session exists. The private
  `CLAUDE_CONFIG_DIR` is writable provider state and not an immutable trust
  boundary.

## Deliberate cuts

| Feature | Reason |
| --- | --- |
| Rotating pools, a selection store, `--commit-selection`, preferences, inferred class or size routing | Weighted, paced selection seeded by the operation gives retry stability without stored state. |
| `--pool` and `--council` flags, council selectors | Pools are route names; a council is distinct exact-model tasks synthesised in the chair. |
| Public capability, cooldown and probe verbs, canary inference, endpoint profiles | Preflight and quota reads are internal effects; observed use was zero. |
| Account pins on routes, native-seat inference, host avoidance | Accounts are filtered by an optional per-task field; Fabric supplies placement. |
| Old tier names (`strong`, `bulk`), typo guessing, hidden model classification, shadow stages, the routing-context hook | Historical names and heuristics; exact names are validated. |
| Manual credit, history and forecast commands, OpenCode spend estimator | Speculative or unused. |
| Kiro, Copilot, Pi and Herdr execution | No dispatches and no recordings. |
| Reads of old transcripts for quota or identity, home mirroring, a shim | Transcripts are research input, never runtime state. |
| Agy identity inferred from logs | No signed-in identity source exists; attribution would be guessed. |
| Concurrent writer batches, a scheduler, daemon or recursive swarm manager | No workflow has shown a need that serial or isolated writers cannot meet. |
| Universal hashes, environment fingerprints, version locks, a catalogue registry | Account keys hash identity fields only. |
| Soft gate bouncing spawns that name no tier, with a logged override | Not built; tier policy is guidance recorded in the receipt. |
| Route-health demotion | Not built; outcomes are recorded and telemetry reads them. |

## Verification

- Unit tests in `tools/front-door/crates/routing/src/tests.rs` cover exact
  selectors, pool expansion and weighting, pacing clamps, bounded fallbacks,
  confidentiality, effort support and revision mismatches.
- Quota tests (`crates/quota/src/admission.rs`, `schedule.rs`) cover unknown,
  zero and stale readings, independent pools, reset and DST handling,
  `Retry-After`, revision and claim expiry, and clearing a refusal on a newer
  observation.
- Provider tests (`crates/providers/src/tests.rs`) replay recordings through the
  real decoders for nested identity, recovered retries, terminal and exit
  combinations and bounded malformed input, and check that each adapter
  prepares a launch without a recording.
- CLI integration tests under `tools/front-door/tests` drive real rendering: no
  process on help, list, show or resolve; envelope, exits and size cap; tick
  partial coverage and revision races. Fake `curl`, `security` and provider CLIs
  assert tokens are absent from argv, environment, files and diagnostics,
  including on overflow and cancel.
- Every process test points `HOME`, state, `CODEX_HOME`, `CLAUDE_CONFIG_DIR` and
  XDG variables at temporary directories, with no real keychain or old-root
  access.
- Handshake and FIFO fixtures, with no sleeps, cover late probe completion,
  competing refresh writers, ENOSPC and copy-back races. macOS CI compiles the
  real plan and profile with a fake provider and probes custody denial and
  nested host-supervised children below the depth limit.
- Supervisor proves custody and cancellation; run proves one writer, atomic
  admission and terminal publication. These crates prove routing policy and wire
  decoding. Live provider acceptance stays a chair gate, not a CI check.
