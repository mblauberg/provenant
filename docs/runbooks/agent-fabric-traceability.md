# Agent fabric traceability

Status: historical staged-delivery traceability map. Every test path listed
below exists at HEAD, but later requirements are not comprehensively indexed;
this is not a current release, hardening or Console conformance receipt.
Protocol source: [Agent Fabric specifications](../specs/README.md#agent-fabric)
Activation context: [Activation and operations](../specs/agent-fabric/activation.md)
Daemon-hardening context: [Architecture assurance](../specs/agent-fabric/architecture-assurance.md)
Delivery context: [Console specifications](../specs/README.md#project-fabric-console)
Test roots: `runtime/agent-fabric/`, `runtime/agent-fabric-protocol/` and
`runtime/agent-fabric-console/`

This runbook preserves the original Agent Fabric staged-delivery requirements
and their implemented Vitest paths. It does not cover every requirement added
after that staged map and proves neither current integrated verification nor
programme acceptance. Use the live specifications and test tree for current
coverage. A listed test path remains mandatory from its introduction stage
onward; absence, skip, todo or quarantine of a deterministic test fails that
stage. Several requirements may share one public-behaviour test; the test name
must identify each mapped ID.

## Capability-authority boundary recorded by this map

D-023 accepts the design in the
[authority](../specs/agent-fabric/authority.md),
[provider-action](../specs/agent-fabric/provider-actions-and-adapters.md),
[activation](../specs/agent-fabric/activation.md) and
[provider/lifecycle custody](../specs/agent-fabric/provider-custody.md)
owners. It did not accept the remaining Console draft by implication. At that
freeze, the direct `AuthorityEnvelopeV2` cutover and narrow, stateless,
write-free compiler were implemented while later persistence and containment
work was incomplete.

The live runtime now implements `workspace-write-offline`; its current contract
and evidence live in the linked specifications and test tree. FR-089–FR-095,
NFR-040–NFR-042 and AC-066–AC-070, plus the Console
[operator projections](../specs/console/operator-interaction.md), postdate this
matrix. Their absence here is why this historical map must not be used as a
current coverage claim.

## Functional requirements

| ID | Stage | Recorded Vitest path | Required evidence |
|---|---:|---|---|
| FR-001 | 2 | `tests/acceptance/stage2/mcp-facade-symmetry.acceptance.test.ts` | Claude and Codex client fixtures expose identical tool and resource semantics. |
| FR-002 | 2 | `tests/integration/mcp-two-proxies.integration.test.ts` | Independent proxies observe one daemon, revision and store. |
| FR-003 | 1 | `tests/acceptance/stage1/mailbox-delivery.acceptance.test.ts` | Commit-before-delivery, receive, acknowledgement, retry and cursor replay. |
| FR-004 | 1 | `tests/acceptance/stage1/task-claim-cas.acceptance.test.ts` | One chair and one active owner; competing claims fail atomically. |
| FR-005 | 1 | `tests/acceptance/stage1/authority-budget-narrowing.acceptance.test.ts` | Wider path, action, disclosure, expiry, unit or amount is rejected. |
| FR-006 | 1 | `tests/acceptance/stage1/fenced-lease-recovery.acceptance.test.ts` | Equal, parent and child prefixes conflict; disjoint prefixes may coexist. |
| FR-007 | 3 | `tests/acceptance/stage3/execution-profiles.acceptance.test.ts` | Headless, observed, interactive and hybrid profiles resolve only supported capabilities. |
| FR-008 | 3 | `tests/acceptance/stage3/herdr-paired-visibility.acceptance.test.ts` | Side-by-side and degraded-unpaned-chair outcomes use a fake Herdr process. |
| FR-009 | 3 | `tests/acceptance/stage3/visibility-degradation.acceptance.test.ts` | Herdr loss preserves tasks, messages, leases and resume references. |
| FR-010 | 1 | `tests/acceptance/stage1/mailbox-delivery.acceptance.test.ts` | Shared-task, direct-dependency and discussion-group participants can address each other. |
| FR-011 | 1 | `tests/acceptance/stage1/mailbox-delivery.acceptance.test.ts` | Message content and delivery never mutate ownership or authority. |
| FR-012 | 3 | `tests/unit/primary-provider-adapters.unit.test.ts` | Injected Claude SDK and Codex app-server boundaries exercise fabric-owned wrappers without provider invocation. |
| FR-013 | 3 | `tests/acceptance/stage3/lifecycle-checkpoint.acceptance.test.ts` | Compact, rotate and release require a complete revision-bound checkpoint. |
| FR-014 | 3 | `tests/acceptance/stage3/safe-completion.acceptance.test.ts` | Release and completion never delete provider-native session files. |
| FR-015 | 3 | `tests/acceptance/stage3/model-routing-receipt.acceptance.test.ts` | Existing `scripts/model-route` output is invoked, validated and retained unchanged. |
| FR-016 | 4 | `tests/acceptance/stage4/optional-adapter-degradation.acceptance.test.ts` | Deadline expiry degrades only the optional leg and records its reason. |
| FR-017 | 1 | `tests/integration/daemon-restart-replay.integration.test.ts` | Unclean restart restores committed coordination state. |
| FR-018 | 1 | `tests/integration/daemon-receipt-link.integration.test.ts` | Schema-valid receipt is exported and declared as the `fabric-coordination-receipt` evidence artifact in a canonical `delivery-run` fixture. |
| FR-019 | 5 | `tests/acceptance/stage5/team-hierarchy.acceptance.test.ts` | Leaders and registered workers obey depth, authority and budget limits. |

## Quality requirements

| ID | Stage | Recorded Vitest path | Required evidence |
|---|---:|---|---|
| NFR-001 | 1 | `tests/integration/daemon-unix-socket.integration.test.ts` | Default startup creates only a local Unix socket and no network listener. |
| NFR-002 | 1 | `tests/integration/daemon-unix-socket.integration.test.ts` | Socket, state and discovery paths reject group/world access. |
| NFR-003 | 1 | `tests/integration/daemon-restart-replay.integration.test.ts` | Crash after commit and before delivery replays without loss. |
| NFR-004 | 3 | `tests/acceptance/stage3/crash-after-provider-acceptance.acceptance.test.ts` | A killed adapter after acceptance reconciles the same stable ID without replaying its effect. |
| NFR-005 | 1 | `tests/performance/daemon-local-coordination.performance.test.ts` | At least 32 simulated agents and 1,000 post-warm-up operations meet 100 ms p95 with host metadata recorded. |
| NFR-006 | 1 | `tests/acceptance/stage1/fake-adapter-conformance.acceptance.test.ts` | A second fixture adapter passes without changes to mailbox, task or lease core. |
| NFR-007 | 2 | `tests/acceptance/stage2/mcp-facade-symmetry.acceptance.test.ts` | Both clients consume the same schemas with no harness fork. |
| NFR-008 | 3 | `tests/acceptance/stage3/operator-intervention-receipt.acceptance.test.ts` | Mediated and reported intervention plus provenance classification appear in the fabric receipt. |
| NFR-009 | 3 | `tests/acceptance/stage3/config-profile-selection.acceptance.test.ts` | An untrusted project layer cannot select an execution profile. |
| NFR-010 | 1 | `tests/acceptance/stage1/restart-state-recovery.acceptance.test.ts` | Restart restores contiguous watermark, acknowledgements above it, task revision and lease generation. |

## Acceptance scenarios

| ID | Stage | Recorded Vitest path | Deterministic oracle |
|---|---:|---|---|
| AC-001 | 3 | `tests/acceptance/stage3/paired-messaging-role-reversal.acceptance.test.ts` | Fake Claude-chair/Codex-peer and reversed roles exchange and acknowledge symmetrically. |
| AC-002 | 3 | `tests/acceptance/stage3/herdr-paired-visibility.acceptance.test.ts` | Fake Herdr verifies pane placement, renderer-only closure and display-cursor resumption. |
| AC-003 | 3 | `tests/acceptance/stage3/interactive-inbox.acceptance.test.ts` | Busy fake TUI queues without acknowledgement; wake-up alone is insufficient. |
| AC-003A | 3 | `tests/acceptance/stage3/interactive-paired-roundtrip.acceptance.test.ts` | Cooperative pull consumes the exact ID, persists the reply and leaves missed deadlines pending. |
| AC-004 | 5 | `tests/acceptance/stage5/team-hierarchy.acceptance.test.ts` | Over-depth, over-budget and wider-path child grants fail. |
| AC-005 | 1 | `tests/acceptance/stage1/fenced-lease-recovery.acceptance.test.ts` | Higher generation starts only after revocation, isolation or patch-only proof; otherwise quarantine. |
| AC-006 | 1 | `tests/integration/daemon-restart-replay.integration.test.ts` | Process kill and restart restore state and redeliver without duplicating acknowledged actions. |
| AC-007 | 3 | `tests/acceptance/stage3/visibility-degradation.acceptance.test.ts` | Telemetry, renderer and interactive-process losses produce distinct states. |
| AC-008 | 3 | `tests/acceptance/stage3/safe-completion.acceptance.test.ts` | Active write lease or child refuses release until reconciliation and barrier closure. |
| AC-009 | 3 | `tests/acceptance/stage3/unannounced-compaction.acceptance.test.ts` | Missing checkpoint yields `context-unreconciled` and removes barrier/write eligibility. |
| AC-010 | 1 | `tests/acceptance/stage1/configuration-trust-boundary.acceptance.test.ts` | Executable, environment and wider-root project overrides fail before startup. |
| AC-011 | 3 | `tests/acceptance/stage3/crash-after-provider-acceptance.acceptance.test.ts` | Crash after acceptance resolves one action or quarantine, never a second action. |
| AC-012 | 2 | `tests/integration/mcp-two-proxies.integration.test.ts` | Two proxy processes share one store and identical protocol/resource results. |
| AC-013 | 4 | `tests/acceptance/stage4/optional-adapter-degradation.acceptance.test.ts` | Unavailable optional provider times out, records degradation and leaves the required pair unblocked. |
| AC-071 | 4 | `tests/acceptance/stage3/ambiguous-provider-action.acceptance.test.ts`, `tests/integration/daemon-adapter-composition.integration.test.ts` | A task-bound read-only ephemeral review admits only the exact current task/target/slot/head, route request and bundle/coverage inputs; persists one route-bound action plus immutable accepted dispatch receipt before provider I/O; advances immutable evidence and the reserved head before terminal read; exact replay returns the same action/route; crossed or stale input fails before provider work; and no retained agent identity, capability or hidden direct-CLI result is created. |

## Adapter-specific conformance

These deterministic fixture tests supplement the requirement matrix. An
adapter cannot be enabled merely because the shared degradation test passes.

| Stage | Recorded Vitest path |
|---:|---|
| 3 | `tests/acceptance/stage3/adapter-compatibility.acceptance.test.ts` |
| 3 | `tests/acceptance/stage3/primary-adapter-conformance.acceptance.test.ts` |
| 4 | `tests/acceptance/stage4/pi-adapter-conformance.acceptance.test.ts` |
| 4 | `tests/acceptance/stage4/agy-adapter-conformance.acceptance.test.ts` |
| 4 | `tests/acceptance/stage4/cursor-adapter-conformance.acceptance.test.ts` |
| 4 | `tests/acceptance/stage4/kiro-adapter-conformance.acceptance.test.ts` |

## Cross-cutting hardening evidence

| Concern | Vitest path |
|---|---|
| Single daemon owner and attached-client shutdown | `tests/integration/daemon-single-instance.integration.test.ts`, `tests/integration/daemon-attached-client-shutdown.integration.test.ts` |
| Immutable atomic seat renewal and project credential lookup | `tests/unit/seat-store.unit.test.ts`, `tests/acceptance/stage2/mcp-provision.acceptance.test.ts`, `tests/unit/mcp-credentials.unit.test.ts` |
| Disabled or runtime-incompatible adapters cannot join trusted composition | `tests/integration/daemon-disabled-adapter-gate.integration.test.ts` |
| Provider authority and trusted model admission before effects | `tests/acceptance/stage3/provider-session-boundary.acceptance.test.ts`, `tests/unit/adapter-supervisor-model-policy.unit.test.ts` |
| Persistent adapter process reuse | `tests/unit/adapter-supervisor.unit.test.ts` |
| Operation-scoped authority and token rotation | `tests/acceptance/stage1/operation-scoped-authority.acceptance.test.ts`, `tests/acceptance/stage1/retryable-capability-issuance.acceptance.test.ts` |
| Chair/owner/participant scoped reads | `tests/acceptance/stage1/scoped-read-policy.acceptance.test.ts` |
| Message hops, expiry and unresolved-backlog quota | `tests/acceptance/stage1/message-policy.acceptance.test.ts` |
| Artifact/check/gate/checkpoint/handoff barrier evidence | `tests/acceptance/stage2/barrier-evidence.acceptance.test.ts` |
| Full immutable receipt and schema verification | `tests/acceptance/stage1/receipt-export.acceptance.test.ts`, `tests/integration/daemon-receipt-link.integration.test.ts` |
| SQLite connection pragmas on reopen | `tests/integration/sqlite-connection-hardening.integration.test.ts` |
| Symmetric denial/disclosure/qualified-budget public contract | `tests/integration/public-authority-contract.integration.test.ts`, `tests/unit/schema-validation.unit.test.ts` |

## Stage gates

Run from the Provenant product checkout. Each gate is cumulative: `npm test`
executes all deterministic `.test.ts` files implemented through the current
stage.

### Every stage

```sh
npm run build
npm run typecheck --workspace=@local/agent-fabric
npm run test --workspace=@local/agent-fabric -- --run
scripts/check-harness
scripts/public-release-check
git diff --check
python3 skills/deliver/scripts/validate_delivery.py \
  .agent-run/AFAB-001/RUN.json --workspace-root "$PWD" --verify-hashes
```

### Additional gates

| Stage | Command |
|---:|---|
| 1 | `npm run test:unit --workspace=@local/agent-fabric` |
| 1 | `npm run test:integration --workspace=@local/agent-fabric` |
| 1 | `npm run test:acceptance --workspace=@local/agent-fabric` |
| 1 | `npm run test:load --workspace=@local/agent-fabric` |
| 2 | `npm run test:integration --workspace=@local/agent-fabric` |
| 2 | `npm run test:acceptance --workspace=@local/agent-fabric` |
| 3 | `npm run test:integration --workspace=@local/agent-fabric` |
| 3 | `npm run test:acceptance --workspace=@local/agent-fabric` |
| 4 | `npm run test:acceptance --workspace=@local/agent-fabric` |
| 5 | `npm run test:evaluation --workspace=@local/agent-fabric` |
| 5 | `npm run test:load --workspace=@local/agent-fabric` |

Provider activation remains blocked until the selected entry in
`config/adapter-compatibility.yaml` is enabled and its current runtime identity,
protocol and capabilities conform. The default `scripts/model-route` fabric
gate enforces enabled and active adapter plus family and model constraints.
Daemon composition enforces current runtime identity, protocol and capability
conformance. Direct CLI workflows must select `--adapter-gate direct-cli`
explicitly and still obey family and model-pattern constraints.

## Deterministic versus live smoke

All matrix tests are deterministic. They use fake provider processes, fixture
protocol transcripts, temporary Unix sockets, temporary SQLite databases and a
fake Herdr command boundary. They require no provider login, quota, MCP
registration or installed daemon and are the only tests that decide a stage's
machine pass.

Live smoke tests are opt-in operational evidence and remain outside the default
Vitest suite:

| Evidence | Current command/artifact |
|---|---|
| Registered five-seat health | `node runtime/agent-fabric/smoke/registered-mcp-health.mjs` with `AGENT_FABRIC_PROJECT_KEY` |
| Registered Codex/Claude round trip | `node runtime/agent-fabric/smoke/registered-mcp-roundtrip.mjs` with `AGENT_FABRIC_PROJECT_KEY` |
| Live workstation adapter capabilities | `cd runtime/agent-fabric && AGENT_FABRIC_LIVE_COMPATIBILITY_SMOKE=1 npx vitest run tests/integration/adapter-compatibility.live-smoke.test.ts` |
| Agy Gemini / Cursor Grok provider-backed turns | Local-only Fabric request/reply evidence after runtime capability, subscription-auth and seat checks; never enabled in CI |

Run live smoke only when its exact provider-use and credential authority is
current. This is operational evidence, not a separate programme-acceptance
gate:

```sh
AGENT_FABRIC_PROJECT_KEY=<project-key> \
  node runtime/agent-fabric/smoke/registered-mcp-health.mjs
```

A live failure may block operational activation, but cannot invalidate a
deterministic result without a reproduced fixture or a recorded adjudication
bound to objective evidence.
Unavailable credentials or quota produce `not-run` evidence, never a fabricated
pass. Installation, daemon startup and MCP registration remain separately
authorised actions.
