# Agent fabric runtime

Local coordination runtime shared by Claude Code, Codex and optional provider adapters. It keeps authority, tasks, leases, mailboxes, provider-action reconciliation, teams, receipts and lifecycle checkpoints in one SQLite/WAL store behind a private Unix socket.

Status: the current pre-release runtime, protocol, Console and activation
extensions are implemented. Final integrated verification, independent review
and human acceptance remain pending. Read live daemon, adapter, registration
and seat state from the machine interface; this README does not cache it.

## Architecture

```text
Console/operator client ─┐
agent MCP proxies       ─┼─ private Unix socket ─ daemon ─ SQLite/WAL
provider-session bridge ─┘                            │
                                                   ├─ provider adapters
Herdr visibility/control adapter ──────────────────┘
```

The daemon is authoritative. Herdr provides visible panes and wake-ups, not coordination state. Pi is an optional worker adapter, not the harness or authority store.

Important boundaries:

- `src/core/fabric.ts`: aggregate coordination façade and transaction owner
- `src/core/client.ts`: capability-bound client façade
- `src/application/provider-action-dispatch-request.ts`: generic provider-dispatch request contract and fail-closed canonicalisation
- `src/application/command-journal.ts`: command dedupe and transaction owner
- `src/core/migrations.ts`: current-baseline custody and cutover inspection
- `src/persistence/sqlite.ts`: hardened current-database connection
- `src/transport/bounded-ndjson.ts`: shared byte-bounded framing
- `src/cli/workspace-trust.ts`: exact machine-local workspace admission
- `src/cli/retention.ts`: report-only retention and non-destructive archive
- `src/core/read-policy.ts`: chair/owner/participant scoped projections
- `src/daemon/`: single-instance process, trusted composition and socket transport
- `src/mcp/`: one input/output schema surface for both primary clients
- `src/adapters/providers/`: isolated provider adapters with contract-based CLI admission
- `src/exports/`: receipt projection, schema enforcement and link verification
- `migrations/0001-current-baseline.sql`: complete current schema, triggers and indexes
- `schemas/database-baseline.v1.json`: pinned SQL and canonical catalogue digests
- `../agent-fabric-protocol/`: sole public operation, schema and MCP descriptor owner
- `../agent-fabric-console/`: standalone responsive operator TUI
- `../agent-fabric-herdr/`: typed Herdr control and degraded-steer boundary
- `../agent-fabric-review-portal-supervisor/`: native review-portal process and filesystem-custody boundary

## Development

Run from the repository root. The root lockfile and project-reference build are
the only supported install and build path.

```sh
scripts/install-agent-fabric-dependencies
npm run build
npm run check
npm run test:evaluation
npm run test:load
```

`scripts/install-agent-fabric-dependencies` runs the root locked install and
records `runtime/agent-fabric/.npm-ci-attestation`. Launchers and adapter
admission fail closed when the lockfile, package integrity values or installed
execution tree no longer match that receipt; rerun the helper to restore the
attested install.

Normal tests use temporary databases and fake provider boundaries. They do not
log into providers, register MCP servers or prove the Console's human
timed-identification acceptance gate. The Console evaluation reports automated
interaction evidence separately from that human result.

### Optional GitHub hosted checks

Local typed Git reads are independent of GitHub. `GitRepositoryReadService`
accepts an optional `GitHostedChecksPort`; omitting it is the default and
projects `unavailable` hosted facts while keeping local Git `live`.

Trusted daemon composition can call
`createOptionalGitHubHostedChecksAdapter({ enabled: true, ... })` with one
canonical repository root, exact `owner/repository`, `github.com`, and a
canonical SHA-256-pinned `gh` executable. The adapter issues only the fixed
bounded check-runs request for the observed native HEAD. It accepts no URL,
ref, arbitrary GitHub endpoint, argument vector, shell or caller environment.
An outage, authentication failure, malformed response, more than 100 checks or
oversized output becomes exact-HEAD `stale` (when a same-binding cache exists)
or `unavailable`; it never fails or rewrites the local Git projection.
Credentials may be inherited ephemerally by `gh`, but are never accepted in
configuration, persisted, logged or projected. Retargeting the canonical root,
repository or HEAD fails before process I/O.

