# Agent fabric operations

Status: current pre-release operations; query live machine state before action
Applies to: `runtime/agent-fabric` and `scripts/agent-fabric*`

## User gates

The following remain separate approvals. One does not imply another:

1. build or install the local runtime outside an active implementation envelope;
2. trust any parent, wildcard, home, sibling collection or other root beyond
   the exact current project, or provision/rotate operator and agent seats;
3. enable a provider adapter after compatibility verification;
4. install an auto-start/login service for the daemon;
5. log into or consume quota from a provider;
6. change or remove a client registry entry;
7. run a smoke that invokes a real provider adapter;
8. accept the implementation, release it or publish Git state.

Standing global harness authority covers only automatic trust registration for
the exact current project's canonical Git root, or its canonical current
directory when no repository exists. This trust grants no task, write,
credential, provider, provisioning, acceptance or release authority. Seat
provisioning remains separately gated.

Read the active authority before acting. Prior activation evidence does not
authorise a different or broader root, login, registry mutation, provider call,
acceptance, release or publication.

## Preflight

```sh
npm ci --no-audit --no-fund
npm run build
npm run check
npm run test:evaluation
npm run test:load
npm audit --omit=dev --audit-level=high
scripts/check-harness
git diff --check
python3 skills/deliver/scripts/validate_delivery.py \
  '<canonical-run>/RUN.json' --workspace-root "$PWD" --verify-hashes
```

Then verify the selected compatibility entries. Fabric library and protocol
schema artifacts are checked against their pinned hashes. Provider CLI versions
and digests are observed evidence, not admission locks.
Repository-owned wrapper code carries Git provenance instead of a hash pin
(`runtime/agent-fabric/src/adapters/compatibility.ts`): the wrapper
entrypoint must resolve inside a Git repository, be tracked at HEAD and
byte-identical to its committed content, and its first-party source spans
(the owning workspace package's src tree, local workspace dependency src
trees and every consulted package manifest) must be diff-clean against HEAD.
An empty or truncated span discovery is a hard verification failure, never a
skip. Provenance, adapter-specific vendor identity and the bounded non-answer
provider interface are required at activation and revalidated at point of use.
Unresolved contract gaps, missing artifacts, disabled entries or any
provenance/identity/interface mismatch fail closed.

## Keep the CLI dist warm

