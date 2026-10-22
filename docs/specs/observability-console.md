# Spec: observability layer and native macOS console

Status: **proposed**, 2026-10-22, workstream W6. GitHub owns delivery state;
this file owns the durable requirements once accepted. It grants no authority
and relaxes no gate in `HARNESS.md`. It builds on
[Fabric across the user's own hosts](fabric-hosts.md), the
[operating model](harness/operating-model.md) (R4 telemetry, R6 continuity)
and the [profile telemetry stamp](harness/profiles.md#telemetry-stamp).

## Outcome

The user can see, from one native window, what every agent on every one of
their hosts is doing, what it costs, and what each session is carrying in its
context. In particular:

- who dispatched which lane, on which host, from which session, and its state;
- what a session's context window is made of, how that changed over time, and
  whether the state file it relies on after compaction recorded the right
  things;
- cost per accepted change, tokens and cache hit rate by model, effort,
  provider and preset;
- every account's quota on every host.

The data comes from a small local projector that turns files already on disk
into cheap digests. The console, the statusline and `provenant lanes` read
those digests instead of rescanning or polling peers over SSH.

## Goals

1. One identity model that attributes every lane and session correctly
   across sessions, projects and hosts.
2. A versioned, language-neutral digest contract, so the Rust projector, the
   Swift console and the Python and TypeScript readers evolve independently.
3. A per-session context breakdown that marks each number exact, estimated
   or unavailable, and shows each declared state file across compactions.
4. Federation by pull with zero or more peers, pluggable discovery and
   transport, degraded display offline, and about one SSH session per peer
   per minute instead of one per poll.
5. A native macOS console in the style of System Settings and Finder.
6. Telemetry rows that separate requested, resolved and observed model, and
   carry preset, harness revision, model and effort.

## Non-goals

- New prompt or tool-detail logging. Model input and output are read on
  demand from run files and transcripts already on disk. OTel prompt and tool
  detail exporters stay off.
- A network listener, HTTP API, broker or shared database. The projector
  writes local files; peers pull through the existing peer entrypoint.
- Writing to any provider store, Fabric store or the telemetry ledger from the
  projector or the console.
- Editing configuration files by hand from the console. Settings go through
  the CLI.
- A remote web board. A static HTML board for remote viewing may follow later
  and reads the same contract.
- Linux or Windows console builds. The CLI and projector run on Linux; the
  console is macOS only.
- Linux federation peers. [Fabric hosts](fabric-hosts.md#exclusions) still
  excludes them; nothing here may make them harder to add.
- Hosted observability or sending data to a third-party trace service.

## Relationship to existing decisions

- [ADR 0020](../adr/0020-retire-the-daemon-fabric.md) retired the daemon that
  sat on the coordination path, and [Fabric hosts](fabric-hosts.md#exclusions)
  excludes any daemon. The projector is different in kind: it is a derived
  cache, holds no authority, opens no port, and every reader falls back to its
  current direct read when the projector is absent or stale. Acceptance of
  this spec is recorded in a new ADR that states this exception narrowly.
- The telemetry ledger (R4) stays the single writer of usage rows. The
  projector reads it and may launch the existing `provenant telemetry ingest`
  command; it never writes the ledger itself.
- The `.agent-run/sessions/<session-id>/STATE.md` convention owned by the
  `session` skill is the default chair state file. This spec adds a way to
  declare it, not a second convention.

## Architecture

```text
 Host A (any OS; console on macOS only)                Host B (peer, optional)
 +----------------------------------------------+     +------------------------+
 | Sources (read-only)                          |     | same sources           |
 |  provider transcripts and stores             |     |                        |
 |  Fabric run dirs, fabric.sqlite3, lane-seats |     | provenant-projector    |
 |  telemetry.sqlite3, quota/report.json        |     |   writes projector/v1/ |
 |  declared state files                        |     |                        |
 |          | watch (kqueue / inotify)          |     | peer entrypoint        |
 |          v                                   |     |   verb projector.digest|
 |  provenant-projector  (Rust, no listener)    |     +-----------^------------+
 |   - parses and rolls up                      |                 |
 |   - pulls peer digests every ~60 s  ---------+-- ssh (pluggable transport)
 |   - launches `telemetry ingest`, debounced   |
 |          | atomic file writes                |
 |          v                                   |
 |  $STATE_ROOT/projector/v1/   (contract)      |
 |    manifest.json  fleet.json  sessions/ ...  |
 |    peers/<host>/...                          |
 |     |              |               |         |
 |     v              v               v         |
 |  Console app   statusline   provenant lanes  |
 |  (SwiftUI)     command      / --wait / stats |
 |     |                                        |
 |     +-- actions and settings --> provenant CLI --json --> owners
 +----------------------------------------------+
```

Reads flow down through the contract. Writes flow only through the CLI, which
delegates to the existing owners (Fabric dispatch owner, hosts configuration,
profile, quota and telemetry commands).

## Identity and attribution

### Fields recorded at dispatch

Every dispatch records, in its seat-index entry and its run receipt:

| Field | Meaning | Source |
|---|---|---|
| `seat` | The client process the Fabric MCP server acts for: `{host, pid, started_at, label}` | MCP server's parent process, as in PR #1085 |
| `seat_id` | `<pid>-<started_at>-<label>@<host>`, the seat directory name qualified by host | derived |
| `client` | MCP `clientInfo` name and version, and the provider session id when known | MCP initialise; session hook where available |
| `project` | Home-relative primary checkout path the lane works for | request, else seat's project |
| `worktree` | The linked worktree the lane writes in, or null for read-only lanes | dispatch owner |
| `owning_host` | Host of the dispatching seat, to which the lane reports | `seat.host` |
| `executing_host` | Host whose process runs the lane; authoritative for its run directory and liveness | dispatch owner on that host |
| `parent_lane` | Qualified id of the enclosing lane when a lane dispatches, else null | lane environment set by the dispatch owner |
| `operation_id` | Caller-generated idempotency key | existing remote dispatch rule |

Rules:

- **cwd decides write scope only.** It selects the worktree and confinement.
  It never decides ownership or project attribution on its own.
- **The seat decides ownership.** "Mine" in any view means lanes whose
  `seat_id` is the viewer's seat or a descendant through `parent_lane`.
- **Views default to everything on the host**, then merge peers. Narrowing to
  a seat, project or host is a filter, never a different data source.
- A CLI dispatch with no MCP seat in its ancestry takes the nearest ancestor
  provider process as its seat when it can identify one, else a `cli` seat for
  the invoking shell's session leader. `AGENT_FABRIC_LABEL` applies as before.
- Host identity comes from one owner (see
  [Migration](#migration-from-current-hardcoding), H1). No module derives a
  host id independently.

### Seat index, extended

PR #1085 adds `lane-seats/<pid>-<start>-<label>/<hash>.json` under the Fabric
state directory, holding `{run_dir, pid, started_at, label}` and written once
with mode 0600. This spec extends it additively:

- `lane-seats/<seat-dir>/seat.json`, written once by the MCP server after
  initialise: `schema`, `host`, `pid`, `started_at`, `label`, `client`, `cwd`,
  `project`, and `provider_session_id` when known.
- `lane-seats/<seat-dir>/state-file.json`, written by `provenant session
  declare` (see [Session state files](#session-state-files)).
- Each per-dispatch file gains `project`, `worktree`, `owning_host`,
  `executing_host`, `parent_lane` and `operation_id`. Readers treat missing
  fields as unknown, so files written before the change stay valid.
- A remote dispatch carries `seat_id` in the request. The executing host stores
  it in the receipt as `owner_seat` and does not create a seat directory for a
  seat it does not host.

The provider session id is joined from, in order: a hook that reports it (for
example Claude Code's SessionStart hook), the Fabric `sessions` table for
lanes (`provider_session_id`, `run_id`), then a match on cwd and process start
time against the first transcript entry, marked `estimated`.

### Scenarios

**S1. Two interactive sessions on one project, same Mac.** Two Claude Code
windows, one in the primary checkout and one in a linked worktree, each with
its own Fabric MCP server. Each MCP server records a different seat (different
client pid and start time; labels if set). Both resolve to the same `project`.
Lanes each window dispatches are indexed under its own seat directory, so
"Mine" in either window shows only its lanes, while Fleet shows both seats
under the one project with their lanes beneath. The worktree difference changes
only write scope.

**S2. Interactive sessions on different projects, same Mac.** A Codex chair in
project A and a Claude chair in project B. Seats differ and `project` differs.
A chair that dispatches a lane for project B while sitting in project A records
`project: B` from the request; ownership stays with its seat, and PR #1085's
`readSeatRuns` lets that seat see the lane even though its own cwd is project
A. Fleet groups by host, then seat, so both appear without either chair
needing the other's project.

**S3. A session on the workshop working with this Mac.** A chair runs on the
workshop and dispatches a lane to this Mac. The seat lives on the workshop:
`owning_host: workshop`, `seat_id: ...@workshop`. This Mac's dispatch owner
runs the lane: `executing_host: laptop`, and this Mac is authoritative for the
run directory and liveness. This Mac's projector lists the lane in its own
digest with `owner_seat` qualified by the workshop, and its console shows it
under a "remote seat" node for the workshop session. The workshop's console
sees it through its pull of this Mac's digest. Neither side probes the other's
processes.

**S4. A local session running lanes on the workshop.** A chair on this Mac
dispatches with `--host workshop`. The seat is local; `executing_host:
workshop`. The workshop's projector lists the lane in its digest; this Mac's
projector pulls that digest about once a minute and merges it into
`fleet.json`, so the lane appears under the local seat with a host badge. `lanes
--wait` on this Mac watches the local manifest instead of opening an SSH
session per poll. If the workshop goes offline, the lane shows `unreachable`
with its last known state and age, and the wait keeps it pending, as
[Fabric hosts](fabric-hosts.md#lanes-status-and-control) requires.

**S5. An SSH terminal into the workshop.** A session started in a terminal on
this Mac but running on the workshop over SSH is a workshop process. Its seat,
transcript and state file are the workshop's. Attribution follows where the
process runs, never where the keyboard is.

## Data sources per provider

Exactness: **exact** is a number the provider recorded; **estimated** is
derived by tokenising text already on disk; **unavailable** means nothing on
disk supports it. Identity directories (several per provider when accounts are
separated by `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or equivalent) come from the
telemetry configuration, never from fixed paths. Parsers key on the provider's
recorded version and count unknown entry types as `other` rather than failing.

| Provider | Store | Total context | Components | Compaction |
|---|---|---|---|---|
| Claude Code | `<identity>/projects/<slug>/<session>.jsonl`, subagents under `<session>/subagents/` | exact: `message.usage` input + cache read + cache creation, deduplicated by `message.id` | estimated from attachment and request-shape entries (below) | exact: `system` entry with `subtype: compact_boundary`, `compactMetadata.preTokens`, `postTokens`, trigger; summary is the next `user` entry with `isCompactSummary` |
| Codex | `<identity>/sessions/YYYY/MM/DD/rollout-*.jsonl`; `state_*.sqlite` (read-only) | exact: `token_count.info.last_token_usage`; window exact from `model_context_window` | estimated: `session_meta.base_instructions`, `world_state.agents_md`, skills and permissions in `world_state`, `response_item` messages and tool output; reasoning token count exact, content encrypted; tool definitions unavailable | exact: `compacted` entries with `replacement_history` (summary and retained items); pre and post totals from the neighbouring `token_count` events |
| OpenCode | `<identity>/opencode.db`, opened `mode=ro` | exact per message: `tokens.input`, `output`, `reasoning`, `cache.read`, `cache.write`, `cost` | estimated: messages and tool output from `part` text; instruction tables when populated; system prompt and tool definitions unavailable | exact: `part` of type `compaction`; summary is the assistant message with `summary: true` |
| Gemini CLI | `<identity>/tmp/<project>/chats/session-*.jsonl` | exact per turn: `tokens.input`, `cached`, `thoughts`, `tool`, `total` | estimated: `GEMINI.md` chain and messages; system prompt unavailable | unavailable until a marker is confirmed |
| agy | per-conversation SQLite with protobuf payloads | unavailable until the payloads are decoded; Fabric receipts give lane usage where the adapter reports it | unavailable | unavailable |
| Copilot CLI | `<identity>/session-state/<id>/events.jsonl`, `session-store.db` | exact: `currentTokens` and usage events | exact at compaction and shutdown: `systemTokens`, `toolDefinitionsTokens`, `conversationTokens` | exact: `session.compaction_start` and `compaction_complete` with `preCompactionTokens` and `summaryContent` |
| Kiro | `<identity>/sessions/<hash>/<id>/` | unavailable | unavailable | unavailable |
| Cursor | per-chat `store.db` with opaque blobs | unavailable; lane usage from Fabric receipts only | unavailable | unavailable |

The context window size comes from the provider when recorded (Codex) and
otherwise from `config/model-routing.json` `context`. The auto-compact buffer
is a harness setting, shown as configured, not observed.

Lane usage for every adapter also comes from Fabric attempt receipts
(`attempt.json`), which the ledger already ingests. A lane links to its
provider session through the Fabric `sessions` table.

### Claude Code context components

| Component | Entry used | Exactness |
|---|---|---|
| System prompt and harness | `prompt_snapshot` attachment, or `api-request-shape` system blocks resolved through `api-request-blob` | estimated; residual when absent |
| Built-in tool schemas | `api-request-shape` tools | estimated; residual when absent |
| Instruction files (CLAUDE.md, AGENTS.md chain, rules) | `instructions` attachment `files[]`, `nested_memory` | paths exact; tokens estimated |
| Memory index | the memory-type entry in `instructions` | estimated |
| Skills listing | `skill_listing` | skill count exact; tokens estimated |
| MCP tool schemas and instructions | `deferred_tools_record` (with `defer_loading`), `deferred_tools_delta`, `mcp_instructions_delta` | loaded versus deferred names exact; tokens estimated |
| Agents listing | `agent_listing_delta` | estimated |
| Conversation | user and assistant text blocks | estimated |
| Tool results | `tool_result` blocks and the session's `tool-results/` files | estimated |
| Files read into context | `file`, `edited_text_file`, `compact_file_reference` attachments | estimated |
| Compaction summaries | `isCompactSummary` entries | estimated; pre and post totals exact |
| Unattributed | exact total minus the sum of estimates | derived |

`/context` is reproducible in shape but not to the token: the total and free
space are exact, the components are estimates, and the buffer is the configured
value. Request-shape entries appear only in newer Claude Code versions; older
sessions fall back to attachments and a larger unattributed share.

### Estimation method

- The anchor is always the exact total for the turn. Component estimates are
  scaled so that, with the unattributed remainder, they sum to it; the console
  shows the remainder rather than hiding it.
- Tokens are estimated from UTF-8 byte length with a per-provider ratio,
  calibrated per model from sessions whose first turn has an exact total and
  known components. A tokeniser may replace the ratio per provider later; the
  contract records `estimator` and its version on each figure.
- Every figure in the contract carries `exactness: exact | estimated |
  unavailable` and, for estimates, the estimator id.
- The breakdown is recomputed at each assistant turn and at each compaction, so
  the series shows how each component grew and what compaction removed.
- Instruction files are attributed as recorded in the transcript, not as they
  are on disk now. Where Claude Code records `contentDiffersFromDisk`, the
  console says so.

## Session state files

A state file is a file a session relies on to restore itself after compaction:
the chair register, the `session` skill checkpoint, or a project-specific file.
The compaction summary itself is shown beside it, read from the transcript.

Declaration, in order of precedence:

1. **Explicit:** `provenant session declare --state-file <path> [--kind
   chair-register|session-checkpoint|other]`, or the equivalent Fabric MCP
   session field. It writes `lane-seats/<seat-dir>/state-file.json` with `path`
   (home-relative), `kind` and `declared_at`. A session may declare one file;
   a new declaration replaces the old.
2. **Convention:** a `.agent-run/sessions/<session-id>/STATE.md` whose `Chair
   session:` line names the provider session id.
3. **None:** the console shows "No state file declared" and the compaction
   summaries alone.

The projector watches the declared file and, at each compaction it detects in
the transcript and on each change, stores a content-addressed snapshot in its
private directory. The console shows the current content, the snapshot taken
at each compaction, a diff between any two snapshots, and the matching
compaction summary side by side, so the user can check whether the state file
recorded what the summary lost.

Snapshots never leave the host and are never copied into the digest. The
digest carries only path, size, modification time, snapshot hashes and
compaction ordinals.

## Telemetry rules

1. **Requested, resolved and observed** are separate fields for model and for
   effort:
   - `requested`: what the caller asked for (a route alias such as `senior`,
     a model, or nothing);
   - `resolved`: what the router chose (adapter, model id, effort);
   - `observed`: what the provider reported (`message.model`, `turn_context`
     effort, Claude's `requestedModel` and `effort` fields).
   A mismatch between resolved and observed is a reported finding, not
   silently merged.
2. **Every usage row carries** `preset` (with the profile digest),
   `harness_rev`, model (all three forms) and effort (all three forms). Today
   `harness_rev` lives only on `events` and `preset` inside `events.attrs`; the
   ledger gains these columns in a new schema version and backfills them by
   joining receipts. Native sessions without a receipt take the preset and
   revision current at the session's start, marked `inferred`.
3. **Cost per accepted change** is the headline cost figure. Spend attributed
   to lanes and sessions whose work landed in an accepted change, divided by
   accepted changes in the period. Attribution runs lane, branch, pull
   request, merge commit, using the ledger's `changes` table. Spend that
   attributes to no accepted change is shown beside it as a share, never
   hidden.
4. **Internal names stay Provenant's.** Mapping to OpenTelemetry `gen_ai.*`
   happens only on export: resolved model to `gen_ai.request.model`, observed
   model to `gen_ai.response.model`, provider to `gen_ai.provider.name`, token
   counts to `gen_ai.usage.*`, session to `gen_ai.conversation.id`; requested
   model, preset, effort and harness revision as `provenant.*` attributes.
   Export is off by default and never includes message content.

## Projector

### Responsibilities and inputs

- Watch sources and maintain per-host digests and rollups under the contract;
  pull peer digests and merge them; snapshot declared state files at
  compaction.
- Launch `provenant telemetry ingest` when ledger sources changed, debounced
  (default at most once every five minutes); read the ledger read-only.
- Serve nothing over a network and write nothing outside its own directory.
- Inputs: the Fabric state directory (`fabric.sqlite3`, `lane-seats/`), run
  roots of registered projects, provider identity directories from the
  telemetry configuration, `telemetry.sqlite3`, `quota/report.json`, declared
  state files and `hosts.json`. SQLite opens read-only and tolerates WAL
  writers.

### Implementation: the Rust pilot

The projector is the first Rust component, run as a pilot with
[kill criteria](#rust-pilot-kill-criteria).

- One binary, `provenant-projector`, one crate under `runtime/projector/`.
- Dependencies: `rusqlite` with the `bundled` feature (or the system library
  where packaging prefers it), `serde` and `serde_json`, a SHA-256
  implementation, and either `notify` or a native watcher. A native watcher is
  written only if kqueue and inotify together fit in about 400 lines with
  tests; otherwise `notify`. No async runtime, HTTP stack or CLI framework:
  threads, channels and a small argument parser.
- **Build versus depend.** Build narrow hot-path tools that Provenant owns
  end to end (watching, parsing, rolling up). Depend on established
  foundations: SQLite, cryptography, Git and tree-sitter-grade parsers.
  Never reimplement those.
- Platforms: macOS and Linux, built and tested in CI on Linux; macOS build
  checked in the console job.
- Supervision: a LaunchAgent `dev.provenant.projector` on macOS
  (`KeepAlive`, background process type, low-priority I/O) and a systemd user
  unit on Linux, both installed opt-in by `scripts/install-harness
  --projector`. `provenant projector run --foreground` runs it without a
  supervisor; `provenant projector status --json` reports health.
- One instance per state root, enforced by an advisory lock on
  `projector/lock`.
- Resource targets, measured in acceptance: a source change reflected in the
  digest within 2 seconds; idle CPU under 1 second per hour; resident memory
  under 64 MiB.

### Federation

- The projector pulls each peer whose `pull` is enabled, every
  `pull_interval_seconds` (default 60) with 10% jitter, through the configured
  transport. One exchange per peer per cadence.
- The peer entrypoint gains one read-only verb, `projector.digest`, taking
  `since_generation`. It returns `unchanged`, or the peer's digest files,
  bounded at 1 MiB. A peer without a projector returns `projector_absent`; the
  puller then uses the existing `lanes` verb once per cadence instead.
- Pulled files land under `projector/v1/peers/<host>/` with a `fetch.json`
  recording status, time and error class.
- An unreachable peer backs off exponentially to at most 10 minutes and is
  retried at once after a configuration change or a detected wake (the wall
  clock moving ahead of the monotonic clock by more than the cadence).
- `--wait` follows the cadence and never forces a pull. An explicit fresh
  read (`lanes --fresh`, ⌘R in the console) drops a request file under
  `projector/requests/`; the projector pulls within a second, at most once per
  peer every 10 seconds, and removes it. No socket is involved.
- Expected cost: about 15 SSH sessions per peer per 15 minutes, against about
  450 for today's `lanes --wait`.

## Projector contract

The contract is the only interface between the projector and its readers.
JSON Schemas live in `schemas/projector/v1/`; golden fixtures in
`tests/fixtures/projector/v1/` are fabricated, never copied from real sessions.

### Layout

```text
$AGENT_FABRIC_STATE_ROOT/projector/          0700
  lock
  requests/                                  pull requests from readers
  private/                                   state-file snapshots, never served
  v1/
    manifest.json                            written last on every update
    host.json                                this host: id, revision, sources
    lanes.json                               lanes this host executes, its seats
    fleet.json                               lanes.json merged with all peers
    statusline.json                          three counters, all hosts
    sessions/index.json                      session list, this host
    sessions/<provider>/<session>.json       one session's detail
    rollups/telemetry-daily.json             tokens, cache, cost by key
    rollups/accepted-changes.json            cost per accepted change
    quota.json                               this host's quota summary
    peers/<host>/{manifest,host,lanes,quota}.json
    peers/<host>/sessions/index.json
    peers/<host>/fetch.json
```

All files are mode 0600 and written by temporary file, flush and rename.

### Manifest and consistency

`manifest.json`:

```json
{
  "schema": "provenant.projector.manifest.v1",
  "host": "laptop",
  "generation": 4182,
  "generated_at": "2026-10-22T03:14:07Z",
  "heartbeat_at": "2026-10-22T03:15:07Z",
  "projector_rev": "0.1.0+79396f51",
  "files": {"fleet.json": {"generation": 4182, "sha256": "..."}},
  "sources": [{"kind": "claude", "status": "ok", "lag_ms": 310}]
}
```

A reader reads the manifest, then the files it needs, and accepts a file only
when its `generation` matches the manifest's entry; otherwise it rereads once.
The heartbeat advances every 30 seconds when idle. A manifest older than three
heartbeats is stale: readers say so and fall back.

### Records

`lanes.json` holds `seats[]` and `lanes[]`, each lane with the identity
fields above plus `state`, `status`, `route` (requested, resolved, observed),
`started_at`, `ended_at`, token totals, estimated cost and `review_state`
(`pending`, `seen`, `adjudicated`). A host serves only lanes it executes, and
the merge key is `run_id@executing_host`, so nothing is counted twice.
`fleet.json` adds `hosts[]` (id, status `ok|stale|unreachable|absent`,
`observed_at`, age), and peer rows carry their host's status and age.

`sessions/<provider>/<session>.json` holds identity (`host`, `provider`,
`session_id`, `seat_id` when joined, `project`, `cwd`, `branch`), `window`
(size, buffer, source), `turns[]` (timestamp, exact total, components with
exactness), `compactions[]` (ordinal, timestamp, trigger, pre and post totals,
transcript locator for the summary), and `state_file` (path, kind, size,
modification time, snapshot hashes keyed by compaction ordinal). It carries
locators (file and byte offset or row id), never message text.

`statusline.json`:

```json
{"schema": "provenant.projector.statusline.v1", "generation": 4182,
 "lanes": 5, "in_review": 2, "needs_you": 1, "hosts_unreachable": 0}
```

- `lanes`: non-terminal lanes on all hosts.
- `in_review`: finished lanes whose result is not yet adjudicated, plus open
  pull requests from lane branches.
- `needs_you`: lanes in `input_required` and Fabric tasks or messages marked
  as a user gate.

### Versioning

- Each file names its schema, `provenant.projector.<name>.v<major>`.
- Within a major version changes are additive; readers ignore unknown fields.
- A breaking change writes `v2/` beside `v1/` for at least one release, so
  readers move one at a time.
- A reader that finds no supported major version falls back to its current
  direct path (`provenant lanes --json`, `provenant quota --json`, transcript
  reads) and says it did.
- The peer verb exchanges the major version; a mismatch is flagged and the
  peer is shown from the legacy `lanes` verb.

### Readers

- **Console (Swift):** `Codable` types written against the schemas and
  checked by the fixture tests; a `DispatchSource` watch on `manifest.json`.
- **`provenant lanes`, `lanes --wait`, `stats` (Python and TypeScript):** read
  the digest when fresh; otherwise today's code path unchanged.
- **Statusline:** `provenant statusline` reads only `statusline.json`. It opens
  no SQLite database and no SSH session, and finishes in under 20 ms.

## Console

### Form

- A window scene in the existing menu-bar app, opened by an "Open Console"
  item in the panel (⌘O) and by `provenant console`. While a console window is
  open the app shows in the Dock and the app switcher; closing it returns the
  app to menu-bar only.
- `NavigationSplitView` with a source-list sidebar (`.listStyle(.sidebar)`),
  SF Symbols, a toolbar, and an inspector (`.inspector`) for selected items.
  Three columns where a section has a list (Sessions), two plus inspector
  where it has a canvas (Fleet).
- System appearance, accent colour, semantic colours and materials only;
  system text styles so Dynamic Type applies where practical.
- Keyboard: ⌘1 to ⌘5 select sections, arrows move in lists and the graph,
  Space toggles the inspector, ⌘F filters, ⌘R pulls, ⌘, opens Settings.
- VoiceOver: every graph node and chart has an accessibility label and value
  that state the content in words (for example "Lane 4f2c, Codex senior,
  running on workshop for 12 minutes, owned by this session"). Status is shown
  by symbol and text as well as colour. The graph has a list equivalent.
- Minimum macOS 14, matching the menu-bar app.
- The console reads only the contract. Actions (cancel a lane, request a
  pull, change a setting) run `provenant ... --json` off the main thread with
  a timeout, and show the CLI's typed result.

### Sidebar

| Group | Item | Symbol |
|---|---|---|
| Activity | Fleet | `point.3.connected.trianglepath.dotted` |
| Activity | Sessions | `text.bubble` |
| Insight | Telemetry | `chart.bar.xaxis` |
| Insight | Accounts | `gauge.with.dots.needle.50percent` |
| (bottom) | Settings | `gearshape` |

A host status strip sits under the sidebar list: one row per host with a dot
and age ("workshop · 40 s"), and "This Mac only" when no peers are configured.

### Detail panes

**Fleet.** A layered graph: host columns, seats as roots, lanes beneath their
seat or parent lane, edges for dispatch. Each node shows provider symbol,
route, state and elapsed time. Toolbar: scope (All hosts, This host, Mine),
project filter, state filter, Graph or List toggle. The inspector for a lane
shows identity fields, requested, resolved and observed route, worktree and
branch, timings, tokens and cost, review state, the first lines of
`result.md` read on demand (bounded `output` read for a remote lane), and
actions: Reveal in Finder (local only), Copy ID, Cancel (with confirmation).

**Sessions.** A list grouped by host then project: provider symbol, project
and branch, last activity, context used as a percentage of the window,
compaction count, and a state-file badge. The detail has a segmented control:

- *Overview:* identity, seat, lanes it owns, model and effort (three forms),
  totals.
- *Context:* current breakdown as a stacked bar and a table (component,
  tokens, share, exactness badge, change since the last compaction); a chart
  of total context per turn with compaction markers; an area chart of
  components over time; free space and buffer.
- *State:* the declared state file's path and kind, its current content
  rendered read-only, a snapshot picker by compaction, a diff between two
  snapshots, and the compaction summary for the selected compaction beside it.
  Summary text is read from the transcript on demand and not retained.
- *Usage:* per-turn tokens, cache read and write, cost.

**Telemetry.** Headline tiles: cost per accepted change, accepted changes,
unattributed spend share, cache hit rate. A table and chart grouped by any of
model, effort, provider, preset, harness revision and host, with a period
picker. A findings list of resolved-versus-observed mismatches.

**Accounts.** The quota report for every host (see M1 in
[Migration](#migration-from-current-hardcoding)): one row per account with a
host badge, binding window, reset, pace and staleness, and history from the
ledger's `quota_series`.

**Settings.** Forms for the keys in the [settings model](#settings-model),
grouped as Hosts, Projector, Console, Telemetry and Profile. Each field is
validated through the CLI as it changes and saved through the CLI on Save.

### Main window wireframe

```text
+------------------------------------------------------------------------------+
| (o)(o)(o)          Fleet        [All hosts v] [Project v] [Graph|List] (Q)   |
+-------------------+----------------------------------------------+-----------+
| ACTIVITY          |  laptop                     workshop         | Lane 4f2c |
|  > Fleet          |  +---------------+         +--------------+  |-----------|
|    Sessions       |  | claude  chair |-------->| codex senior |  | Route     |
| INSIGHT           |  |  this session |         |  running 12m |  |  senior   |
|    Telemetry      |  +-------+-------+         +--------------+  |  -> codex |
|    Accounts       |          |                                   |  gpt high |
|                   |   +------v-------+  +--------------+         | Host      |
|                   |   | opencode jr  |  | claude rev.  |         |  workshop |
|                   |   |  done  4m    |  |  in review   |         | Owner     |
|                   |   +--------------+  +--------------+         |  laptop   |
|                   |                                              | Tokens    |
|-------------------|                                              |  41k  $0.3|
| * laptop    now   |                                              | [Cancel]  |
| * workshop  40 s  |                                              |           |
|   Settings        |                                              |           |
+-------------------+----------------------------------------------+-----------+
```

### Sessions and Context wireframe

```text
+------------------------------------------------------------------------------+
| (o)(o)(o)          Sessions                  [Overview|Context|State|Usage]  |
+-----------------+------------------------+-----------------------------------+
| ACTIVITY        | laptop                 | provenant  docs/branch   claude   |
|   Fleet         |  provenant             | 142k of 250k (57%)  3 compactions |
| > Sessions      |  > claude  57%  [S] 3c |                                   |
| INSIGHT         |    codex   31%      0c | [####|##|#|###|########|####|   ] |
|   Telemetry     |  other-project         |  sys instr skl mcp  conv  tools   |
|   Accounts      |    claude  12%  [S] 0c |                                   |
|                 | workshop  (40 s)       | Component       Tokens  Exact    d|
|                 |  provenant             | System, harness  14.2k  est      0|
|                 |    codex   44%      1c | Instructions      6.1k  est   +0.4|
|                 |                        | Memory index      0.9k  est      0|
|                 |                        | Skills listing    6.5k  est      0|
|                 |                        | MCP 9/57 loaded   4.8k  est   +1.2|
|                 |                        | Conversation     38.0k  est    +21|
|                 |                        | Tool results     61.3k  est    +40|
|                 |                        | Compact summary   3.2k  est      -|
|                 |                        | Unattributed      7.0k  -        -|
|                 |                        | Total           142.0k  exact     |
|                 |                        |                                   |
|                 |                        |  ctx |    /|    /|      /         |
|                 |                        |      |  /  |  /  |    /           |
|                 |                        |      |/    |/    |  /   C: compact|
|                 |                        |      +--C-----C-----C----- turns  |
+-----------------+------------------------+-----------------------------------+
```

`[S]` marks a declared state file; `3c` is the compaction count.

## Statusline

`provenant statusline` prints lanes, in review and needs you, for example
`lanes 5 | review 2 | needs you 1`, and appends `| 1 host offline` only when
one is. It shows no usage or quota; the menu bar owns accounts. It is the
command a harness's status-line hook runs, and reads only `statusline.json`;
without a fresh digest it prints nothing rather than scanning.

## Settings model

Settings are validated and saved only through the CLI. The console and any
script use the same commands:

```text
provenant settings list [--json]          keys, values, defaults, sources, revisions
provenant settings get <key> [--json]
provenant settings validate <key>=<value>... --json        dry run, typed errors
provenant settings set <key>=<value>... [--expect-rev R] --json
```

`settings` is a facade. Each key belongs to an existing owner, which keeps its
own file, validator and command; `settings set` calls that owner and writes
atomically. `--expect-rev` refuses a write when the file changed since it was
read, and the console then reloads and shows the change.

| Key | Owner file | Type and default |
|---|---|---|
| `hosts.local_host` | `hosts.json` | name matching `[a-z0-9-]+`; default from the host identity owner |
| `hosts.peers.<name>.transport.kind` | `hosts.json` | `ssh` (only value in version 1) |
| `hosts.peers.<name>.transport.ssh_destination` | `hosts.json` | string, required for `ssh` |
| `hosts.peers.<name>.transport.peer_command` | `hosts.json` | string; default `.local/bin/provenant peer` |
| `hosts.peers.<name>.connect_timeout` | `hosts.json` | seconds, 1 to 60, default 5 |
| `hosts.peers.<name>.response_deadline` | `hosts.json` | seconds, 1 to 120, default 15 |
| `hosts.peers.<name>.pull` | `hosts.json` | bool, default true |
| `hosts.peers.<name>.pull_interval_seconds` | `hosts.json` | 15 to 3600; default from `observe.pull_interval_seconds` |
| `hosts.discovery.kind` | `hosts.json` | `static` (version 1) |
| `observe.projector.enabled` | `observability.json` | bool, default true once installed |
| `observe.pull_interval_seconds` | `observability.json` | 15 to 3600, default 60 |
| `observe.ingest_min_interval_seconds` | `observability.json` | 60 to 86400, default 300 |
| `observe.retention_days` | `observability.json` | 1 to 365, default 14 |
| `observe.state_snapshots.max_per_session` | `observability.json` | 0 to 200, default 20; 0 disables |
| `observe.context.estimator` | `observability.json` | `byte-ratio` (version 1) |
| `observe.export.otlp.enabled` | `observability.json` | bool, default false |
| `observe.export.otlp.endpoint` | `observability.json` | URL, loopback by default |
| `console.default_scope` | `observability.json` | `all-hosts`, `this-host` or `mine`; default `all-hosts` |
| `console.quota_scope` | `observability.json` | `all-hosts` or `this-host`; default `all-hosts` |
| `profile.preset` and knobs | profile file | as `provenant profile` |
| `telemetry.accounts.*` | `telemetry.json` | as the telemetry runbook |

`observability.json` lives at `<instance-root>/config/observability.json`
beside `telemetry.json`. Unknown keys are errors. A missing file means all
defaults.

## Host-agnostic configuration

The host registry is the existing `hosts.json`, extended additively within
schema version 1 (a peer that has only `ssh_destination` is read as
`transport.kind: ssh`). No host name appears in code; names exist only in this
file and in test fixtures.

Zero peers (or no file at all, which behaves the same):

```json
{
  "schema_version": 1,
  "local_host": "mini",
  "peers": {},
  "projects": {}
}
```

With zero peers the projector pulls nothing, the console's host strip reads
"This Mac only", the statusline never shows offline hosts, and every command
behaves as on a single machine.

Two peers:

```json
{
  "schema_version": 1,
  "local_host": "laptop",
  "discovery": {"kind": "static"},
  "peers": {
    "workshop": {
      "transport": {"kind": "ssh", "ssh_destination": "workshop.tailnet",
                    "peer_command": ".local/bin/provenant peer"},
      "connect_timeout": 5,
      "response_deadline": 15,
      "pull": true
    },
    "studio": {
      "transport": {"kind": "ssh", "ssh_destination": "studio"},
      "pull": true,
      "pull_interval_seconds": 300
    }
  },
  "projects": {
    "Repos/provenant": {"default_host": "workshop"}
  }
}
```

Extension points:

- **Transport** is an interface with one operation, `exchange(verb, request,
  deadlines) -> response`, and typed errors (`unreachable`, `timeout`,
  `protocol_mismatch`, `too_large`). `ssh` is the first implementation. A
  `local-exec` implementation, which runs a peer entrypoint as a subprocess
  with its own state root, exists for tests.
- **Discovery** proposes candidate peers and never adds one by itself. `static`
  reads the file. A later discoverer (for example from a tailnet's status
  output) offers candidates that the user adds with `provenant hosts add`.
- **Platform** differences sit behind the projector's watcher and the
  supervisor installer. Paths derive from `AGENT_FABRIC_STATE_ROOT`,
  `AGENT_FABRIC_INSTANCE_ROOT` and provider identity directories.

## Security and privacy

- **No prompt logging.** Nothing in this spec records prompts, model output or
  tool detail that is not already on disk. Session detail files carry
  locators, not text. Text is read on demand from transcripts and run files
  and held only in memory for display.
- **Nothing leaves the host except digests to configured peers.** A digest
  holds identifiers, states, timings, counts, costs and hashes. It excludes
  message text, state-file content, compaction summaries, file paths outside
  the user's home (home-relative paths only) and credentials.
- **Remote content on demand only.** Reading a remote lane's result uses the
  existing bounded `output` verb. Reading a remote session's state file or
  compaction summary is a separate open question below.
- **Credentials.** The projector and console never read credential files,
  tokens or the Keychain, and open only the session stores named above.
- **Permissions.** Projector directories 0700, files 0600; it refuses to start
  if its directory is a symlink or group or world writable. Seat files stay
  0600 as in PR #1085.
- **No listener.** No socket. Readers signal through `requests/` files,
  validated by name pattern.
- **Read-only sources.** SQLite opens with `mode=ro`; transcripts read-only.
  No source is ever repaired, migrated or truncated.
- **Retention.** Digest history and state-file snapshots expire after
  `observe.retention_days`. `provenant clean` covers the projector directory.
- **Export.** OTLP export is off by default, loopback by default when on, and
  carries no content attributes.

## Delivery plan

Each slice is usable on its own. Tests follow the repository rule: heavy suites
in CI, fast targeted checks locally.

### Slice 1: local Fleet and Sessions, read-only

Scope: seat-index extension and identity fields (building on PR #1085); the
contract v1 schemas and fixtures; the Rust projector for this host only
(lanes, seats, sessions for Claude Code and Codex, state-file snapshots,
statusline); `provenant statusline`; `provenant session declare`; the console
window with Fleet and Sessions (Overview, Context, State), local only.

Acceptance:
1. Scenarios S1 and S2 attribute every lane to the right seat and project, and
   "Mine" in each session shows only its own lanes.
2. A Claude Code and a Codex session each show a context breakdown whose total
   matches the transcript's last recorded total exactly, with every component
   badged exact or estimated and the unattributed remainder shown.
3. After a compaction, the State tab shows a new snapshot of the declared state
   file, a diff against the previous one, and the compaction summary beside
   it.
4. A new lane appears in the console within 2 seconds of its run directory
   being created.
5. With the projector stopped, `provenant lanes` output is unchanged from
   today, and the console shows a stale banner and reads `provenant lanes
   --json`.
6. Console passes a keyboard-only walkthrough and a VoiceOver walkthrough of
   Fleet and Sessions, in light and dark appearance and at a larger text size.

Tests: Rust unit tests and contract tests that build digests from the
fabricated fixtures and compare them with the expected JSON; schema validation
of every fixture; Python and TypeScript reader tests against the same
fixtures; Swift view-model tests (`swift test`) decoding the fixtures, in a
path-filtered macOS CI job; scripted keyboard and VoiceOver checklist recorded
in the pull request.

### Slice 2: federation

Scope: `projector.digest` peer verb; pull loop with backoff and wake retry;
`fleet.json` merge; pull requests by file; `lanes --wait` on the digest; host
strip and degraded display in the console.

Acceptance:
1. Scenarios S3, S4 and S5 attribute owning and executing host correctly on
   both hosts.
2. With one peer, `lanes --wait --all` over 15 minutes opens no more than 16
   SSH sessions to it, counted at the transport.
3. With the peer unreachable, its lanes show `unreachable` with last state and
   age, `--wait` keeps them pending, and local lanes are unaffected.
4. Zero, one and three configured peers all work with no code change.
5. A peer without a projector is shown through the legacy `lanes` verb.

Tests: multi-host tests on the `local-exec` transport with separate state
roots (zero, one and three peers, an unreachable peer, a version mismatch); a
counted-session test on the transport; the existing Fabric hosts suites
unchanged.

### Slice 3: telemetry

Scope: ledger schema version with requested, resolved and observed model and
effort, `preset`, `harness_rev`; backfill; cost per accepted change rollup;
Telemetry pane; OTLP export mapping.

Acceptance:
1. Every usage row ingested after the change carries preset, harness revision,
   and all three model and effort forms, or an explicit `unknown` or
   `inferred`.
2. For a known period, cost per accepted change reconciles with receipts and
   the `changes` table, and unattributed spend is reported.
3. Exported OTLP passes a schema check for the mapped `gen_ai.*` attributes and
   contains no content attributes.

Tests: ledger migration and backfill tests on fabricated rows; rollup
golden tests; an export test that asserts the attribute set.

### Slice 4: accounts and settings

Scope: peer quota in the digest, Accounts pane, menu-bar headline across hosts
(M1); `provenant settings` facade and the Settings pane.

Acceptance:
1. The menu bar and Accounts pane show every host's accounts with a host
   badge, and `console.quota_scope: this-host` restores today's view.
2. Every setting in the table can be changed from the console, is refused with
   the owner's typed error when invalid, and is never written by the console
   itself.
3. A concurrent change to the same file is detected by `--expect-rev` and
   shown.

Tests: `settings` facade tests per owner (valid, invalid, concurrent);
console view-model tests against recorded CLI JSON.

### Slice 5: remaining providers

Scope: context and compaction parsers for OpenCode, Gemini CLI and Copilot
CLI; agy once its payloads are decoded; Kiro and Cursor as lane-only.

Acceptance: each provider's row in the data-source table is met or the table
is corrected with evidence.

Tests: per-provider fixtures fabricated from the recorded structure, never from
real content.

Later: a static HTML board on the same contract for remote viewing, and
further transports and discoverers.

## Rust pilot kill criteria

Measured at the end of slice 2 against a reference implementation of the same
contract in Python (the slice 1 reader fixtures double as its tests). The pilot
is stopped, and the projector reimplemented behind the same contract in
Python, if any of these holds:

1. **Less than 2x faster.** On a fixed fabricated corpus, the Rust projector
   is less than twice as fast as the reference for both a cold rebuild and the
   median incremental update. CPU time is the measure; idle cost is reported
   alongside.
2. **More than 1.5x the CI repair cycles.** Across the pilot's pull requests,
   the mean count of red CI runs before green per pull request exceeds 1.5
   times the mean for Python and TypeScript pull requests in the same period.
3. **More than 3 minutes added to median CI.** The median wall time of the
   required CI checks rises by more than 3 minutes over the 20 green runs
   before the first Rust job, measured over 20 green runs after.

The contract, fixtures and readers survive a stop unchanged; only the writer
is replaced.

## Migration from current hardcoding

From the host-agnostic audit of 2026-10-22. Code paths are largely
host-agnostic: no host names or user names appear in executable code.

In flight in a separate pull request, referenced here, not designed here:

- **H1, one host identity owner.** Fabric defaults the local host to `local`,
  while telemetry and quota use the raw hostname, which can fail the host-name
  pattern and collide in merged ledgers. One `host_id()` owner, one default
  (a slugified hostname matching the pattern), validated everywhere. This spec
  depends on it: every digest and identity field uses that owner.
- **M2** menu-bar launcher shell lookup; **M3** agy identity default gated by
  platform; **M4** Claude quota's keychain call guarded on Linux; **M5** OTLP
  collector port and non-macOS behaviour; **L1** Node fallback paths in the
  Fabric launchers; **L3** Git lookup in `check-harness`.

Owned by this spec:

- **M1, peer quota.** The menu bar reads only local quota, although quota
  snapshots are already federated. Slice 4 adds peer quota to the digest, the
  Accounts pane and the menu-bar headline, with `console.quota_scope` and a
  host badge.
- **Topology wording.** `fabric-hosts.md` and its runbook describe the
  topology as fixed to a laptop and a workshop, and the telemetry and model
  routing runbooks refer to "the workshop Mac". They are reworded so the
  laptop and workshop pair is one example of zero or more peers, with an
  always-on peer as the recommended home host. Slice 2 ships this.
- **Test gaps.** Zero-peer `stats --hosts all`, an empty `peers` lane path,
  three hosts, and Linux markers are covered by slice 2's multi-host tests.

Noted, not owned here: peer requirements for Linux or a custom install prefix
(M6), trusted directory lists (L2), Linux reflink behaviour (L5), per-host
account configuration (L6) and the BSD socket-path limit (L7).

## Open questions

1. **One app or two?** The console could be a window in the existing
   menu-bar app or a separate app. *Recommendation:* one app, renamed from
   "quota menu bar" to Provenant, with a one-time LaunchAgent label migration;
   one process, one login item, one place to look.
2. **Remote session content.** Reading a peer session's state file or
   compaction summary means that host sends content, not just a digest.
   *Recommendation:* allow it only on an explicit click, through a bounded
   read-only peer verb, never cached on disk, and off by default per peer
   behind `hosts.peers.<name>.allow_content_reads`.
3. **State-file snapshots.** Diffs between compactions need local copies of
   an untracked file. *Recommendation:* keep them, local only, 20 per session
   for 14 days, configurable to zero.
4. **Daemon exception.** The projector is a long-running process, which ADR
   0020 and the Fabric hosts exclusions rule out for coordination.
   *Recommendation:* accept it through a new ADR as a derived cache with no
   authority, no listener and mandatory reader fallback, so the exception
   cannot grow into a coordination daemon.
5. **What counts as an accepted change?** *Recommendation:* a pull request
   merged through the repository's own workflow is the denominator for cost
   per accepted change; R4.3 accepted tasks without a pull request are a
   second, separately labelled figure.
