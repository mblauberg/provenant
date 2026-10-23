# Spec: observability layer and native macOS console

Status: **proposed**, 2026-10-22, workstream W6. GitHub owns delivery state;
this file owns the durable requirements once accepted. It grants no authority
and relaxes no gate in `HARNESS.md`. It builds on
[Fabric across the user's own hosts](fabric-hosts.md), the
[operating model](harness/operating-model.md) (R4 telemetry, R6 continuity)
and the [profile telemetry stamp](harness/profiles.md#telemetry-stamp). Visual
and interaction design lives in the companion
[console design](observability-console-design.md).

## Outcome

From one native window the user can see what each session carries in its
context window and how that changed across compactions; the summary and state
text each compaction re-injected, exactly as received; who dispatched which
lane, on which host and from which session; cost per accepted change and
tokens by model, effort, provider and preset; and every account's quota,
including accounts signed out on this host.

The first slice reads everything on demand through the CLI. A caching
projector and federation between hosts come later, and only when a
measurement shows the on-demand path is too slow or costs too many SSH
sessions.

## Goals

1. Every context figure labelled `exact`, `apportioned`, `derived`,
   `estimated` or `unavailable`; no estimate scaled to fit a total.
2. Compaction records that show the recorded re-injection, not a later copy.
3. One identity model across sessions, projects and hosts.
4. One record schema shared by the CLI, the console and any later projector.
5. Zero or more peers, no host names in code, no SSH when nothing watches.
6. Requested, resolved and observed model and effort on telemetry rows, with
   preset and harness revision.
7. One modular app. Every feature is a module the user can switch off, a
   disabled module costs nothing at runtime, and nothing in the CLI, Fabric or
   telemetry depends on the app.

## Non-goals

- New prompt or tool-detail logging; OTel prompt and tool detail exporters
  stay off. Text already on disk is read on demand and held in memory only.
- A network listener, HTTP API, broker or shared database; writes to any
  provider or Fabric store; hand edits of configuration from the console.
- Linux or Windows as supported hosts. Linux is a CI build target only, so
  nothing here makes Linux peers harder to add later.
- A remote web board, hosted observability or third-party trace export.

## Relationship to existing decisions

- [ADR 0020](../adr/0020-retire-the-daemon-fabric.md) retired the daemon on the
  coordination path, including its Rust review-portal supervisor, and
  [Fabric hosts](fabric-hosts.md#exclusions) excludes "any daemon, broker,
  relay service or listening port". Slices 1 to 4 add no long-running process.
  The [projector](#projector), if built, has real
  [side effects](#side-effects), so it needs two amendments first: an ADR 0020
  section admitting a derived cache with exactly those side effects, and the
  Fabric hosts exclusion reworded to "any coordination daemon, broker, relay
  service or listening port", linking to it. The user approved this in
  principle (see [Decisions](#decisions)); each amendment is its own pull
  request through normal review and still waits for the Slice 5 measurement
  trigger.
- The telemetry ledger (R4) stays the single writer of usage rows.
- The `session` skill saves bounded Markdown under the explicit project and
  host-session key. SessionStart injects only that checkpoint, or a missing or
  stale status; this spec adds no state-file convention. Managed hook
  registration changes occur at the final Rust install.

## Architecture

```text
 Slices 1 to 4 (no long-running process)
  transcripts, provider stores, Fabric state, ledger, quota report
     --> provenant session | lanes | quota | statusline | settings --json
     <-- Provenant app: menu bar agent and console process (see Console),
         both off main thread

 Slice 5 onwards, only if a trigger fires
  same sources --watch--> provenant-projector --> projector/v1/ (contract)
                               \--ssh, while watched--> peer entrypoint
```

The CLI output and the contract files share one schema per record, so a
reader that finds no fresh contract runs the CLI and gets the same JSON.
Writes go only through the CLI, which delegates to the existing owners.

## Identity and attribution

### Fields recorded at dispatch

Every dispatch records, in its seat-index entry and its run receipt:

| Field | Meaning | Source |
|---|---|---|
| `seat` | The client process the Fabric MCP server acts for: `{host, pid, started_at, label}` | MCP server's parent process, as in PR #1085 |
| `seat_id` | `<pid>-<started_at>-<label>@<host>`, the seat directory name qualified by host | derived |
| `client` | MCP `clientInfo` name and version, and the provider session id when known | MCP initialise; session hook where available |
| `project` | Home-relative primary checkout path the lane works for | request, else the seat's project |
| `worktree` | The linked worktree the lane writes in, or null for read-only lanes | dispatch owner |
| `owning_host` | Host of the dispatching seat, to which the lane reports | `seat.host` |
| `executing_host` | Host whose process runs the lane; authoritative for its run directory and liveness | dispatch owner on that host |
| `parent_lane` | Qualified id of the enclosing lane when a lane dispatches, else null | lane environment set by the dispatch owner |
| `operation_id` | Caller-generated idempotency key | existing remote dispatch rule |

Rules:

- **cwd decides write scope only.** It selects the worktree and confinement,
  never ownership or project attribution on its own.
- **The seat decides ownership.** "Mine" means lanes whose `seat_id` is the
  viewer's seat or a descendant through `parent_lane`. The statusline and CLI
  know their seat. The console is launched without one, so it offers a seat
  picker (default: all seats, choice remembered) and "Mine" is that choice.
- **Views default to everything on the host**, then add peers. Narrowing to a
  seat, project or host is a filter, never a different data source.
- A dispatch with no MCP seat in its ancestry (a plain shell `provenant
  dispatch`) has `seat: null` and appears under "No seat" for its project. It
  is never attributed to a guessed process.
- Host identity comes from one owner (H1 in
  [Migration](#migration-from-current-hardcoding)). No module derives a host
  id independently.

### Seat index, extended

PR #1085 adds `lane-seats/<pid>-<start>-<label>/<hash>.json` under the Fabric
state directory, holding `{run_dir, pid, started_at, label}`, written once
with mode 0600. This spec extends it additively:

- `lane-seats/<seat-dir>/seat.json`, written once by the MCP server after
  initialise: `schema`, `host`, `pid`, `started_at`, `label`, `client`, `cwd`,
  `project`, and `provider_session_id` when known.
- Each per-dispatch file gains `project`, `worktree`, `owning_host`,
  `executing_host`, `parent_lane` and `operation_id`. Readers treat missing
  fields as unknown, so older files stay valid.
- A remote dispatch carries `seat_id` in the request. The executing host
  stores it in the receipt as `owner_seat` and creates no seat directory for a
  seat it does not host.

The provider session id comes from a hook that reports it (Claude Code's
SessionStart), else the Fabric `sessions` table for lanes, else a cwd and
start-time match against the first transcript entry, labelled `estimated`.

### Scenarios

Host names below (laptop, workshop) are examples; code holds none.

- **S1. Two sessions, one project, one host**, one in the primary checkout
  and one in a linked worktree. Each Fabric MCP server records its own seat;
  both resolve to the same `project`. "Mine" in each shows only its own
  lanes; Fleet shows both seats under the project.
- **S2. Different projects, one host.** A chair in project A dispatches a
  lane for project B. The lane records `project: B`; ownership stays with the
  chair's seat, and PR #1085's `readSeatRuns` lets that seat see it.
- **S3. A peer's session running a lane here.** `owning_host: workshop`,
  `executing_host: laptop`. The laptop owns the run directory and liveness
  and lists the lane under a "remote seat" node. Neither host probes the
  other's processes.
- **S4. A local session running lanes on a peer** (`--host workshop`). The
  seat is local; the lane shows under it with a host badge, and when the peer
  is offline shows `unreachable` with last state and age while `--wait` keeps
  it pending, as [Fabric hosts](fabric-hosts.md#lanes-status-and-control)
  requires.
- **S5. A shell dispatch** with no agent session records `seat: null` and is
  listed under "No seat" in its project.
- A terminal session running on a peer over SSH is that peer's process.

## Session context

### Exactness labels

Every figure carries one label:

| Label | Meaning |
|---|---|
| `exact` | Recorded or counted by the provider's own counter |
| `apportioned` | An exact total split across items by their local text length |
| `derived` | Arithmetic on exact figures only (for example free space) |
| `estimated` | Computed by Provenant from text on disk; records the estimator id |
| `unavailable` | Nothing on disk or from the provider supports it |

Each measurement also records its `method`: `provider_counted` (the provider
counted it for the live session), `probe_reconstructed` (the provider counted
a reconstruction, below), or `transcript_estimate`.

### Collection methods

1. **Transcript parse (all providers, always).** Free, offline, and works for
   finished sessions. Totals are exact where the provider records usage;
   categories are estimated from the recorded content.
2. **Claude Code `/context` probe (opt-in per account).** Claude Code answers
   `/context` in print mode as a local command with no model turn:

   ```text
   claude -p "/context" --resume <session-id> --no-session-persistence \
     --output-format stream-json --verbose \
     --settings '{"disableAllHooks":true}'
   ```

   run from the session's recorded `cwd` under its own identity directory
   (`CLAUDE_CONFIG_DIR`), so traffic and rate limits fall on the right account.
   The response's `context_usage` holds `categories[{name, tokens, kind}]`,
   `memory_files[]`, `mcp_tools[]`, `agents[]` and `skills[]`, counted by
   Claude Code with the provider's count endpoint under its own login.
   Provenant reads no credential, and the transcript is unchanged.
   - Claude Code rebuilds prompt, tools, skills, agents and memory files from
     the configuration and `cwd` at probe time, and MCP start-up can race the
     count, so the result is `probe_reconstructed` ("what this session would
     carry if resumed now") and never overwrites a transcript figure.
   - It starts the session's MCP servers with `PROVENANT_CONTEXT_PROBE=1` set,
     and the Fabric MCP server then registers no seat.
   - It sends the counted text to the provider's count endpoint. That provider
     already received it, but Provenant initiates the egress, so the probe
     runs only when `observe.context.exact_counts` allows it for the account,
     on an explicit user action, at most once per session per 10 minutes,
     never in parallel for one session, with back-off on rate limiting.

### Per-provider sources

Identity directories (several per provider when accounts are separated by
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` or equivalent) come from the telemetry
configuration, never fixed paths. Parsers key on the provider's recorded
version and count unknown entry types as `other` rather than failing.

| Provider | Total | Categories | Compaction summary | Re-injected state |
|---|---|---|---|---|
| Claude Code | exact: last `message.usage` input + cache read + cache creation, deduplicated by `message.id` | probe: exact per category, items as below; transcript: estimated | exact: `user` entry with `isCompactSummary` after `compact_boundary` | exact: `hook_success` attachment for `SessionStart:compact` |
| Codex | exact: `token_count.info.last_token_usage`; window exact from `model_context_window` | estimated, keyed by `content_item_kinds` (instructions, AGENTS.md, skills, permissions, environment); MCP and app tool schemas from `thread_dynamic_tools` in `state_*.sqlite` (read-only); built-in tool definitions unavailable | unavailable: `encrypted_content` | exact: `replacement_history[]` and `retained_context` (plaintext); no state hook today |
| OpenCode | exact per step: `step-finish.tokens`, v2 `assistant.tokens` | instruction paths exact, tokens estimated; system core and tool definitions unavailable | exact: v2 `session_message` `compaction.summary` | unavailable |
| Gemini CLI | exact per turn: `tokens.input`, `cached`, `thoughts`, `tool` | estimated from the `GEMINI.md` chain and messages | unavailable until a marker is confirmed | unavailable |
| Copilot CLI | exact: `currentTokens` and usage events | exact at compaction and shutdown: `systemTokens`, `toolDefinitionsTokens`, `conversationTokens` | exact: `summaryContent` | unavailable |
| agy | unavailable until a decode spike confirms `gen_metadata` fields against receipt usage | unavailable | unavailable | unavailable |
| Kiro | unavailable | unavailable | unavailable | unavailable |
| Cursor | unavailable; lane totals from Fabric receipts only | unavailable | unavailable | unavailable |

Rows for Gemini CLI, Copilot CLI and Kiro come from an earlier structural
survey and are re-verified in [Slice 7](#slice-7-remaining-providers). Cursor's
chat `meta` holds a blob encryption key: readers parse only `agentId`,
`latestRootBlobId`, `name` and `createdAt` and never load the rest. OpenCode's
credential and account tables, and every provider's auth files, are never
opened.

Lane usage for every adapter also comes from Fabric attempt receipts, which
the ledger already ingests.

### Claude Code categories

| Category | Probe | Transcript only |
|---|---|---|
| Total in use | exact | exact |
| System prompt | exact | estimated from `prompt_snapshot.systemPrompt[]` |
| System tools, loaded and deferred | exact (deferred rows `kind: deferred`, outside the window) | estimated from `prompt_snapshot.tools[]`; deferred names from `deferred_tools_record` |
| MCP tools, loaded and deferred | category exact; per tool apportioned | estimated from `deferred_tools_record.entries[]` schemas |
| MCP server instructions | category exact; per server apportioned | estimated from `mcp_instructions_delta` |
| Custom agents | exact per agent | estimated from `agent_listing_delta` |
| Memory files | exact per file (`path`, `type`) | paths exact from `instructions.files[]` and `nested_memory`; tokens estimated |
| Skills | category exact; per skill estimated | estimated from `skill_listing`; invoked skill bodies from `invoked_skills` |
| Slash commands | exact | not separable |
| Messages | derived: exact total minus the counted categories | estimated from user, assistant, tool-result and file attachments, including `session_context` |
| Autocompact buffer | exact from configuration (`autoCompactThreshold`, `autocompactSource`) | unavailable |
| Free space | derived | derived when the window is known |
| Unattributed | none (Messages absorbs framing) | exact total minus the raw sum of estimates |

`total_tokens_reminder` entries report remaining budget, not context size,
and are never read as context. Per-item figures are labelled as the provider
reports them; a provider-side switch to bulk counting turns "exact per file"
into `apportioned`, and the parser records what the response says rather than
assuming.

### Window size

The window comes from the provider when recorded (`model_context_window` for
Codex, `rawMaxTokens` from a Claude Code probe). Otherwise it comes from the
optional setting `observe.context.windows.<model-id>`. With neither, the
window, percentage and free space are `unavailable`; the console shows tokens
in use without a percentage. `config/model-routing.json` `context` is a global
ceiling, not a window, and is not used.

### Estimation rules

- Estimates are never scaled to the exact total. The console shows the raw
  estimates and, separately, the unattributed remainder (exact total minus
  their sum), which may be negative when estimates overshoot.
- The estimator is Claude Code's own offline rule for Claude sessions
  (`chars/4`, `chars/2` for JSON, 2000 per image or document), recorded as
  `estimator: {id: "chars4", version: 1}`; other providers record their own
  id. No calibration step.
- **Accuracy bound.** Before transcript estimates ship, an evidence run on at
  least ten local Claude Code sessions compares each transcript-estimated
  category with a probe count of the same session and records numbers only.
  For categories of at least 1,000 counted tokens, the estimate must lie
  within 20% of the count in at least 90% of pairs. A category that misses the
  bound is shown as `unavailable` in transcript-only mode until a later
  estimator meets it. The run is repeatable with
  `provenant session context --calibration-report`.
- Instruction files are attributed as the transcript recorded them, not as
  they are on disk now; where Claude Code records `contentDiffersFromDisk`,
  the console says so.

## Compaction and re-injected state

For each compaction ordinal *n* the record holds:

1. trigger, timestamp, and exact pre and post totals (`compactMetadata` for
   Claude Code; neighbouring `token_count` events for Codex);
2. the summary: exact text locator for Claude Code and OpenCode, `encrypted`
   for Codex, `unavailable` otherwise;
3. what was re-injected: for Claude Code, every `hook_success` attachment with
   `hookName: "SessionStart:compact"` between the boundary and the first
   assistant turn, with the state path named in its header, byte size and
   hash; for Codex, the `replacement_history` items. This is the ground truth
   for "what the session received", including the hook's 8,000-character cap;
4. what was re-attached: `file`, `compact_file_reference`, `invoked_skills`
   and `task_status` attachments before the first assistant turn.

Diffs between compactions compare successive recorded payloads (injected
state *n−1* against *n*, and summary *n−1* against *n*). Both texts are already
in the transcript, so nothing is copied or stored. The state file as it is on
disk now is shown as secondary, with a flag when it differs from the last
injected payload (a later edit, truncation at the cap, or a different file
chosen by the hook). A compaction with no recorded re-injection says so;
Codex sessions show that no state hook is configured for that provider.

Text (summaries, injected state, re-attached content) is read on demand and
returned only when the caller asks for it (`--text`). Records otherwise carry
locators (file and byte offset, or row id), sizes and hashes.

## Session CLI

Slice 1's core is one command family, owned by a new `scripts/provenant-session`
in Python, matching the other CLI owners:

```text
provenant session list [--provider P] [--project PATH] [--since T] --json
provenant session context <session-id> [--at-compaction N] [--exact] --json
provenant session compactions <session-id> [--text] --json
provenant session context --calibration-report --json
```

- `list` returns identity, last activity, total in use, window when known and
  compaction count, reading file sizes and tails, not whole transcripts.
- `context` returns the record below; `--exact` runs the probe when settings
  allow it and is refused with a typed error otherwise.
- `compactions --text` adds summary, injected and re-attached text, at most
  64 KiB per item, held in memory only.
- The schema, `provenant.session.context.v1`, lives in `schemas/session/v1/`;
  a later projector writes the same schema.

Record outline:

```text
identity:  host, provider, provider_version, identity_dir (home-relative),
           session_id, seat_id?, project, cwd, branch,
           model {requested, resolved, observed}
window:    size?, size_source (provider|settings), autocompact_threshold?,
           autocompact_source?, buffer_tokens?
snapshots[]:
  at, turn_ordinal, message_id?, compaction_epoch,
  method (provider_counted|probe_reconstructed|transcript_estimate),
  estimator? {id, version},
  total {tokens, exactness}, api_usage {input, cache_read, cache_write, output},
  categories[] {id, name, kind (used|deferred|buffer|free), tokens, exactness},
  items[] {category_id, kind, name, source?, server?, path?, tokens, exactness,
           locator?},
  unattributed_tokens?, deferred_tokens
compactions[]:
  ordinal, at, trigger, pre_tokens, post_tokens, dropped_tokens?,
  summary {exactness (exact|encrypted|unavailable), locator?, bytes?, sha256?},
  reinjected[] {source (hook_success|replacement_history), hook_name?,
                state_path?, locator, bytes, sha256},
  reattached[] {kind, name_or_path, locator},
  post_snapshot_ref?
state_file: {injected_path?, on_disk {bytes, mtime, sha256}?, differs: bool}
```

Paths are home-relative; paths outside home are dropped. Snapshots are taken
at the latest turn, at each compaction boundary and after each compaction;
`--at-compaction` selects one.

## Telemetry rules

1. **Requested, resolved and observed** are separate fields for model and
   effort: what the caller asked for (a route alias, a model, or nothing),
   what the router chose, and what the provider reported (`message.model`,
   `turn_context` effort, Claude's `requestedModel` and `effort`). A
   resolved-versus-observed mismatch is a reported finding.
2. **Every usage row carries** preset with its digest, `harness_rev`, model in
   all three forms and effort in all three forms. Today `harness_rev` lives on
   `events` and the preset in `events.attrs` under the key `profile`. The
   ledger bumps `PRAGMA user_version` from 4 to 5, adds the columns to
   `usage`, and backfills them by joining receipts. Native sessions without a
   receipt take the preset and revision current at the session's start,
   labelled `inferred`.
3. **Cost per accepted change** is the headline cost figure: spend attributed
   to accepted changes in a period divided by the number of accepted changes.
   - An accepted change is a merged pull request (decided): a row in the
     ledger's `changes` table with a `merge_commit`, merged through the
     repository's own workflow. It is not one agent's or one session's output:
     many agents may code one pull request, and the user sometimes prefers one
     large branch to many small merges. R4.3 accepted tasks without a pull
     request are a second, separately labelled figure.
   - **Contributors.** A merged pull request credits every session and lane
     found on its branch. Neither `changes` rows nor receipts hold the head
     branch or a lane's commit range today, so Slice 3 adds the capture: the
     paths that open a pull request from a lane or chair session emit an
     `events` row carrying the pull request, head branch and session or lane,
     and lane finish records its start and finish commits. The ledger keeps
     these rows after branch cleanup. Pull requests merged before Slice 3 show
     unlinked commits only. Usage joins to a lane by its host-qualified lane,
     `task_id` and `attempt` (never `run_id` alone, which siblings in a batch
     share) and to a session by session id. Commits made outside any recorded
     lane are shown as unlinked commits and credit no session.
   - **Shares are descriptive.** Each contributor shows its commits, changed
     lines, review and repair rounds and spend. There is no winner, no single
     owner and no score that sums to one. Spend of a session or lane linked to
     several pull requests is divided in proportion to its commits in each
     (equally when it has none). Spend with no merged pull request, including
     abandoned branches, is unattributed and shown beside the headline as a
     share, never hidden.
   - **Size normalisation.** Beside cost per accepted change the console shows
     spend per 100 changed lines (`git diff --numstat` of the merge, ignoring
     paths marked generated) and the median cost per change in three size
     buckets (up to 100, up to 500, over 500 changed lines). Period
     comparisons use the per-line figure or the buckets, so a large pull
     request is neither penalised nor overweighted against small ones.
   - **Long-lived branches.** Review and repair rounds are counted within the
     pull request, across every intermediate review cycle before the merge,
     from review and repair `events` carrying the pull request.
   - **Approximate:** commit-to-lane placement, proportional spend splits and
     line counts are approximate and labelled so; totals of spend are exact.
4. **Internal names stay Provenant's.** Mapping to OpenTelemetry `gen_ai.*`
   happens only on export, which stays off by default and never carries
   content: resolved model to `gen_ai.request.model`, observed model to
   `gen_ai.response.model`, provider to `gen_ai.provider.name`, tokens to
   `gen_ai.usage.*`, session to `gen_ai.conversation.id`; requested model,
   preset, effort and harness revision as `provenant.*` attributes.

## Fleet and statusline

**Fleet** reads `provenant lanes --json` and the seat index, local host only
until federation exists. Peer lanes this host dispatched appear with their
last locally recorded state and a host badge.

**Statusline.** `provenant statusline` prints lanes, in review and needs you,
for example `lanes 5 | review 2 | needs you 1`, and appends `| 1 host offline`
only when the last peer exchange failed. It shows no usage or quota. It reads
local Fabric state only (one read-only SQLite query and the seat index), never
opens SSH, and once a projector exists reads `statusline.json` instead. With
`--json` it prints `{"lanes", "in_review", "needs_you", "hosts_unreachable"}`.

- `lanes`: non-terminal lanes known to this host.
- `in_review`: lanes that finished successfully whose output the owning seat
  has not yet read (Slice 2 makes Fabric record the first `output` read).
- `needs_you`: lanes in `input_required`, and Fabric tasks or messages marked
  as a user gate.

## Accounts and quota

Quota presentation belongs to the quota report (W17). The console and menu
bar consume its fields and never re-derive them: window `label`, provider and
account `display_name`, plan display, per-window `pool` (an account is usable
while any pool is), the display window, the headline account choice, reset
time versus pace estimate, and the dropping of duplicate windows. Raw
provider vocabulary (window keys, plan codes, pool hashes, adapter names)
never reaches the display; a missing field is shown as unknown. The model is
account, then pools, then windows.

**Remembered accounts** (decided, built in W17). An account is added on its
first successful reading. When it later reads as signed out it stays listed
with its last cached reading, the time it was last seen and, once it
returns, "back since". The user removes one with `provenant quota forget
<account>`; an optional `quota.forget_after_days` forgets accounts signed out
for longer. Signed-out accounts never count as usable in the headline choice.

**Cross-host merge** (decided, M1). Merging other hosts' accounts is
optional (`quota.merge_hosts`, default on) and demand-driven: it runs only
while a viewer is open (the Accounts pane or the menu-bar panel), at most once
per minute, through `provenant quota --hosts all --json`. Each peer answers a
new read-only `quota.snapshot` verb from its cached report and never refreshes
from the provider for a remote caller. Accounts are deduplicated by their
host-independent fingerprint label (the same non-secret label on every host,
as the telemetry runbook asks); an account seen on several hosts appears
once, takes the freshest observation per window, and carries every host's
badge. Nothing is pulled when no viewer is open, nor while
`quota.merge_hosts` is off; the app then shows this host's accounts, and the
CLI's `quota --hosts all` still works on its own. This path is independent of
`app.modules.federation`.

## Console

Provenant is one app (decided): the quota menu bar app becomes Provenant and
gains the console. Its visual and interaction design is in
[the console design](observability-console-design.md); this section fixes
the decisions the spec depends on.

- **Platform.** Minimum macOS 26 (decided; both hosts run 27). macOS 27
  refinements (the `.tabs` picker, pointer-tuned interactive glass) sit
  behind availability checks. Nothing older: there is no macOS 25 (Apple
  went from 15 to 26), and macOS 15 lacks Liquid Glass and has no host to
  test on. `Package.swift` moves from macOS 14 to 26 in Slice 1, and the package is
  renamed from `apps/quota-menubar` to `apps/provenant`.
- **Structure.** `NavigationSplitView` with sidebar sections Activity (Fleet,
  Sessions), Insight (Telemetry, Accounts), Hosts (one row per host, "This
  Mac" alone with no peers) and Settings as a pane, reached by ⌘5 and ⌘,.
  ⌃⌘I toggles the inspector. Sessions detail tabs are Overview, Context,
  State and Usage.
- **Visual decisions** (design doc, section 8): a custom `Canvas` context
  bar with per-category drill-down; Swift Charts for real charts; the
  re-stepped categorical palette; custom glass only for the Fleet zoom
  controls; system text styles, since macOS has no Dynamic Type.
- **Menu bar.** The panel adds one activity line, "5 lanes · 2 in review · 1
  needs you", from `provenant statusline --json` run when the panel opens,
  hidden when all three are zero (a module, see below).
- **Settings.** Simple fields commit on edit through `provenant settings set
  --expect-rev`; a host's connection details edit together in a sheet with
  Cancel and Save. A Modules group holds the switches in
  [Modules](#modules-and-build-targets).
- **Data path.** The console never parses transcripts or opens SSH. It runs
  `provenant ... --json` off the main thread with a timeout and shows the
  typed result, and prefers the projector contract once one exists. Remote
  sessions' Context and State tabs are local-only: they show "Content stays on
  <host>" unless `hosts.peers.<name>.allow_content_reads` is on for that peer.
- **Accessibility.** Nodes and charts have labels in words; status is never
  colour alone; Fleet has a list equivalent.

### Processes and lifecycle

Two processes in one bundle, `Provenant.app` (decided).

| Process | Role | Lifetime |
|---|---|---|
| Menu bar agent | `LSUIElement` helper in `Contents/Library/LoginItems/`; status item, quota panel, activity line, optional notifications | Always on while enabled; started at login by the LaunchAgent `dev.provenant.app` (renamed from `dev.provenant.quota-menubar`) |
| Console | The visible `Provenant.app`: Dock icon, window, all console sections | Launched on demand; exits when its last window closes |

- **Mechanism.** The console quits with
  `applicationShouldTerminateAfterLastWindowClosed`, so closing the last
  window ends the process and the operating system takes back all of its
  memory, caches, charts and timers. The agent never imports console code, and
  the two share state only through files and the CLI: settings via `provenant
  settings`, data via `provenant ... --json` or the projector contract, window
  restoration in the console's own defaults. No XPC service, no shared
  in-process cache.
- **Why not one process.** Closing a SwiftUI window in a long-running process
  releases the views but not reliably the process footprint (view-model
  caches, chart and glass layers and allocator pages stay resident), so the
  menu bar would carry the console's peak. Process exit is the one mechanism
  that returns the memory by construction. The cost is a cold start of the
  console on open, inside the budget below. If a Slice 1 measurement shows a
  single process returning to the agent budget within 60 seconds of close, the
  split is reconsidered.
- **Opening.** The console registers the URL scheme `provenant`. `provenant
  console [section]` runs `open "provenant://console/<section>"`;
  LaunchServices starts the console if it is not running, or brings it to the
  front. The agent's Open Console item (⌘O) opens the same URL. The console
  is also opened from the Dock, Spotlight or Finder. Only the agent is a
  login item, so logging in never opens the console. On Linux the
  command prints that the console is macOS only and exits 2. If the app is not
  installed the command says so and exits 2; no other command mentions it.
- **Migration.** `provenant install` boots out the old label if loaded, removes
  its plist, writes and bootstraps `dev.provenant.app` (the agent), and is
  idempotent. Uninstall removes both labels.

### Independence from the menu bar

The menu bar is optional. With it off or hidden, the CLI, Fabric, telemetry
collection and the full console each run unchanged. Nothing in the CLI or
Fabric depends on the app being installed or running: they never launch,
signal or wait for it, and the only command that references the app is
`provenant console`. `provenant app menubar on|off` sets
`app.menubar.enabled` and boots the LaunchAgent in or out; the console's
Settings toggle runs the same command. The console opens straight from the
Dock, Spotlight or `provenant console` with no agent present. A hidden status
item (the user removed it with Command-drag) leaves the agent idle; turning
the menu bar off stops it.

### Modules and build targets

Every feature is a module registered at launch from `app.modules.*`. A
disabled module is never constructed: no timers, no file watchers, no cached
data, no code path run. A change takes effect the next time the console
opens or the panel opens, with no restart of the agent.

| Module | Process | Key | Default | Slice |
|---|---|---|---|---|
| Console | console | `app.modules.console` | on | 1 |
| Sessions context counts (the exact-count probe and its UI) | console | `app.modules.exact_counts` | on, but counting stays off per account | 1 |
| Fleet graph | console | `app.modules.fleet_graph` | on | 2 |
| Activity line | agent | `app.modules.activity_line` | on | 2 |
| Cross-host federation | both | `app.modules.federation` | on, inert with no peers | 6 |
| Notifications | agent | `app.modules.notifications` | off | not scheduled |

Weight is separable by target. The SwiftPM package has a small
`ProvenantCore` (CLI client, typed JSON models, settings reader), the two
executables `ProvenantMenu` and `ProvenantConsole`, and one library target per
heavy module (`FleetGraph`, `ContextCounts`, `Federation`, `Notifications`).
`ProvenantCore` is the only shared target. `scripts/build-app --without
<module>` leaves a library out of the build, so a build can drop Fleet graph
or Federation entirely; the registry then reports the module as not built and
hides its switch.

### Budgets and measurement

Measured on the user's own Mac (model and macOS version recorded), release
build, over a corpus of 500 generated sessions (a committed generator; sizes
from 100 KB to 20 MB), with `footprint -p <pid>`, `top -pid <pid> -stats
cpu,idlew` and `ps -o cputime`. Idle figures are the worst of three
10-minute windows; cold launch is the median of five runs from `open` of the
URL after the process has exited to the first-frame signpost. Recorded in the
Slice pull request, not run in CI.

| Process | State | Budget |
|---|---|---|
| Menu bar agent | idle, panel closed, any module combination | footprint at most 40 MB; CPU 0.0%, at most 2 s per hour; at most 1 wake-up per minute |
| Console | open, 500 sessions, one Context tab | footprint at most 120 MB; idle CPU at most 1%; cold launch to first frame at most 700 ms |
| Console | last window closed | process gone within 2 seconds; agent footprint unchanged within 5 MB |

## Settings model

Settings are validated and saved only through the CLI:

```text
provenant settings list [--json]         keys, values, defaults, sources, revisions
provenant settings get <key> [--json]
provenant settings validate <key>=<value>... --json        dry run, typed errors
provenant settings set <key>=<value>... [--expect-rev R] --json
provenant settings unset <key>... [--expect-rev R] --json
```

`settings` is a facade: each key's owner keeps its file and validator, and
`set` writes all its keys atomically or none. `--expect-rev` refuses a write
when the file changed since it was read. Setting a `hosts.peers.<name>.*` key
for a new name creates that peer and `settings unset hosts.peers.<name>`
removes it, so no `hosts add` command is needed.

| Key | Owner file | Type and default |
|---|---|---|
| `hosts.local_host` | `hosts.json` | name matching `[a-z0-9-]+`; default from the host identity owner |
| `hosts.peers.<name>.ssh_destination` | `hosts.json` | string, required |
| `hosts.peers.<name>.peer_command` | `hosts.json` | string; default `.local/bin/provenant peer` |
| `hosts.peers.<name>.connect_timeout` | `hosts.json` | seconds, 1 to 60, default 5 |
| `hosts.peers.<name>.response_deadline` | `hosts.json` | seconds, 1 to 120, default 15 |
| `hosts.peers.<name>.allow_content_reads` | `hosts.json` | bool, default false; lets this host read that peer's session context and state text on demand |
| `observe.context.exact_counts` | `observability.json` | `off`, `on` per account (`observe.context.exact_counts.<account>`); default `off`, opt-in per account (decided) |
| `app.modules.<module>` | `observability.json` | bool per module in [Modules](#modules-and-build-targets); defaults there |
| `app.menubar.enabled` | `observability.json` | bool, default true; set through `provenant app menubar on\|off` |
| `observe.context.windows.<model-id>` | `observability.json` | tokens, 1,000 to 10,000,000; no default |
| `console.default_scope` | `observability.json` | `all-hosts`, `this-host` or `mine`; default `all-hosts` |
| `quota.merge_hosts` | `telemetry.json` | bool, default true |
| `quota.forget_after_days` | `telemetry.json` | 1 to 3650, or unset (never); default unset |
| `observe.projector.enabled` | `observability.json` | bool, default false; exists only from Slice 5 |
| `observe.pull_interval_seconds` | `observability.json` | 15 to 3600, default 60, applied only while watched; from Slice 6 |

`observability.json` lives at `<instance-root>/config/observability.json`
beside `telemetry.json`. Unknown keys are errors. A missing file means all
defaults. Profile and telemetry account settings keep their own commands and
are not mirrored in this facade.

## Host-agnostic configuration

The host registry is the existing `hosts.json`, unchanged in shape except for
the additive `allow_content_reads`. No host name appears in code; names exist
only in this file and in test fixtures.

Zero peers, or no file at all, behaves the same:
`{"schema_version": 1, "local_host": "mini", "peers": {}, "projects": {}}`.
Nothing is pulled, the Hosts section shows "This Mac", host badges are
hidden, and every command behaves as on a single machine.

Two peers (names are examples):

```json
{
  "schema_version": 1,
  "local_host": "laptop",
  "peers": {
    "workshop": {"ssh_destination": "workshop.tailnet", "connect_timeout": 5,
                 "response_deadline": 15, "allow_content_reads": true},
    "studio": {"ssh_destination": "studio"}
  },
  "projects": {"Repos/provenant": {"default_host": "workshop"}}
}
```

Paths derive from `AGENT_FABRIC_STATE_ROOT`, `AGENT_FABRIC_INSTANCE_ROOT` and
provider identity directories.

## Projector

Built only when a [trigger](#slice-5-projector-trigger-gated) fires.

It watches sources and maintains the [contract](#projector-contract) for this
host (session records in the CLI's schema, lanes and seats, statusline
counters, the quota report) and, from Slice 6, pulls peers while watched. It
never runs a Claude Code probe.

### Side effects

The amendments to ADR 0020 and Fabric hosts must list exactly these:

1. **Ledger writes by proxy.** It launches `provenant telemetry ingest` when
   ledger sources change, at most once every five minutes. The ingest command
   remains the only ledger writer, but the projector causes the writes.
2. **SSH use.** From Slice 6 it uses the user's SSH configuration and agent to
   contact peers, only while a watcher is registered.
3. **Its own directory.** It writes only under
   `$AGENT_FABRIC_STATE_ROOT/projector/`.
4. **One supervised process.** An opt-in LaunchAgent named
   `dev.provenant.projector` (`KeepAlive`, background process type, low-priority
   I/O). No Linux supervisor; `provenant projector run --foreground` serves CI
   and manual runs.

It holds no authority over lanes, opens no port and serves nothing. Every
reader keeps its CLI path; a stale or absent contract means the reader runs
the CLI.

### Implementation: the Rust pilot

The projector is a Rust pilot with [kill criteria](#rust-pilot-kill-criteria).
ADR 0020 retired an earlier Rust component, the review-portal supervisor,
because of the authority it held, not its language; this pilot holds none.

- One binary, `provenant-projector`, in `runtime/projector/`, the first Cargo
  workspace. Dependencies: `rusqlite` (bundled), `serde`, `serde_json`,
  `sha2`, `notify`; no async runtime, HTTP stack or CLI framework.
- **Build versus depend.** Build narrow hot-path tools Provenant owns end to
  end (watching, parsing, rolling up); depend on established foundations
  (SQLite, cryptography, file watching, Git).
- SSH: it shells out to `ssh` (or `AGENT_FABRIC_SSH_PROGRAM`) with the same
  argument construction as `scripts/fabric_hosts.py`, checked by one shared
  fixture of expected argument vectors tested in both languages.
- macOS supported; Linux built and tested in CI only. One instance per state
  root, by advisory lock. Targets: a change reflected within 2 seconds, idle
  CPU under 1 second per hour, resident memory under 64 MiB.

### Federation (Slice 6)

- **Demand-driven.** A reader that wants peer data writes a watch lease,
  `projector/requests/watch-<pid>-<start>.json`, expiring 90 seconds ahead and
  renewed every 30: the console while a window is visible, `lanes --wait`
  while it runs. A local seat owning a non-terminal remote lane counts as a
  watcher at a 300-second cadence. With no live lease, no SSH session opens.
- While watched, each peer is pulled once per `observe.pull_interval_seconds`
  (default 60, 10% jitter). An explicit refresh (⌘R, `lanes --fresh`) writes a
  request file, honoured at most once per peer every 10 seconds.
- **Peer verb.** The peer entrypoint gains `observe.digest`, added to both
  the Python `parse_request` in `scripts/fabric_hosts.py` and the TypeScript
  peer handler in `runtime/fabric/src/peer-lanes.ts`. Like `telemetry.stats`
  it takes `{schema_version, since_generation}` and no `project_path`, which
  stays required for lane verbs only. It returns `unchanged`, or the peer's
  manifest, host record, lanes and cached quota report. Session lists are not
  in the digest.
- **Size.** Responses stay under the 1 MiB `MAX_RESPONSE_BYTES` cap: the lanes
  file holds non-terminal lanes and those finished in the last 24 hours, at
  most 500 rows; beyond that it sets `truncated: true` and the reader uses the
  legacy `lanes` verb for that host. A peer without a projector returns
  `projector_absent`, and the puller uses the `lanes` verb once per cadence.
- **Peer sessions.** `observe.sessions` returns a peer's session list in pages
  of 200 when the user opens that host in Sessions. `observe.session` returns
  one session record, with text only on request, and is refused unless this
  host's entry for that peer has `allow_content_reads`. Results are held in
  memory and never written to disk.
- An unreachable peer backs off exponentially to at most 10 minutes and is
  retried at once after a configuration change or a detected wake (the wall
  clock moving ahead of the monotonic clock by more than the cadence).

## Projector contract

JSON Schemas live in `schemas/projector/v1/`; fabricated fixtures in
`tests/fixtures/projector/v1/`, never copied from real sessions.

```text
$AGENT_FABRIC_STATE_ROOT/projector/          0700
  lock
  requests/                                  watch leases and refresh requests
  v1/
    manifest.json                            written last on every update
    host.json                                this host: id, revision, sources
    lanes.json                               lanes this host executes, its seats
    fleet.json                               lanes.json merged with peers
    statusline.json                          the statusline counters
    sessions/index.json                      session list, this host
    sessions/<provider>/<session>.json       provenant.session.context.v1
    quota.json                               this host's quota report, unchanged
    peers/<host>/{manifest,host,lanes,quota,fetch}.json
```

- Files are 0600, written by temporary file, flush and rename. The manifest
  holds per-file generation and hash; a reader accepts a file only when its
  generation matches, else rereads once. When idle the projector only touches
  the manifest's mtime every 30 seconds; an mtime older than 90 seconds means
  stale, and readers use the CLI.
- `lanes.json` holds `seats[]` and `lanes[]` with the identity fields,
  `state`, three-form `route`, timings, tokens, cost and `review_state`. A
  host serves only lanes it executes; the merge key `run_id@executing_host`
  prevents double counting. `fleet.json` adds `hosts[]` with status
  `ok|stale|unreachable|absent` and age.
- Each file names `provenant.projector.<name>.v<major>`; changes within a
  major are additive; a breaking change writes `v2/` beside `v1/` for at
  least one release; a peer major mismatch falls back to the `lanes` verb.

## Security and privacy

- **No new prompt logging.** Text is read on demand, held in memory for
  display, and returned by the CLI only with `--text`. The CLI and any
  projector read text in memory to size categories; they never persist it.
- **Privacy check.** A CI test plants sentinel strings in the message bodies,
  summaries and hook payloads of the fabricated fixtures, then asserts that no
  sentinel appears in any `--json` output without `--text`, in any contract
  file, in any log line, or in any peer response without
  `allow_content_reads`.
- **Egress.** Only probes to the provider's count endpoint when allowed;
  quota snapshots and digests to configured peers while watched; and session
  records to a peer allowed to read content. Digests hold identifiers,
  states, timings, counts, costs and hashes, with home-relative paths only.
- **Credentials.** The CLI, projector and console never read credential
  files, tokens or the Keychain, never open provider credential or account
  tables, and read Cursor's chat `meta` only for the named fields. The probe
  runs under the session's own identity directory.
- **Permissions.** Projector directories 0700 and files 0600; it refuses a
  symlinked or group- or world-writable directory. Seat files stay 0600.
- **Read-only sources.** SQLite opens `mode=ro` (`immutable=1` only for stores
  the provider holds open, labelled possibly stale). No source is repaired,
  migrated or truncated.

## Delivery plan

Tests follow the repository rule: heavy suites in CI, fast targeted checks
locally. Every slice is usable on its own.

### Slice 1: Sessions context and state, on demand

Scope: `provenant session list|context|compactions` for Claude Code and Codex
with the record schema and fabricated fixtures; the Claude Code probe behind
`observe.context.exact_counts`; `observability.json` with the `observe.context`
keys; the split into the menu bar agent and the console process, URL scheme,
`provenant console`, LaunchAgent migration, the module registry with only the
console and exact-count modules, and `provenant app menubar`; the console
window with the Sessions section (Overview, Context, State) reading the CLI.
Module switches are set through a minimal `provenant settings`
(list, get, set, unset for the module and `observe.context` keys) until the
Settings pane arrives in Slice 4. No projector, no seat-index change, no peer work.

Acceptance:
1. For fixture Claude Code and Codex sessions, the total matches the last
   recorded usage exactly, every figure has an exactness label and method,
   and transcript-only views show raw estimates plus the remainder.
2. With exact counts on, Count Exactly shows `probe_reconstructed`
   categories, leaves the transcript checksum unchanged and registers no
   seat; with it off, the command is refused and nothing is sent.
3. Each Claude Code compaction's injected text equals the `hook_success`
   content byte for byte, diffs come from successive payloads, and a changed
   on-disk file is flagged. Codex shows "encrypted by provider" and the
   `replacement_history` items.
4. The accuracy evidence run is attached; categories missing the bound show
   `unavailable`. The privacy check passes.
5. `provenant session context` p50 and p95 latency, cold and warm, on the
   user's largest sessions is recorded; it feeds the projector trigger.
6. Keyboard-only and VoiceOver walkthroughs of Sessions pass in light, dark
   and Increase Contrast.
7. With the menu bar off (`provenant app menubar off`) and the agent not
   running, `provenant console` opens the full console, and `provenant
   session`, `lanes`, `quota` and Fabric dispatch behave identically; no CLI
   or Fabric code path references the app.
8. Closing the last console window ends the console process within 2 seconds;
   the budgets above are recorded for the agent and the console; with
   `app.modules.exact_counts` off, no probe code runs and the switch's
   timers, watchers and caches are absent.

Tests: parser fixtures per provider; schema validation; the privacy test;
Swift view-model tests on recorded CLI JSON in a path-filtered macOS CI job;
a test of the Rust install path's label migration.

### Slice 2: Fleet and statusline, this host

Scope: seat-index extension and identity fields; `provenant statusline` and
the Fabric record of the first output read; the Fleet section reading
`provenant lanes --json`; the menu-bar activity line. Prerequisites: PR
#1085 merged and H1 landed.

Acceptance:
1. Scenarios S1, S2 and S5 attribute every lane to the right seat and
   project, and "Mine" shows only the viewer's lanes.
2. `provenant statusline` over a fixture state with 200 lanes finishes in
   under 150 ms at p95 over 20 runs in CI, and opens no SSH session.
3. A lane moves from running to in review when it finishes, and leaves in
   review when its owning seat first reads its output.

### Slice 3: telemetry

Scope: `user_version` 5 with three-form model and effort, preset and
`harness_rev` on usage rows; backfill; lane-to-PR and session-to-PR events;
the cost per accepted change rollup with contributor attribution and size
normalisation; the Telemetry section; OTLP export mapping.

Acceptance:
1. New usage rows carry preset, harness revision and three-form model and
   effort, or an explicit `unknown` or `inferred`.
2. For a fixture period, cost per accepted change reconciles with receipts,
   `events` and `changes`, and unattributed spend is reported.
3. A fixture pull request coded by three lanes and two sessions, with four
   review rounds, credits all five and counts four rounds; a second fixture
   with one small and one large pull request shows the per-line figure and
   size buckets so the large one is not overweighted; spend of a session
   linked to two pull requests is split by commits and sums to its total.
4. Exported OTLP carries the mapped `gen_ai.*` attributes and no content.

### Slice 4: accounts across hosts and settings

Scope: the `quota.snapshot` peer verb and `provenant quota --hosts all`; the
Accounts section; menu-bar headline across hosts; `provenant settings`
extended to the remaining keys; the Settings pane (including the Modules group and menu bar
switch) and host sheet. Remembered accounts are delivered by W17.

Acceptance:
1. An account on several hosts appears once, with the freshest observation
   per window and every host badge; a signed-out account shows its last
   reading; no raw window key, plan code, pool hash or adapter name shows.
2. With no viewer open for ten minutes, no `quota.snapshot` exchange happens.
3. Each settings key is accepted when valid, refused with the owner's typed
   error when invalid, and guarded by `--expect-rev`; peers can be added and
   removed through `settings`.

### Slice 5: projector, trigger-gated

Built only if, after Slices 1 to 4 have been in use for two weeks, one of
these holds:
- `provenant session context` p95 exceeds 1.5 s on sessions the user opens;
- Fleet refresh through `provenant lanes --json` exceeds 1 s at p95;
- `provenant statusline` exceeds 150 ms at p95 on the user's real state.

Scope: the ADR 0020 and Fabric hosts amendments (approved in principle, each
its own pull request, merged first); the Rust projector for this host,
a separate LaunchAgent that does not depend on the app; readers preferring the
contract.

Acceptance: the triggering measurement falls below its threshold; with the
projector stopped, every reader's output is unchanged; a new lane appears in
the console within 2 seconds of its run directory being created.

### Slice 6: federation, trigger-gated

First, without a projector, lengthen `lanes --wait`'s remote poll from 2 s to
30 s, taking today's roughly 450 SSH sessions per peer per 15 minutes to about
30. Federation is built only if, after that, a peer costs more than 120 SSH
sessions per hour while the console is open (counted at the transport), or
peer lanes take more than 2 s at p95 to appear in Fleet. Meeting this trigger
also opens Slice 5 (an exception to its gate), whose projector (leases,
manifests) is built and accepted first.

Scope: the three `observe.*` verbs, watch leases, merge, Hosts pages,
reworded topology docs, all behind `app.modules.federation`. Acceptance:
1. S3 and S4 attribute owning and executing host correctly on both hosts.
2. With no live lease for ten minutes, no SSH session opens; while watched,
   one peer costs at most 16 SSH sessions per 15 minutes.
3. An unreachable peer's lanes show `unreachable` with last state and age;
   `--wait` keeps them pending; local lanes are unaffected.
4. Zero, one and three peers work with no code change; a peer without a
   projector is shown through the `lanes` verb.
5. With `app.modules.federation` off, no `observe.*` timer, watch lease or
   SSH session exists in either process (quota merging is governed by
   `quota.merge_hosts`).

Tests use separate state roots and a fake `ssh` program (zero, one and three
peers, unreachable, version mismatch, truncated lanes) and count sessions.

### Slice 7: remaining providers

OpenCode, Gemini CLI and Copilot CLI parsers; agy after a decode spike that
confirms `gen_metadata` against receipt usage; Kiro and Cursor as lane-only.
Acceptance: each provider's row in the source table is met, or corrected with
evidence. Fixtures are fabricated from recorded structure, never from real
content.

## Rust pilot kill criteria

Measured at the end of Slice 5 against today's Python reading path
(`provenant session context` and `provenant lanes --json` on the same
fabricated corpus); no second implementation is built for the comparison.
The pilot stops, and the projector is reimplemented in Python behind the same
contract, if any of these holds:

1. **Less than 2x faster** in CPU time, for both a cold rebuild and the
   median incremental update, than the Python path producing the same
   records. Idle cost is reported alongside.
2. **More than 1.5x the CI repair cycles:** mean red runs before green over
   at least eight pilot pull requests, against at least eight Python and
   TypeScript pull requests merged in the same period. A smaller sample defers
   the measurement.
3. **More than 3 minutes added to median CI:** median wall time of required
   checks over 20 green runs after the first Rust job, against the 20 before.

The contract, fixtures and readers survive a stop unchanged.

## Migration from current hardcoding

From the host-agnostic audit of 2026-10-22. Executable code holds no host or
user names.

In flight in a separate pull request, referenced here and not designed here:

- **H1, one host identity owner.** Fabric defaults to `local` while telemetry
  and quota use the raw hostname; one `host_id()` owner with a slugified
  default replaces both. Every record uses it; Slice 2 depends on it.
- **M2** menu-bar launcher shell lookup; **M3** agy identity default gated by
  platform; **M4** Claude quota's Keychain call guarded on Linux; **M5** OTLP
  collector port and non-macOS behaviour; **L1** Node fallback paths in the
  Fabric launchers; **L3** Git lookup in `check-harness`.

Owned by this spec:

- **M1, peer quota.** The menu bar reads only local quota. Slice 4 adds the
  demand-driven cross-host merge described in
  [Accounts and quota](#accounts-and-quota); the report's presentation fields
  and remembered accounts come from W17.
- **Topology wording.** `fabric-hosts.md` and its runbook describe a fixed
  laptop and workshop pair, and the telemetry and model routing runbooks
  refer to "the workshop Mac". They are reworded so that pair is one example
  of zero or more peers, with an always-on peer as the recommended home host.
  Slice 6 ships this, or Slice 4 if Slice 6 never triggers.
- **Daemon amendments** to ADR 0020 and the Fabric hosts exclusion, as in
  [Relationship to existing decisions](#relationship-to-existing-decisions).
- **Test gaps** (zero-peer `stats --hosts all`, an empty `peers` lane path,
  three hosts, Linux markers) are covered by Slice 6's multi-host tests, or
  Slice 4's quota tests if Slice 6 never runs.

Noted, not owned here: M6 (Linux peer prerequisites), L2 (trusted
directories), L5 (Linux reflinks), L6 (per-host accounts), L7 (socket paths).

## Decisions

Settled by the user on 2026-10-22; none remain open.

1. **Exact context counts: off by default, opt-in per account.** The probe
   sends counted text to the provider's count endpoint under the session's own
   login, so the user enables it deliberately, per account, from a one-time
   prompt in the Context tab.
2. **Accepted change is a merged pull request, across contributors.** Many
   agents may code one pull request, and the user sometimes prefers a large
   branch to avoid the merge and verification churn of many small changes, so
   acceptance never assumes one agent or session per pull request. Attribution,
   normalisation and approximations are in [Telemetry rules](#telemetry-rules)
   rule 3.
3. **Background service amendment: approved in principle.** If the projector
   improves the app it may amend ADR 0020 and the Fabric hosts exclusion. It
   still goes through its ADR pull requests and the Slice 5 measurement
   trigger; no further user gate is needed beyond normal review.
4. **One app, not two, and modular.** The quota menu bar app becomes
   Provenant and the LaunchAgent becomes `dev.provenant.app`. The user wants
   it flexible and customisable (features are modules that can be switched
   off, costing nothing when off), with extra weight easily shed (separate
   targets), its memory freed when the full window closes while the menu bar
   stays light (two processes in one bundle), and fully working with the menu
   bar off (CLI, Fabric, telemetry and console independent of the agent). See
   [Processes and lifecycle](#processes-and-lifecycle), [Independence from the
   menu bar](#independence-from-the-menu-bar) and [Modules and build
   targets](#modules-and-build-targets).