`scripts/agent-fabric` and `scripts/agent-fabric-mcp` execute the compiled
dist and fall back to the tsx loader only when the dist is absent or older
than the TypeScript sources; the fallback adds noticeable per-invocation
latency. After integrating runtime changes into the main checkout (the
post-merge sync in [the GitHub workflow
runbook](github-workflow.md#after-merge)), run:

```sh
scripts/agent-fabric-warm
```

It is a no-op when the dist is fresh and runs the workspace build only when
stale, so normal operation never hits the fallback path.

## Live discovery and registrations

Read workstation-specific run, roster, expiry, adapter and socket state from
the machine interface. Do not copy it into this runbook:

```sh
scripts/agent-fabric status --json --project "$PWD"
scripts/agent-fabric doctor --json
```

`doctor` reports one typed overall state and exits successfully for both normal
operating modes:

| `state` | `code` | `healthy` | Meaning |
| --- | --- | --- | --- |
| `idle` | `DAEMON_ON_DEMAND_IDLE` | `true` | Configuration, compatibility, private paths, database and election state pass; no daemon is expected to be running. `daemon.pid` and `daemon.socketPath` are `null`. |
| `current` | `DAEMON_LIVE` | `true` | Generation-bound discovery, bootstrap election, process, owned Unix socket, authenticated negotiation and a non-mutating bootstrap-scope contract probe agree. |
| `recovering` | converging transition or unreachable-daemon residue | `false` | The next ordinary bootstrap reconciles this locally and reversibly under existing authority. The command exits non-zero. |
| `blocked` | every other causal failure code | `false` | Repair needs user authority, material state displacement, or an action outside this lifecycle. The command exits non-zero. |

Every state is causal: `cause` names the deciding check, the precondition that
check asserts, whether that precondition is satisfied, and whether this
lifecycle may repair it. The deciding check is the worst one — a failure first,
otherwise an unresolved check, otherwise the daemon summary. `cause.satisfied`
reports that check alone and is independent of `healthy`: only a `pass` check
is satisfied, so a healthy idle lifecycle reports an unsatisfied daemon
precondition (no daemon is expected), and an unknown provider probe stays
advisory without being called satisfied.

`recovering` is deliberately narrow, and covers only states an ordinary
bootstrap actually converges: an active bootstrap or shutdown transition
another owner is completing, active discovery whose PID is dead, and terminal
crashed or unclean-stop discovery that the ordinary spawn path replaces. Absent
discovery, a stale socket and an unreachable socket under a live PID are
`blocked`, because reconciliation returns false for all three and bootstrap
then raises `BOOTSTRAP_RECONCILIATION_REQUIRED` or `BOOTSTRAP_READY_UNREACHABLE`
— reporting them as `recovering` would send a zero-touch caller into an
infinite retry. An incompatible incumbent, an ambiguous generation, a schema
mismatch and any unrecognised code fail closed to `blocked` for the same
reason; `doctor` never promises a repair that does not exist.

`doctor` evaluates protocol-build freshness itself, as the `protocol-build`
check, and reports `AGENT_FABRIC_PROTOCOL_BUILD_STALE` as `blocked`. A stale
build no longer prevents it from running: `scripts/agent-fabric doctor` runs the
shared preflight in doctor mode, which hands the verdict and the exact repair
command down instead of exiting 78, and the check reports both. Every other
subcommand and `scripts/agent-fabric-mcp` still exit 78 on a stale build. A dist
that is missing or cannot be imported blocks `doctor` too, because `doctor`
imports the protocol package transitively and cannot report through a dist it
cannot load; that import probe runs only for `doctor`, so no other entrypoint
pays a Node start-up per invocation. `wrapperIntercepted` is now empty: it
recorded that the launcher preempted this check, and it no longer does.

`doctor` does not start the daemon, but its certifying-profile check may run one
real Claude canary per six-hour cache window and update the private
`review-profile-pin-observations.json` cache. Obtain provider login, quota and
real-provider-smoke authority before running it.

In the idle case the `daemon-socket` check has status `idle`, not `pass` or
`fail`. This does not weaken preflight: for example, an intact idle daemon
state alongside an incompatible database still produces overall `state:
"blocked"` and a non-zero exit. A failed or in-progress bootstrap, ambiguous or
stale discovery, crashed process, orphan socket and failed handshake are never
relabelled idle. Concurrent doctors share one non-blocking inspection fence and
may report the same idle snapshot; bootstrap and shutdown ownership are
exclusive and report an in-progress code before socket artifacts are
classified. The daemon acquires its shutdown-transition fence before releasing
the election lock and retains it until terminal discovery is durable. Only
absent discovery or an exact, generation-matched `stopped` owner with exit `0`
and no signal can be idle. Forced, non-zero, unknown, crashed and otherwise
non-clean owners fail closed.

### Repair certifying-profile pin drift

When `doctor` reports `REVIEW_PROFILE_PIN_DRIFT`, it names `npm run profile:pin`
as the repair. This is an uncached live-provider probe and a repository edit,
not a diagnostic. Never run it inside certification: the review profile is
digest-bound, and changing it would move the profile a running certification
already verified. Exit `1` may coexist with independently observed changes when
another pin was unobservable. Inspect and review the resulting diff before
retrying.

`PROTOCOL_INCOMPATIBLE` means the incumbent answered the contract probe but did
not negotiate a result shape required by the current client. Do not attempt MCP
bootstrap or seat renewal against it, delete discovery files, compare source
commits or replace the database. Stop the incumbent through its owning Fabric
lifecycle, then rerun `provenant doctor` before retrying bootstrap or renewal.

Each global project-dynamic client registry contains the proxy command and
exactly three environment variables:

- `AGENT_FABRIC_STATE_DIRECTORY`
- `AGENT_FABRIC_SEAT`
- `AGENT_FABRIC_CLIENT_LABEL`

`AGENT_FABRIC_PROJECT_PATH` is not a fourth global variable. It is permitted
only in an explicit, separately managed project-scoped compatibility entry for
a client that cannot preserve workspace cwd; that entry must never be reused
globally.

| Client | Global registry |
| --- | --- |
| Agy | `~/.gemini/config/mcp_config.json` |
| Claude Code | `~/.claude.json` |
| Codex | `~/.codex/config.toml` |
| Cursor | `~/.cursor/mcp.json` |
| Kiro | `~/.kiro/settings/mcp.json` |
| OpenCode | `~/.config/opencode/opencode.jsonc` |

At proxy start, the client working directory is canonicalised and ancestors are
searched for the nearest project-keyed seat. An unprovisioned project fails
closed instead of authenticating into another project's run. Clients that do
not preserve the workspace working directory need project-scoped registration.
Subdirectories intentionally inherit the nearest ancestor project's seat.

The harness installer configures only its selected primary client by default.
Pass `--mcp-clients all` to register all six clients. Configure and verify all
six clients, including Claude Code and Codex, from the harness checkout with:

```sh
scripts/configure-agent-fabric-mcp.py --platform all
scripts/configure-agent-fabric-mcp.py --platform all --check
opencode mcp list
```

The `--check` command proves the expected registry entry for all six clients;
`opencode mcp list` additionally confirms that OpenCode can discover its
configured server. The configurer atomically replaces only the
`agent-fabric` entry in each client configuration and reports only that entry's
status. It never prints capabilities or unrelated configuration. Optional
clients use the supported `codex` seat while retaining distinct client labels.
OpenCode JSONC containing comments fails closed rather than being rewritten.
During apply, each successful changed-client write flushes its committed receipt
before the next client begins. A client initially classified as existing is
revalidated immediately before its receipt. Drift before any commit retains
exit code `3`. Once any live client path may have changed, including a
first-client atomic install followed by a durability or validation failure,
the command uses exit code `4`. The typed `partial-state` result names fully
completed clients as committed, keeps the affected current client and later
clients in remaining, and provides the reconcile-and-rerun recovery action.
Drift in an initially existing client after an earlier commit uses the same
result.
If stdout write or flush fails after a durable commit, the command
stops before the next client, attempts the same typed result on stderr and exits
`4`; the result names the committed client, remaining clients, configuration
path and recovery action. An output failure before any config mutation exits
`3` without a shutdown-time status override. Exit `4` remains the partial-state
signal even when stderr is unavailable too. Rerunning `--platform all` after
reconciliation is idempotent.
Existing files are updated with an atomic exchange: the displaced identity and
bytes must match the composed snapshot, and the requested direct path or
symlink must still resolve to the installed inode. On any mismatch the command
exits with a typed conflict and retains the displaced file at the reported
recovery path inside a fresh owner-only `0700` directory. The displaced object
is inspected without following symlinks and is not chmodded, so hard-linked
content retains its caller-owned mode. Conflict handling never rolls that object
back over a pathname that a concurrent writer may have changed. Reconcile both
the current client configuration and recovery object before rerunning.

Primary provider execution uses the locked Claude Agent SDK and the installed,
vendor-signed Claude Code and Codex CLIs through repository-owned wrappers. On
the supported host, restore the package closure and verify both boundaries
with:

```sh
npm ci --no-audit --no-fund
npm run compatibility:check:primary
```

The verifier checks the stable provider launchers, native signing identity,
non-answer interface, protocol schemas and Fabric wrapper provenance. Each
provider wrapper revalidates its launcher, safe path and vendor identity
immediately before a new provider process. Normal signed CLI updates need no
registry edit; wrapper or protocol changes remain fail closed.

The resolved `.cap` file must remain a private regular file with mode `0600`.
The adjacent `.json` file is secret-free metadata and is checked against the
canonical project, project key, seat and credential path before use. Never
paste capability values into a registry, log or document.

## Daemon supervision

Fabric is on-demand, not a login service. The first current client read or
command authenticates and attaches to a compatible incumbent before inspecting
the database. Only the elected no-incumbent path inspects current state,
rechecks it under the daemon-election lock and starts one owner. A compatible
busy WAL writer is therefore attachable; incompatible state remains untouched
and returns `SCHEMA_CUTOVER_REQUIRED`.

Use the following foreground command only for an authorised manual diagnostic
or supervised activation:

```sh
env AGENT_FABRIC_RUNTIME_DIRECTORY="$HOME/.local/state/agent-harness/fabric/runtime" \
  "$HOME/.agents/scripts/agent-fabric" daemon run \
  --trusted-config "$HOME/.agents/config/agent-fabric.yaml" \
  --compatibility "$HOME/.agents/config/adapter-compatibility.yaml" \
  --compatibility-schema "$HOME/.agents/runtime/agent-fabric/schemas/adapter-compatibility.schema.json" \
  --agents-home "$HOME/.agents"
```

Do not start this command merely because a pane or PID is absent. Re-run
`status` and `doctor`; on-demand bootstrap or the existing supervisor owns the
next step. A second daemon for the same socket or SQLite database fails closed.
The election, socket and database locks prevent two startup/recovery owners or
a shutdown/start race from serving one durable store.

## Shared-client model

Every client uses a separate stdio proxy process:

```text
AGENT_FABRIC_SOCKET_PATH=<same socket>
AGENT_FABRIC_STATE_DIRECTORY=<private fabric state directory>
AGENT_FABRIC_SEAT=<claude|codex>
AGENT_FABRIC_CLIENT_LABEL=<agy|claude|codex|cursor|kiro|opencode>
scripts/agent-fabric-mcp
```

The seat selects one of Fabric's two primary MCP identities. The client label
identifies the connecting surface; optional clients use the `codex` seat and
retain their own label.

Reviewed operator launch custody creates the project session, run and one
generation-fenced chair. Agents cannot create runs through MCP. Peers receive
narrowed authority and their own capability. Swapping Claude and Codex
leadership requires typed handoff/takeover custody; it does not change the
protocol or create a fallback chain.

### First use in an exact trusted project

An unprovisioned Claude or Codex global MCP exposes exactly one tool:
`fabric_bootstrap`. Call it without arguments. The proxy derives its validated
seat from `AGENT_FABRIC_SEAT` and the exact project root from its working
directory. The daemon atomically creates one deterministic, narrow scoping run;
the same MCP connection then emits `tools/list_changed` and exposes the normal
Fabric tools. A concurrent second primary joins that run as its peer and rotates
the normal two-seat generation.

Bootstrap never launches a provider and accepts no model, policy, root, run or
agent identifiers. Its fixed authority reads only the exact project root,
writes only its bootstrap run directory and Fabric coordination/evidence, and
denies secrets, deployment, irreversible actions and tool egress. An untrusted
root fails closed. The local fallback is `provenant fabric bootstrap --seat
claude|codex`; it invokes the same composition. Public `mcp provision` retains
its full-roster requirement.

Bootstrap returns one machine-readable action receipt covering every automatic
action it took, on both the CLI and the `fabric_bootstrap` tool result:

| `action` | `outcome` | `mutated` |
| --- | --- | --- |
| `workspace-trust` | `resolved` | always `false`; resolution only reads the trust record |
| `daemon` | `started` or `attached` | `true` only when this call spawned the daemon |
| `seat-generation` | `installed` or `replayed` | `true` only when the active generation changed |
| `identity-smoke` | `passed` or `failed` | always `false`; `whoami` and `mailbox.read` only read |

The receipt's top-level `mutated` is the idempotency surface, and it means one
specific thing: no logical custody state changed — the healthy seat was not
rotated, the compatible daemon was not restarted, and the active generation and
its rows are the ones that were already there. It is not a claim that no bytes
were written: a replay still stages the roster through a private temporary tree
and re-verifies the installed files before discarding it, which touches
directory metadata. Reason about disk effects by observing the filesystem, not
from this field. The receipt carries no bearer capability.

The identity/mailbox smoke is bounded by a whole-smoke wall-clock deadline
covering connect, `whoami` and `mailbox.read`. Settling is bounded under a
responsive event loop, by a timer that aborts the connect or the pending RPCs.
The reported outcome is bounded unconditionally: elapsed time is measured and
an overrun is failed even when the work resolved, because a synchronous frame
parse can outlive the timer it then clears. On either bound the receipt reports
`healthy: false` with a failed smoke and the installed seat still stands, since
an unbounded health check is a hang rather than a diagnosis. The CLI still exits
`1`; callers must inspect `receipt.actions` and `receipt.healthy` before deciding
whether to retry.

If the machine database is not current-schema, bootstrap raises
`SCHEMA_CUTOVER_GATE_REQUIRED` instead of starting. The gate carries the schema
mismatch, a per-table census of the coordination rows that exist today, the
consequences of approval, and the exact `database archive-and-fresh` command
including its `--confirm-source-set` digest. Bootstrap and its retry paths never
invoke the cutover: the SQLite source set is byte-identical after the gate.
Report counts and consequences, obtain exactly one user decision, and only then
run the command in
[Database archive-and-fresh cutover](#database-archive-and-fresh-cutover). The
cutover entrypoint is separately invocable and confirms only a source-set
digest, so it does not itself verify that a user decision happened; treat
running it as the approval, and never script it unattended.

After bootstrap, call `fabric_whoami` before constructing any request. It
returns the authenticated seat, agent, run, authority, active seat generation
and chair-lease state without caller-supplied identifiers. Fresh bootstrap
authorities use `bootstrap-authority:<lowercase-sha256>:<seat>`. The equivalent
non-rotating local inspection is:

```sh
provenant fabric bootstrap --inspect --seat "$AGENT_FABRIC_SEAT"
```

Normal bootstrap output includes the same caller `authorityId`. Inspection is
read-only: it neither starts a daemon nor replaces the active credential
generation.

For a zero-context bootstrap peer roundtrip:

1. call `fabric_bootstrap` only when it is the sole advertised tool, then call
   `fabric_whoami` on each seat;
2. create the task with its eligible agents; omitted `participantAgentIds`
   defaults to the creator plus those eligible agents;
3. send a task-audience request with a stable `dedupeKey`; the sender is not
   given its own pending delivery;
4. the peer receives and acknowledges the request, publishes any result
   artifact, and sends a `response` preserving the request's conversation and
   `replyToMessageId` while naming the artifact path and digest; and
5. the creator receives and acknowledges that correlated response and verifies
   the referenced artifact digest.

Bootstrap authority does not grant `task.claim`. For bootstrap-scoped work,
the correlated response plus verified artifact digest is the completion
evidence; do not widen authority or infer completion from pane text.

Bootstrap seats are short-lived bearers over a bounded bootstrap authority
that deliberately outlives them. When a bootstrap seat is expired or within
one hour of expiry, the Claude/Codex MCP proxy automatically revalidates trust
for the exact current root and asks the daemon to rotate the complete roster.
The daemon compare-and-swaps only the current generation and revokes every
predecessor token; the project session, run and chair do not change. If exact
trust or the generation changed, renewal fails closed. Stop and restart a
stale proxy after another host completes the cutover. Operator-created runs
continue to use the explicit `mcp provision` flow below.
The bounded bootstrap authority currently lasts 365 days. Any descendant-safe
rollover change requires separately scoped work; this runbook claims no such
implementation.

In production Console, Launch is available only when the dedicated
`projectSessions.prepareLaunch` operation and explicit operator-action commit
surface are negotiated. The selected live Project row supplies the session
revision, generation and reviewed launch-packet reference; Launch accepts no
caller-authored CAS fields. Preview preparation uses a per-input-attempt command
so an expired, effect-free preview can be replaced. Console derives the commit
command ID from the operator, project, session, session generation and exact
launch packet path/digest; input events and Console client instances are
deliberately excluded. An exact reopen therefore polls the existing commit,
while a new generation or packet gets a new identity. Provider dispatch still requires a
separate explicit confirmation gesture. Sessions projected as `launching` or
`launch_ambiguous` rehydrate through status-only observation; Console never
redispatches or invokes generic action reconciliation for launch custody.

If a selected session is `recovery_required`, Console asks the daemon to derive one loss-bound takeover capability and
server-authored recovery intent. Do not copy loss IDs or construct recovery payloads by hand. Abandon remains a
destructive action: inspect the preview, then use the separate explicit Confirm gesture. On confirmed
abandon, the daemon may repair historical launch accounting only from the exact terminal-success provider action
(`provider_calls=1`, retained `concurrent_turns=0`); other unknown capacity remains unknown.

Cancel may terminalise a `draft` or `awaiting_launch` session without provider I/O only when it has no run, membership,
launch custody, reservation, gate or prior operator effect. A live Console attachment is transport, not lifecycle work,
and does not block this path. Once any such lifecycle evidence exists, use its owning recovery or cancellation flow;
zero cancelled tasks must be reported as rejected, never as a successful task cancellation.

For visible pairing, Herdr attaches panes or observer renderers while messages still travel through the durable fabric mailbox. For headless orchestration, no pane is required. Both profiles can coexist in one run.

Before direct fire-and-forget steering, verify the exact registry environment
and negotiated integration:

```sh
provenant fabric herdr steer --check
```

The preflight checks, in order,
`AGENT_FABRIC_STATE_DIRECTORY`, `AGENT_FABRIC_SEAT`,
`AGENT_FABRIC_CLIENT_LABEL`, the resolved capability and the
`herdr-control.v1` integration. It exits non-zero and names the first failed
check. Steering remains fire-and-forget; answer-bearing work stays in Fabric
request/reply.

Herdr provides pane visibility and process supervision. Fabric events are
rendered by the explicit least-privilege `fabric-events` observer described
below; MCP tool responses and the SQLite-backed fabric remain authoritative.

## Provider controls and context

Set controls directly on each admitted provider spawn or turn:

| Control | Operator rule |
| --- | --- |
| `model` + `modelFamily` | Use exact provider values. A retained role/model change uses rotate and a fresh context. |
| `effort` | Use an explicit value supported by that provider/model. |
| `compact` | Checkpoint first, then continue the same retained task with bounded context. |
| `rotate` / clear | Checkpoint first, then start fresh for a new task, independent review, stale/confused/unreconciled context, or role/model change. Fabric rotate is the clear equivalent; never clear silently. |

The active optional reviewer routes are exact and subscription-authenticated:

| Review route | Exact selection | Family / effort |
| --- | --- | --- |
| Agy | `Gemini 3.1 Pro (High)` | Google / high |
| Cursor | `cursor-grok-4.5-high` | xAI / high |
| OpenCode | `opencode/<catalogue-model>` | generic-open / advertised ACP effort |

Kiro is an active optional open-weight ACP worker. Select an explicit model
reported by the current subscription; Fabric admits the maintained family
prefixes rather than locking exact model names.

OpenCode is limited to its `opencode/*` account catalogue. Its wrapper applies
only effort values advertised by that model's ACP session configuration.

Do not set or persist provider API keys for these routes or Kiro. The wrappers forward
only the minimal process environment (`HOME`, `PATH` and `TMPDIR`) and use the
provider CLIs' existing subscription sessions. `scripts/model-route resolve`
must report the exact family, model and high effort through `--adapter-gate
fabric` before dispatch. A disabled entry, inactive adapter, unresolved pin,
artifact mismatch, unavailable model or wrong family is terminal routing
evidence; use another already-admitted review family or record the distinct-family
leg as unavailable. Never bypass the gate with a direct CLI and claim Fabric
evidence.

Real provider smokes are local-only, consume subscription quota and require
current provider-use authority. They traverse the verified Fabric adapter
request protocol, require exact spawn/turn/release sentinels and prove the
isolated workspace stayed unchanged:

```sh
cd runtime/agent-fabric
node smoke/provider-adapter-readonly.mjs \
  --adapter agy --model 'Gemini 3.1 Pro (High)' \
  --model-family google --effort high \
  --provider-executable "$(../../scripts/agent-fabric adapter executable --adapter agy)"
node smoke/provider-adapter-readonly.mjs \
  --adapter cursor-agent --model cursor-grok-4.5-high \
  --model-family xai --effort high \
  --provider-executable "$(../../scripts/agent-fabric adapter executable --adapter cursor-agent)"
kiro-cli chat --list-models --format json-pretty
node smoke/provider-adapter-readonly.mjs \
  --adapter kiro-acp --model qwen3-coder-next \
  --model-family open-weight --effort low \
  --provider-executable "$(../../scripts/agent-fabric adapter executable --adapter kiro-acp)"
node smoke/provider-adapter-readonly.mjs \
  --adapter opencode-acp --model opencode/deepseek-v4-flash-free \
  --model-family generic-open --effort high \
  --provider-executable "$(../../scripts/agent-fabric adapter executable --adapter opencode-acp)"
```

For Kiro, first replace the example model when the account's current list has
changed; the name is smoke input, not an admission lock. The activation proof
for issue #265 returned `status: pass`, `output: exact-sentinel`, `workspace:
unchanged` and `session: spawn-turn-release` through Kiro 2.13.0 with Amazon
Team ID `94KV3E626L`. Version and digest in that receipt are observations only.

The issue #253 OpenCode acceptance returned `status: pass`, `output:
exact-sentinel`, `workspace: unchanged`, `providerConfig: unchanged`,
`credentialInput: subscription-session`, `fabricCapability: not-provided`,
`effort: high` and
`session: spawn-turn-release` for the advertised
`opencode/deepseek-v4-flash-free` model. It observed OpenCode 1.17.18 at the
canonical owner-controlled Homebrew Cellar path; version and digest remain
observations, not admission bounds.

`adapter executable` prints only the validated executable path from the active
adapter's compatibility entry. It fails closed before the provider smoke if the
adapter is inactive, the stable executable is missing or its compatibility
contract no longer conforms.

Claude reviewers and one-task workers start fresh and release when done. For a
retained Claude pair, checkpoint and compact at each stage or work-unit
boundary, by four answer-bearing provider turns, or before a pause expected to
exceed five minutes. Codex follows stage boundaries; native auto-compaction is
only a fallback. Fabric does not enforce these turn/time thresholds.

## Project Fabric Console

Build and verify the standalone Console before attaching it to live state:

```sh
npm run check --workspace=@local/agent-fabric-console
node runtime/agent-fabric-console/dist/bin.js --help
node runtime/agent-fabric-console/dist/bin.js --project "$PWD"
```

Use `--session '<stable project-session ID>'` when more than one attachable
session exists, `--herdr` when launched through the typed Herdr surface, or
`--export json|markdown` for a non-interactive snapshot. The interactive
Console follows the current terminal dimensions. `80x24` is the reference and
default when dimensions are unavailable, not a fixed size. Resize events
reflow full, compact and inert layouts while preserving stable selection,
focus, scroll, drafts and pending commands. `q` detaches the UI; it does not
stop a project session or daemon.

Opening a run selects its bound project session when necessary and displays
exact run-scoped identity, freshness, progress, work, agents, evidence,
activity and issues.

### Onboard accepted work

Use this path when a reviewed project artifact is accepted and a new draft
project session is ready to launch:

1. Resolve the owning Git root (`git rev-parse --show-toplevel`), or the
   canonical current project directory when no repository exists. Inspect that
   exact root and, when absent, establish trust with
   `$HOME/.agents/scripts/agent-fabric workspace trust "$project_root"` before
   opening the Console. This first-use step is automatic under the global
   harness; never substitute a parent, wildcard, home directory or sibling
   collection.
2. Create or select the draft project session. If several sessions are
   attachable, pass its stable ID with `--session`.
3. Open the complete, verified accepted-evidence row and choose
   `Implement...`. Supply exactly `intake`, `launch-packet-path`, `packet` and
   `resource-plan`. The packet contains the authority expiry, budget, provider
   route, run directory and write/worktree scopes. It references artifact
   paths and digests instead of embedding accepted source bytes.
4. Review the accepted-evidence, launch-packet and resource-plan refs;
   authority; budget; provider route; and worktree/write scopes. Editing either
   JSON document creates a new digest and review. Confirming Implement closes
   the two artifacts and moves the same project session to `awaiting_launch`;
   it does not contact the provider. Provider input is shown through the shared
   inert redactor and any trusted control, credential-like key or credential
   value is rejected before a preparation row or artifact is written. Cancel or
   fix any missing, stale, expired or inconsistent binding.
5. Select the live Project row and choose `Launch...`. Review the daemon-owned
   launch preview, then use a separate confirmation gesture. Only this step may
   dispatch the provider. After reconnect or handoff, reopen the session by its
   stable ID; Console uses its persisted exact packet ref.
6. Wait for committed launch status with a terminal-success journal and a
   current `seatProvisioning` descriptor. Provision the complete roster with
   the command under [Renew seats](#renew-seats), reconnect all clients, then
   run both registered MCP smoke checks.

Stop before Launch if Implement reports a changed evidence digest, session
revision, authority expiry, budget, provider route or artifact ref. Do not
repair those bindings by editing a generated digest or bypassing the Console.
Implement uses one stable command identity derived from the exact session,
accepted-scope, packet and plan bindings. A lost response is replayed with that
identity; after Console restart, reopen the same accepted evidence and submit
the exact packet and plan to recover the committed result. A changed binding is
a new command and never adopts the earlier result.

## Verify registrations

Client registry commands should report `agent-fabric` connected or ready. New
sessions may be required after changing a registry.

```sh
claude mcp list
codex mcp list
cursor-agent mcp list
kiro-cli mcp list
agy mcp list
opencode mcp list
```

The current Agy CLI uses a Bubble Tea TUI for `mcp list` and fails when no TTY
is available. In headless verification, inspect only the `agent-fabric` object
in `~/.gemini/config/mcp_config.json` and confirm its command.
Confirm exactly three global variables: `AGENT_FABRIC_STATE_DIRECTORY`,
`AGENT_FABRIC_SEAT` and
`AGENT_FABRIC_CLIENT_LABEL`. Confirm `AGENT_FABRIC_PROJECT_PATH` is absent.
That fourth variable is valid only in an explicit project-scoped compatibility
entry for a client that cannot preserve cwd. Never print capability files or
unrelated registry values.

Resolve the active credential and metadata paths for one project seat without
printing the capability:

```sh
scripts/agent-fabric mcp seat-path --project "$PWD" --seat codex
PROJECT_KEY="$(scripts/agent-fabric mcp seat-path \
  --project "$PWD" --seat codex | jq -r .projectKey)"
SEAT_GENERATION="$(scripts/agent-fabric mcp seat-path \
  --project "$PWD" --seat codex | jq -r .generation)"
```

Both values come from the current project-keyed seat pointer. Do not derive a
project key from status prose, a copied path or an older generation.

The daemon and every MCP proxy derive the same stable private socket at
`$AGENT_FABRIC_STATE_DIRECTORY/runtime/fabric-v1.sock`. Dynamic Claude Code and
Codex registry entries use the same exactly three environment variables as the
other global dynamic clients and bind no project path or credential. A client
that cannot preserve cwd may add `AGENT_FABRIC_PROJECT_PATH` only as the fourth
variable in an explicit project-scoped compatibility entry for one project; it
must never be reused as a global registration.

Start a least-privilege observer after provisioning or renewal:

```sh
scripts/agent-fabric mcp observer-provision --project "$PWD"
scripts/agent-fabric observe \
  --socket "$HOME/.local/state/agent-harness/fabric/runtime/fabric-v1.sock" \
  --capability-file "$HOME/.local/state/agent-harness/fabric/seats/$PROJECT_KEY/observer.cap" \
  --run-id '<current run id>' \
  --cursor "$HOME/.local/state/agent-harness/fabric/observer/$PROJECT_KEY.cursor.json"
```

When an authorised supervised foreground daemon is intentionally used, keep
its quiet process separate from the optional `fabric-events` observer. The
observer renders terminal-safe one-line events in Brisbane time (`AEST`,
UTC+10) and 160-character local message previews, never bearer credentials.
The cursor is saved after rendering. Orderly restarts resume at the next event;
a crash between rendering and cursor persistence can repeat the last event, so
consumers must treat the stream as at-least-once.

Run transport-only checks independently of provider execution:

```sh
cd runtime/agent-fabric
export AGENT_FABRIC_PROJECT_KEY="$(../../scripts/agent-fabric mcp seat-path \
  --project ../.. --seat codex | jq -r .projectKey)"
node smoke/registered-mcp-health.mjs ../..
node smoke/registered-mcp-roundtrip.mjs ../..
```

The health smoke checks all five seats, tool/resource discovery and readable
run state. The round-trip smoke sends and acknowledges Codex to Claude and
Claude to Codex mailbox messages through separate MCP proxies.

## Renew seats

Bind a new immutable seat generation to the exact current operator-launched
project session and coordination run before the current credentials expire.
After launch reaches committed status, use the current `seatProvisioning`
descriptor returned by `operatorActionStatus` for the session/run revisions,
generations, chair identity and active chair lease. This descriptor is a
current CAS projection and is not part of the immutable commit receipt; refresh
status immediately before provisioning. The command derives the current active
roster generation from the locked project pointer and passes it as the expected
predecessor; there is no caller-selected rollback value. The requested expiry
must be a future ISO timestamp no more than 31 days away and cannot outlive any
bound agent's authority:

```sh
scripts/agent-fabric mcp provision \
  --project "$HOME/.agents" \
  --project-session-id '<current project-session ID>' \
  --session-revision '<current session revision>' \
  --session-generation '<current session generation>' \
  --run-id '<current coordination-run ID>' \
  --run-revision '<current run revision>' \
  --chair-seat codex \
  --chair-agent-id '<current chair agent ID>' \
  --chair-generation '<current chair generation>' \
  --chair-lease-id '<active chair lease ID>' \
  --seat-bindings 'agy=<agent>@<generation>,claude=<agent>@<generation>,codex=<chair-agent>@<generation>,cursor=<agent>@<generation>,kiro=<agent>@<generation>' \
  --expires-at '<ISO timestamp>'
```

Provisioning creates only agent capabilities for the supplied existing
principals. It does not create or select a project, session, run, chair,
authority, agent or discussion group. Any stale, retired, rolled-back,
cross-project or crossed identity fails atomically. An exact replay is
idempotent. The JSON result includes `expectedPreviousGeneration` and the new
content-addressed `generation`.

The daemon compare-and-swaps the active generation and revokes every prior
roster token in one transaction. The CLI stages and fsyncs the complete
`generations/<generation>/` directory, then compare-and-swaps `current.json`
under the private project lock only if its predecessor still matches. A delayed
writer cannot replace a newer pointer, and readers never fall back to a flat or
old pointer shape. Stop old proxies before cutover, restart or reconnect all
clients together, and rerun both smoke checks. An already-connected old proxy
is rejected on its next authenticated operation; do not treat two generations
as one team.

## Recovery

- A retained MCP proxy reconnects after a daemon restart or a terminal protocol
  timeout with its current seat, refreshing that seat only when authentication
  rejects it. The negotiated five-minute idle limit bounds an unused transport,
  not the duration of a provider turn or review: the next Fabric call reconnects
  transparently when the old transport had already closed, because that call was
  never submitted. A request that times out locally while still queued beyond
  the negotiated in-flight limit is likewise proved unsubmitted and reconnects
  for one bounded replay under the same exact principal. Concurrent requests
  share that reconnect attempt. An
  in-flight request is replayed only with a durable identity: a stable
  `commandId`, or `dedupeKey` for `fabric_message_send`. An ambiguous
  commandless in-flight request reconnects but returns `RECONNECT_REQUIRED` with
  an operation-aware reconciliation action before an explicit retry. For
  `fabric_message_receive`, the outcome remains unknown, no delivery is
  acknowledged, and the caller must wait at least its requested
  `visibilityTimeoutMs` before retrying so any hidden claim becomes visible.
  A second timeout or disconnect after replay returns the same typed recovery
  class and the same operation-aware action instead of a raw
  `PROTOCOL_TIMEOUT`. The same typed error and one action report an unavailable
  daemon or seat; a healthy `doctor` does not repair an already terminal proxy
  transport.
- A second daemon for the same socket or canonical database is rejected by an
  OS-backed SQLite exclusive owner lock held for the daemon lifetime. Process
  death releases the kernel lock without pathname deletion or stale-takeover
  races. Symlinked, dangling-symlink and hard-linked database paths fail closed.
- Startup releases expired delivery claims, quarantines expired unfenced write leases, reconciles non-terminal provider actions and marks unrecoverable sessions `context-unreconciled`.
- Provider effects use stable action IDs. Ambiguous effects are looked up or quarantined; they are not silently replayed.
- Interactive pane/TUI loss suspends the principal and freezes delivery until explicit reattach/rotation.
- Agents request compact or rotate with a revision-bound checkpoint. The lead closes stage/run barriers only after tasks, evidence, messages, leases, provider actions, handoffs and gates reconcile.

## Database archive-and-fresh cutover

Use this deliberate operator command only after
`SCHEMA_CUTOVER_GATE_REQUIRED` whose `gate.mismatch.code` is
`SCHEMA_CUTOVER_REQUIRED`. It archives incompatible pre-release state and
creates a fresh current baseline. It does not import, migrate or automatically
restore archived state. First run the read-only preview without confirmation:

```sh
archive_directory="$HOME/.local/state/agent-harness/fabric/backups/cutover-$(date -u +%Y%m%dT%H%M%SZ)"
scripts/agent-fabric database archive-and-fresh \
  --archive "$archive_directory"
```

The JSON result is `confirmation-required` and reports the exact expected and
observed schema fields, every existing source member's identity, mode and
SHA-256, and a `confirmation.sourceSetSha256`. With no matching cutover
residue, an absent, empty or already current database instead returns a typed
`no-op`; neither result creates the archive directory or changes the database.

The process exit contract is:

| JSON `status` | Exit | Meaning |
| --- | ---: | --- |
| `confirmation-required`, `no-op`, `completed` | `0` | Preview, unchanged state or completed cutover; these are different outcomes. |
| `failed`, `conflict` | `1` | Validation, source-set or archive failure before the recovery boundary. |
| `recovery-required`, `archive-complete-cutover-failed`, `archive-complete-fresh-init-failed` | `4` | Recovery or post-publication handling is required. |

Automation must branch on the JSON `status`, never on exit `0` alone. In
particular, `confirmation-required` is a read-only preview, not a completed
cutover. Exit `4` requires recovery or post-publication handling before Fabric
starts.

Before confirmation, stop Fabric through its owning supervision surface and
ensure every direct SQLite writer is quiescent. The command does not inspect,
list or signal operating-system processes. It re-reads the complete source set
and fails closed on a race, but this last-moment check is not a lifetime writer
lock. Confirm the exact previewed bytes without a TTY:

```sh
scripts/agent-fabric database archive-and-fresh \
  --archive "$archive_directory" \
  --confirm-source-set 'sha256:<digest from preview>'
```

Confirmation is digest-bound. The token is a correctness interlock: it proves
the destructive invocation still targets exactly the bytes inspected by the
preview. It is not a user-authority gate because the preview itself reports the
token. Explicit user authority for the destructive step is enforced by the
permission gate outside this command; [issue #450](https://github.com/mblauberg/provenant/issues/450)
owns the deeper interlock follow-up. A changed source set, stale digest,
symlink, archive collision,
SHM without WAL or a rollback journal mixed with WAL/SHM returns a typed
conflict or failure without displacing the source. A readable database plus WAL
without SHM is valid because SQLite rebuilds the missing wal-index. The archive
destination must be an absolute, wholly absent directory. The command stages
and fsyncs the complete payload, rejects any destination beneath a canonical
source-member path after resolving existing destination ancestors, claims the
final archive directory without overwrite, and atomically publishes
`source-set/`. Only after that durable recovery boundary does it atomically
claim the exact confirmed source files into a private same-directory holding
area and verify their identities before removal. The archive contains the main database
and every existing canonical sidecar (`-wal`, `-shm`, `-journal`) plus
`source-set/receipt.json`. Modes are preserved and the receipt records the
source identity and SHA-256 of each member without database rows, environment,
capabilities or credentials.

`-shm` is included deliberately. A working private inspection clone omits it so
SQLite can rebuild process-shared lock/index state, but an archive is immutable
custody of the exact observed source set. Reconstructibility is not permission
to omit observed evidence.

The rollback boundary is archive publication. All archive-preparation failures
leave the original source paths in place. After a complete archive is durable,
the verified private source claim is removed and the existing current-baseline
initializer runs. Any failure after publication exits
`4` with an explicit `archive-complete` recovery result; it never claims that
the source was preserved. If source-claim cleanup fails the fresh baseline
remains pending. If the baseline is current but completion-receipt finalisation
fails, do not repeat the cutover: preserve the archive and reconcile
`receipt.json` before starting Fabric.
If a writer wins a race after archive publication but before the source claim,
preserve both the archive and every live/private-claim path. Do not blindly
restore over the live paths: compare their identities and hashes with the
receipt and manually reconcile any newer or unarchived state. Source-claim
failure results name the private `claimDirectory` and whether it was preserved,
removed after exact rollback or removed empty. Where a later invocation would
otherwise report a `no-op` — an absent, empty or already current database — it
first reports any matching private claim directory or archive `.staging-*`
directory as `CUTOVER_RESIDUE_DETECTED` with exit `4`, so an interrupted
cutover cannot read as a clean exit `0`. Preserve and reconcile this residue;
remove an empty directory only after verifying that it contains no source data.
A live source set is governed by the claim race instead, so a cutover already in
flight does not make an ordinary preview report residue.
If that initializer fails, the command exits `4` with
`archive-complete-fresh-init-failed`, the receipt path and one recovery action.
Do not start Fabric. Preserve the archive, move aside any incomplete fresh
source set, restore **every** member named by `receipt.json` to the original
database basename with its recorded mode, verify every SHA-256, then retry; do
not restore only the main file. The command itself is not a restore utility.

## Retention and archive

Retention is report-only. It never deletes data:

```sh
scripts/agent-fabric retention status
scripts/agent-fabric retention preview
scripts/agent-fabric retention archive \
  --run-id '<terminal run id>' \
  --output "$HOME/.local/state/agent-harness/fabric/archives"
```

Archive requires a terminal, non-quarantined run with a verified exported
receipt. It copies that immutable coordination receipt and a hash-bound
manifest without modifying the source database or run directory. There is no
retention `apply` command.

## Receipt and shutdown

Export `fabric-receipt.json`, declare it in the canonical `delivery-run`
`RUN.json` as the `fabric-coordination-receipt` evidence artifact, and verify
the artifact digest before stopping the daemon. The fabric receipt hashes
provider resume references and records full coordination fields; it does not
expose provider secrets. Do not create or adopt a second run-receipt shape.

Do not delete the SQLite database, capability key, provider-native session, or `.agent-run` evidence as part of normal completion. Retention or destructive cleanup requires its own user decision.