Inspect the live machine without printing capabilities:

```sh
scripts/agent-fabric status --json --project "$PWD"
scripts/agent-fabric doctor --json
scripts/agent-fabric retention preview
```

Fabric source, Fabric tests, and the compiled Fabric CLI all resolve the
protocol package's `import` export to dist. The launcher and source daemon API
verify that local protocol dist is current before daemon election. A missing or
stale build reports `AGENT_FABRIC_PROTOCOL_BUILD_STALE` and the exact repair:

```sh
scripts/agent-fabric-protocol-build
```

## Runtime locations

Resolved by `src/cli/paths.ts` under the user data/runtime directories:

- durable database and capability key: state directory
- Unix socket and socket-identity owner lock: runtime directory
- database-identity owner lock: beside the durable database
- per-project evidence: `.agent-run/<run-id>/`

Each configured client launches `scripts/agent-fabric-mcp` with the same socket
and a seat label. The proxy canonicalises its working directory, walks ancestor
projects for the nearest matching provisioned seat and fails closed when none
exists. Client labels never change the MCP schema or authority. The raw
capability is stored only in its private `0600` `.cap` file; registry
configuration contains neither the credential nor a fixed project seat path.
Seat rotation binds one content-addressed generation in the daemon database,
atomically revokes the prior roster, stages the complete immutable filesystem
generation and compare-and-swaps a private `current.json` pointer. There is no
flat-seat fallback or second accepted generation.

Installers and operators configure project-dynamic Claude Code and Codex plus
Cursor, Agy, Kiro and OpenCode entries through
`scripts/configure-agent-fabric-mcp.py`; `--platform all` configures all six
clients, and `--check` verifies only the `agent-fabric` entries for all six
clients. The command therefore covers all six clients. Every global dynamic
entry contains the proxy command. It contains
exactly three environment variables:
`AGENT_FABRIC_STATE_DIRECTORY`, `AGENT_FABRIC_SEAT` and
`AGENT_FABRIC_CLIENT_LABEL`. `AGENT_FABRIC_PROJECT_PATH` is permitted only as
the fourth variable in an explicit, separately managed project-scoped
compatibility entry for a client that cannot preserve workspace cwd; it is
never part of a global entry. Existing-file updates use an atomic exchange with
displaced-byte and installed-path verification. Concurrent configuration drift
produces a typed conflict and retains the displaced object without symlink
following or chmod inside a fresh owner-only `0700` recovery directory;
conflict handling never rolls back over the live client pathname. Multi-client
apply flushes each committed receipt immediately and revalidates an initially
existing client immediately before its receipt. Existing-client drift before
any commit exits `3`. Once a live client path may have changed, including a
first-client install followed by a durability or validation failure, apply
exits `4` with a typed `partial-state` result. It identifies fully completed
clients as committed, leaves the affected current and later clients in
remaining, and gives the reconcile-and-rerun action. If stdout write or flush
fails after a durable commit, apply stops before the next client, attempts a
typed stderr result naming the committed client, remaining clients and
configuration path, and exits `4` even if stderr is unavailable too. Output
failure before any config mutation exits `3` without a shutdown-time status
override.

Clients use lock-safe on-demand bootstrap: they attach to a compatible
incumbent before database preflight, or elect one daemon and inspect/publish
current state on the no-incumbent path. Fabric is not a login service. Herdr
may host or observe processes, but it does not own daemon truth. A newly
started client session loads its MCP registry entry; an older provider session
may need to reconnect or restart after a registry or seat-generation change.

Doctor inspection uses a shared flock across election and discovery
classification. Concurrent doctors coexist, while an exclusive bootstrap or
shutdown-transition fence reports in progress rather than idle or
stale-socket. Healthy stopped-idle requires exit `0` with no signal.

See [the operations runbook](../../docs/runbooks/agent-fabric-operations.md) before any live start or registration.
