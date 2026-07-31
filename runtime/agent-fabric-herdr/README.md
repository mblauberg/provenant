# Agent Fabric Herdr adapter

This package is the optional, non-authoritative boundary between Agent Fabric
and Herdr. It exposes typed pane, presence, focus, wake, metadata, attention,
notification and direct-steer operations. It does not own coordination state,
authority, persistence or provider sessions.

## Production composition

`createProductionHerdrIntegration` is the shipped composition boundary. It
resolves the configured absolute Herdr path to an executable regular file and
checks its current structured snapshot protocol and capability shape. Stable
provider symlinks may move when Herdr updates. It separately verifies SHA-256
digests for the repository-owned Console and optional observer executables,
then composes:

- a fixed-argument, no-shell Herdr process port with bounded output and
  deadlines;
- structured snapshot presence that never reads pane scrollback;
- typed pane, metadata, attention, focus, wake, notification and injection
  controls;
- a private `0700`/`0600` adapter-evidence journal for lookup after an
  ambiguous effect; and
- the canonical Fabric journal and direct-steer reference validator supplied by
  the daemon integration seam.

The last two Fabric ports are deliberately required. This package does not
open Fabric SQLite, mint actions or create a second authority store. Fabric
must prepare the stable action and validate a task/message reference before
`HerdrAdapter` can dispatch a production effect. The adapter-local evidence
journal is lookup evidence only; it cannot approve, acknowledge, complete or
authorise anything.

The typed `agent.wake` operation is deliberately a focus-only Herdr signal. It
does not write terminal text or claim delivery; durable Fabric mailbox or
request/result state remains the only work transport.

```ts
const integration = await createProductionHerdrIntegration({
  executable: "/opt/homebrew/bin/herdr",
  expectedProtocol: 16,
  stateDirectory: "/absolute/private/fabric/herdr",
  projectId,
  projectSessionId,
  canonicalProjectRoot,
  consoleExecutable: "/absolute/pinned/agent-fabric-console",
  consoleExecutableDigest: "sha256:<pinned-file-digest>",
  observerExecutable: "/absolute/pinned/agent-fabric",
  observerExecutableDigest: "sha256:<pinned-file-digest>",
  observerSocketPath: "/absolute/private/fabric.sock",
  observerCapabilityFile: "/absolute/private/observer.cap",
  observerCursorDirectory: "/absolute/private/observer-cursors",
  fabricJournal,
  fabricDirectSteer,
});
```

The production Fabric daemon owns this composition in normal use. Pass the
same configuration through `startFabricDaemon({ herdr: { enabled: true, ... } })`; the
child daemon validates a closed configuration, creates a private per-session
evidence directory, prepares each `herdr-control-v1` action before loading or
probing Herdr, and polls structured presence without overlapping passes.
Herdr's observed version is diagnostic evidence only. Neither it nor the
provider executable's bytes decide admission.
Restart performs lookup only for already-dispatched/ambiguous actions. It also
rehydrates terminal Console/agent pane observation bindings from durable Fabric
actions without replaying pane creation. Disabled or missing packages remain a
typed optional-integration failure and do not disable non-Herdr Fabric paths.

Provider-TUI lookup binds an exact provider-session reference, but Herdr does
not expose the Fabric project/run identity or provider-session generation.
Presence therefore remains `identity-unverified` with `identity: null`; pane
existence never makes an agent ready. Observer panes are explicitly
`observer-presence-only`. Enabling them requires all five observer settings:
the repository-owned executable and digest, the canonical Fabric socket, a
bounded private capability file and a canonical private cursor directory. The
launched command is the closed `agent-fabric observe --socket ...
--capability-file ... --run-id ... --cursor ... --interval-ms 1000` surface;
capability content is never read, copied or projected by this adapter.

## Library use

Provide three ports to `HerdrAdapter`:

- `FabricActionJournalPort`, backed by Fabric's durable stable-action records;
- `HerdrControlPort`, backed by the admitted Herdr CLI or socket integration;
- `HerdrPresencePort`, backed only by structured presence metadata.

Fabric must prepare an action and its intent digest before calling
`HerdrAdapter.execute`. The adapter records `dispatched` before the external
call. If the call or its receipt is uncertain, it performs lookup and leaves the
action `ambiguous`; it never blindly repeats the effect.

Provider readiness requires exact reconciliation of project, session, run,
agent, provider, model, provider-session reference and session generation.
Pane existence alone returns `identity-unverified`. Missing Herdr visibility
returns provider state `unknown`, not lost or stopped.

## Direct steering

Use `DirectSteerService` only for one-way steering where no answer is needed.
It requires:

- explicit `fireAndForget: true`;
- a caller-supplied Fabric task or message reference;
- authoritative validation of that exact reference and target;
- no acknowledgement, expected result or dependent completion barrier; and
- a terminal-neutral prompt of at most 4096 UTF-8 bytes.

Fabric prepares the stable action before injection. A successful pane write is
reported only as `dispatched-unconfirmed`; it is never a delivery
acknowledgement and cannot complete expected work or close a barrier.

The installed `agent-fabric-herdr` executable provides only boundary
diagnostics:

```sh
agent-fabric-herdr doctor --config /absolute/trusted/herdr.json
```

`doctor` validates the current local boundary and reports the observed Herdr
version and negotiated protocol. Steering is available only
through the authenticated public Fabric operation
`fabric.v1.herdr-steer.dispatch`, which uses `DirectSteerService` to validate
the exact reference and stable action before pane I/O. The CLI accepts no
arbitrary command, argument vector, environment or shell surface.

## Degraded artifact collection

When structured provider callback is unavailable, call
`createDegradedArtifactCollectionPlan` with the exact artifact path and digest,
request task/message, collector, stable collection action, deadline and finite
attempt bound. Run `collectDegradedArtifact` through a Fabric-authorised
artifact resolver. The function stops at the declared bound or deadline and
reports missing or conflicting evidence explicitly.

Even a matching digest is `collected-unverified`. A separate authorised Fabric
workflow must review and commit any result or completion transition; this
fallback supplies neither delivery evidence nor barrier satisfaction.

## Verify

Run from the repository root.

```sh
scripts/install-agent-fabric-dependencies
npm run check --workspace=@local/agent-fabric-herdr
```

Tests use fake Fabric ports and local sealed process fixtures. They do not start
Herdr, contact a provider or network, inject into a real pane, persist a
credential or perform a hosted effect.
