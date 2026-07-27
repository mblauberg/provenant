# Agent Fabric messaging and public protocol

## Execution control, visibility and inbox delivery

Execution control, operator visibility and inbox delivery are independent dimensions. A named profile resolves all three and is accepted only when the selected adapter advertises the required capabilities.

```yaml
profile_dimensions:
  control_mode:
    - managed
    - shared-session-ui
    - attached-interactive
  visibility_mode:
    - none
    - event-mirror
    - provider-tui
  inbox_delivery_mode:
    - structured-push
    - verified-boundary-inject
    - cooperative-pull
    - notify-only
```

Authority, task, mailbox and evidence semantics do not change with the profile. Control strength, delivery latency and direct-input provenance are explicit in the run receipt.

### Headless managed sessions

Provider sessions run through SDK, app-server, RPC or ACP adapters without a dedicated Herdr pane. They require `managed` control and normally use `structured-push`. This is the lowest-overhead profile for mechanical workers and large fan-out.

### Observed managed sessions

The provider session remains owned by its adapter. Herdr starts a read-only `agent-fabric observe` renderer in a pane. The renderer follows the fabric's redacted activity-event cursor and displays bounded status, tool and output events. It cannot send provider turns, acknowledge mailbox messages, acquire leases or mutate task state.

Closing the pane stops only the renderer. Reopening it resumes from the last display cursor or a bounded current snapshot; it never creates another provider session. A provider-native shared-session UI may replace the renderer only when the adapter contract-tests that capability.

The renderer consumes redacted event-envelope version 1, persists only its display cursor, and exits non-zero on schema or authentication failure. Its CLI supports `observe --run <id> [--agent <id>] [--after <cursor>] [--json]`. Renderer reads never claim or acknowledge mailbox deliveries.

Observed managed sessions are recommended for the non-chair primary when direct typing into it is not required. The chair remains the human-driven session and is never replaced by a fabric-owned observer.

### Attached interactive sessions

A provider TUI runs in a terminal the operator controls: either the terminal where the human started it or a Herdr pane opened for it. A running TUI cannot be re-parented into Herdr. A chair outside Herdr remains interactive but has no pane telemetry. Its delivery mode is declared by the adapter:

- `verified-boundary-inject` requires an integration that returns the delivered
  message IDs to the fabric;
- `cooperative-pull` requires the agent to call `fabric_message_receive` and
  `fabric_delivery_acknowledge` at instructed turn boundaries;
- `notify-only` surfaces unread state but cannot satisfy a bounded automatic
  response requirement.

An idle interactive session has no bounded delivery time. The fabric retries wake-ups with backoff and escalates a still-unacknowledged `requires_ack` message to the operator after the configured deadline.

A safe turn boundary is a versioned adapter event emitted after the provider reports no active tool or model turn and before the adapter accepts another turn. Cooperative clients pull the mailbox only at this boundary or on an explicit operator request. Absence of this event keeps delivery pending.

A message is consumed only when the fabric receives an authenticated consume or acknowledgement operation for its ID. Hook invocation, pane focus, terminal input and prompt submission are not consumption evidence. Terminal input is a wake-up capability, never a structured `send_turn`.

Direct operator input may change the active turn outside the task plan. Fabric tools provide an explicit `operator_intervention` operation. When an integration reports an external revision or the operator records an intervention, the task owner reconciles it before barrier closure. If direct-input provenance is unavailable, the receipt records that limitation and interactive task closure requires explicit owner confirmation.

### Profile changes

A profile change that cannot preserve the provider session is a lifecycle rotation: checkpoint, stop delivery, close the adapter-turn lease, attach or spawn the replacement, rehydrate it from the checkpoint, then acknowledge the new generation. Only a contract-tested `shared-session-ui` adapter may add or remove a view without rotation.

### Hybrid profiles

Roles may use different profiles. The default side-by-side profile is:

```yaml
execution_profile:
  name: paired-visible
  default:
    control_mode: managed
    visibility_mode: none
    inbox_delivery_mode: structured-push
  roles:
    chair:
      control_mode: attached-interactive
      visibility_mode: provider-tui
      inbox_delivery_mode: cooperative-pull
    paired-primary:
      control_mode: attached-interactive
      visibility_mode: provider-tui
      inbox_delivery_mode: cooperative-pull
    leader:
      control_mode: managed
      visibility_mode: event-mirror
      inbox_delivery_mode: structured-push
    worker:
      control_mode: managed
      visibility_mode: none
      inbox_delivery_mode: structured-push
  herdr:
    layout: side-by-side
    retain_panes_after_completion: prompt
```

`paired-visible` places both primaries side-by-side only when the chair was launched or attached under Herdr. Otherwise it shows the peer beside an unpaned chair and records `visibility-degraded` for chair pane telemetry only.

The `paired-observed` profile keeps the chair interactive and runs the non-chair primary with managed control, event-mirror visibility and structured push. It provides stronger control over the peer while preserving the human-driven chair.

## MCP and client interface

Claude Code and Codex launch separate stdio MCP proxy processes. Each proxy connects to the same Unix socket and shared daemon. The proxy may safely start the daemon under a single-instance lock when it is absent.

The operation registry is the sole owner of the current MCP tool set. Every active agent-principal operation has an exhaustive `tool` or `none` classification and every `tool` entry owns one stable name. The generated descriptor/reference artifact, rather than a second list in this document, records the Stage 2-5 and the Console contract names. V1 has exactly one descriptor per operation; constant-bound aliases such as a steer-only provider action or a release-only lifecycle action are not additional descriptors. A future variant projection must replace the whole-operation descriptor with a registry-declared exhaustive, non-overlapping set and cannot silently omit an admitted enum member.

Run creation belongs only to reviewed operator launch custody. There is no private or agent `createRun` method, and `fabric_run_create` is never an MCP descriptor. The proxy accepts only an `afc_` agent capability, initialises with `expectedPrincipalKind: agent` and rejects a bootstrap credential before advertising tools.

Private local seat provisioning names an expected prior roster generation and one immutable replacement generation for the exact project/session/run/chair identity. The database owns the sole active generation per project and revokes all capabilities belonging to its predecessor in the same transaction that activates the replacement. Exact current-generation replay is idempotent; stale, rollback and cross-project attempts fail closed.

The generated read descriptors additionally own these resource templates:

- `fabric://runs/{run_id}/status`
- `fabric://runs/{run_id}/tasks`
- `fabric://runs/{run_id}/agents`
- `fabric://runs/{run_id}/receipts`

Unshipped or registry-classified `none` operations are absent rather than stubbed. Stage 2 contract tests verify resource round-trips from both MCP clients. The four URI templates are MCP-native convenience projections of the same generated run-status, task-list, agent-list and receipt-list descriptors; they are not another schema owner. Every surface, including Codex dynamic tools, exposes those four generated read tools even when it has no resource-template channel. Subtree-barrier closure by a leader becomes available with teams in Stage 5; before then `fabric_barrier_close` accepts only chair-owned run or stage scope.

MCP notifications are not assumed to reach every interactive client. Mailbox state and adapter delivery remain authoritative.

The complete current set is generated from the same closed agent-operation codecs and principal registry as the public protocol. The standalone MCP proxy negotiates the authenticated agent protocol before advertising tools; it does not retain a second hand-written method vocabulary. Operations absent from the negotiated features or current authority are absent from the advertised tool list, not present as permissive generic RPC. Inputs and outputs are validated by the shared codecs before and after the daemon call.

Registry classification is compile-time exhaustive. `registerAgent`, `rotateCapability` and any other operation whose result still contains a bearer secret are `none`. Spawn and attach become `tool` only with secret-free public result codecs and shared custody: the daemon generates the target capability, persists only its digest and privately hands plaintext to a bridge-provisioning adapter inside the stable one-effect provider action. The model-visible result contains agent/action/session identity plus `bridgeState` and `bridgeGeneration`, never the token. An adapter that cannot provision a bridge advertises that fact before dispatch; attach may then register a bridge-less mailbox/wake-up participant with `bridgeState: none`, but neither attach nor spawn fabricates a live Fabric tool surface. A supported retained bridge must complete a later provider-originated Fabric call before it is claimed active. Attach remains registry-classified `tool` regardless of the selected adapter's bridge capability; `bridgeState` reports the runtime outcome.

Every model-visible result is the exact closed public result codec. Opaque provider output is replaced by typed contract evidence and/or a digest before projection. A validated bounded `providerAnswer` remains available only for a non-review task-bound ephemeral spawn. A certifying review projects its answer digest and safe parsed result, never raw text. `additionalProperties: true`, raw provider JSON and copied output schemas are forbidden.

A launched chair receives this same current, principal-scoped MCP operation surface through the secret-consuming provider-session bridge. Its one-use attestation operation is also registry-classified `tool`, but only the launch-attestation feature/grant projects it; standalone proxies cannot see it. “Private” means grant-scoped and one-use, not registry-exempt. Claude SDK MCP tools and Codex app-server dynamic tools may use provider-specific transport descriptions, but their Fabric names, schemas, authority results and receipts are generated from the same descriptors. The provider session must originate every tool call. The adapter wrapper may route and validate an attributed call but cannot invoke a Fabric operation on the model's behalf and report that as session activity. Later turns reuse the same retained bridge; a resume reference without it exposes no Fabric tools and follows chair-loss recovery.

## Observability and operator control

The fabric exports `<run-dir>/fabric-receipt.json` as generated coordination evidence. It does not own human acceptance, delivery completion or the final gate. The chair-owned `.agent-run/<run-id>/RUN.json`, using `contract: delivery-run` and `schema_version: 1`, remains authoritative. It declares the fabric receipt as an evidence artifact with a workspace-relative path and SHA-256 digest; no second run-receipt shape is adopted. Fabric receipt schema version 2 is the only current codec. No v1 decoder/import/projection exists; non-current files are preserved but rejected as protocol evidence.

`schemas/fabric-receipt.v2.schema.json` is the normative standalone Draft 2020-12 schema. Every object has `additionalProperties: false`; every property shown below is required unless its value explicitly admits null; every reference is local `#/$defs/...`. The runtime shall delete the v1 decoder/import/ projection and fixtures rather than accepting either shape.

The root required properties are exactly:

~~~yaml
$schema: https://json-schema.org/draft/2020-12/schema
type: object
additionalProperties: false
required: [schemaVersion, runId, chair, taskOwners, agents, executionProfile,
  directInputProvenance, reviewCompletion, providerRoutes, providerReviews,
  findingPages,
  routeIntegrityRecoveries, taskAndWriteLeases, messageAndDeliveryCounts,
  objectiveChecks, providerFailuresAndSubstitutions, operatorInterventions,
  compactionsAndRotations, eventWatermark, counts, stateHash]
properties:
  schemaVersion: {const: 2}
  runId: {$ref: '#/$defs/id'}
  chair: {$ref: '#/$defs/chair'}
  taskOwners: {$ref: '#/$defs/taskOwners'}
  agents: {$ref: '#/$defs/agents'}
  executionProfile: {$ref: '#/$defs/executionProfile'}
  directInputProvenance: {$ref: '#/$defs/directInput'}
  reviewCompletion: {$ref: '#/$defs/reviewCompletion'}
  providerRoutes: {$ref: '#/$defs/providerRoutes'}
  providerReviews: {$ref: '#/$defs/providerReviews'}
  findingPages: {$ref: '#/$defs/findingPages'}
  routeIntegrityRecoveries: {$ref: '#/$defs/recoveries'}
  taskAndWriteLeases: {$ref: '#/$defs/leases'}
  messageAndDeliveryCounts: {$ref: '#/$defs/messageCounts'}
  objectiveChecks: {$ref: '#/$defs/objectiveChecks'}
  providerFailuresAndSubstitutions: {$ref: '#/$defs/providerFailures'}
  operatorInterventions: {$ref: '#/$defs/interventions'}
  compactionsAndRotations: {$ref: '#/$defs/lifecycleRows'}
  eventWatermark: {$ref: '#/$defs/nonnegative'}
  counts: {$ref: '#/$defs/counts'}
  stateHash: {$ref: '#/$defs/digest'}
~~~

Common local scalars are `id` (`type:string`, `minLength:1`, `maxLength:256`), `nonnegative` (`type:integer`, minimum 0), `positive` (`type:integer`, minimum 1), `boolean`, `digest` (`type:string`, lowercase pattern `^sha256:[0-9a-f]{64}$`), `nullableDigest` (`digest|null`) and `timestamp` (RFC 3339 `date-time`). JSON Schema length counts Unicode code points, not UTF-8 bytes. After schema validation and before projection/hash, the runtime therefore performs a mandatory UTF-8 byte validator: every `id` is 1..256 bytes, finding ID 1..64, safe summary 1..256 and safe evidence 1..768. No schema-valid value bypasses that validator. Closed object definitions have these exact property sets, scalar mappings and null rules. The following is binding schema shorthand, not a claim that keys such as `nullOnly` are JSON Schema vocabulary; the checked-in schema must expand every line into standard `properties`, `required`, `enum`, `oneOf` and conditional constraints:

~~~yaml
topReviewBlockerEnum: [certifying-review-capability-unavailable,
  finding-capacity-exhausted, missing-target, stale-target,
  profile-unavailable, integrity-failure]
slotReviewBlockerEnum: [missing-evidence, nonterminal-action,
  ambiguous-action, provider-terminal-failure, terminal-no-effect,
  retired-unknown, route-integrity, insufficient-read-coverage,
  noncertifying, actual-route-mismatch, actual-route-unproved,
  unusable, wrong-artifact, wrong-bundle, wrong-route,
  wrong-provider, wrong-model, wrong-chair-generation,
  reviewer-family-distinctness, open-findings]
reviewCurrencyBlockerEnum: [certifying-review-capability-unavailable,
  finding-capacity-exhausted, missing-target, stale-target,
  profile-unavailable, integrity-failure, missing-evidence,
  nonterminal-action, ambiguous-action, provider-terminal-failure,
  terminal-no-effect, retired-unknown, route-integrity,
  insufficient-read-coverage, noncertifying, actual-route-mismatch,
  actual-route-unproved, unusable, superseded,
  wrong-artifact, wrong-bundle, wrong-route, wrong-provider, wrong-model,
  wrong-chair-generation, reviewer-family-distinctness, open-findings]
providerTerminalFailureEnum: [max-turns-exhausted, provider-rejected,
  terminal-no-answer, adapter-terminal-failure]
reviewerFamilyRelationEnum: [same-family-exempt, distinct-family-proved,
  same-family-forbidden, family-unproved]

objectiveCheckKindEnum: [test, evaluation, load, migration,
  generated-contract]
providerFailureOrSubstitutionEnum: [adapter-unavailable,
  adapter-contract-mismatch, provider-unavailable, provider-timeout,
  provider-rejected, provider-response-invalid, route-rejected,
  model-unavailable, capability-unavailable, quota-exhausted,
  substitution-applied, optional-leg-degraded]
operatorInterventionOperationEnum: [pause, resume, steer, cancel, drain, stop,
  launch, takeover, git, git-authorise, git-custody-resolve,
  agent-lifecycle-recovery-issue, external-effect]
ProviderActionRefV1:
  required: [adapterId, actionId]
  ids: [adapterId, actionId]

resolvedEffortV1:
  oneOf:
    - kind: applied
      required: [kind, value]
      valueType: id
    - kind: inapplicable
      required: [kind]

chair:
  required: [agentId, principalGeneration, chairLeaseGeneration,
    providerSessionGeneration, bridgeGeneration, adapterId,
    adapterContractDigest, modelFamily, model, routeReceiptDigest]
  nullOnly: [routeReceiptDigest]
  ids: [agentId, adapterId, modelFamily, model]
  positive: [principalGeneration, chairLeaseGeneration,
    providerSessionGeneration, bridgeGeneration]
  digests: [adapterContractDigest]
  nullableDigests: [routeReceiptDigest]

taskOwner:
  required: [taskId, taskRevision, taskState, ownerAgentId, ownerLeaseId,
    ownerLeaseGeneration, ownerLeaseState, membershipRevision]
  taskStateEnum: [blocked, ready, active, complete, cancelled, degraded]
  ownerLeaseStateEnum: [active, frozen, released, revoked, abandoned]
  ids: [taskId, ownerAgentId, ownerLeaseId]
  positive: [taskRevision, ownerLeaseGeneration, membershipRevision]

agent:
  required: [agentId, role, lifecycle, contextState, principalGeneration,
    providerSessionGeneration, bridgeGeneration, currentTaskId,
    checkpointDigest, membershipRevision, membershipState]
  nullOnly: [currentTaskId, checkpointDigest]
  lifecycleEnum: [starting, ready, busy, checkpointing, idle, suspended, archived]
  contextStateEnum: [current, context-unreconciled]
  roleEnum: [chair, leader, worker, reviewer]
  membershipStateEnum: [active, released, abandoned]
  ids: [agentId, currentTaskId]
  positive: [principalGeneration, providerSessionGeneration, bridgeGeneration,
    membershipRevision]
  nullableDigests: [checkpointDigest]

executionProfile:
  required: [profileId, profileSchemaDigest, resolvedProfileDigest,
    authorityDigest, budgetDigest]
  ids: [profileId]
  digests: [profileSchemaDigest, resolvedProfileDigest, authorityDigest,
    budgetDigest]

directInput:
  required: [state, attestations]
  stateEnum: [complete, partial, unavailable]
  attestationRequired: [attestationId, providerMessageId, operatorId,
    provenanceDigest, evidenceDigest]
  attestationIds: [attestationId, providerMessageId, operatorId]
  attestationDigests: [provenanceDigest, evidenceDigest]

targetChair:
  required: [agentId, bindingGeneration, principalGeneration,
    chairLeaseGeneration, providerSessionGeneration, bridgeGeneration,
    adapterId, adapterContractDigest, modelFamily, model, routeReceiptDigest]
  ids: [agentId, adapterId, modelFamily, model]
  positive: [bindingGeneration, principalGeneration, chairLeaseGeneration,
    providerSessionGeneration, bridgeGeneration]
  digests: [adapterContractDigest]
  nullableDigests: [routeReceiptDigest]

localProviderRoute:
  required: [schemaVersion, routeRequestDigest, routeReceiptDigest, adapterId,
    adapterContractDigest, providerFamily, resolvedModel, requestedEffort,
    resolvedEffort, targetGeneration, slot, reviewedArtifactRef,
    publicationLineageDigest, bundleDigest, manifestRootDigest,
    coverageDigest, bundleSearchIndexDigest, riskReadMapDigest,
    mandatoryReadSetDigest, finalPromptDigest, targetChair,
    profileDigest, slotHeadGeneration, attemptGeneration]
  schemaVersion: {const: 1}
  nullTogetherForNonReview: [targetGeneration, slot, reviewedArtifactRef,
    publicationLineageDigest, bundleDigest, manifestRootDigest,
    coverageDigest, bundleSearchIndexDigest, riskReadMapDigest,
    mandatoryReadSetDigest, finalPromptDigest, targetChair, profileDigest,
    slotHeadGeneration, attemptGeneration]
  targetChairType: targetChair-or-null
  requestedEffortType: id-or-null
  resolvedEffort: resolvedEffortV1
  conditional: resolvedEffort.inapplicable requires requestedEffort null;
    resolvedEffort.applied admits requestedEffort null only as configured-default
  ids: [adapterId, providerFamily, resolvedModel]
  nullableIds: [reviewedArtifactRef]
  digests: [routeRequestDigest, routeReceiptDigest, adapterContractDigest]
  nullableDigests: [publicationLineageDigest, bundleDigest,
    manifestRootDigest, coverageDigest, bundleSearchIndexDigest,
    riskReadMapDigest, mandatoryReadSetDigest, finalPromptDigest,
    profileDigest]
  nullablePositive: [targetGeneration, attemptGeneration]
  nullableNonnegative: [slotHeadGeneration]
  slotEnum: [native, other-primary, cursor-grok, agy-gemini, null]

providerRoute:
  required: [actionRef, taskId, route, admission, capabilitySummary,
    latestDispatch, observation]
  actionRef: ProviderActionRefV1
  route: localProviderRoute
  admission: deployedRouteAdmissionV1
  capabilitySummary: capabilitySnapshotSummaryV1
  latestDispatch: deployedRouteDispatchV1-or-null
  observation: deployedRouteObservationV1-or-null
  ids: [taskId]
  invariant: actionRef equals admission.actionRef;
    actionRef.adapterId equals route.adapterId;
    admission.routeRequestDigest equals route.routeRequestDigest;
    admission.routeReceiptDigest equals route.routeReceiptDigest;
    admission.requested.rawProviderEffort equals route.requestedEffort;
    admission.admitted.adapterId equals route.adapterId;
    admission.admitted.adapterContractDigest equals route.adapterContractDigest;
    admission.admitted.family equals route.providerFamily;
    admission.admitted.model equals route.resolvedModel;
    admission.admitted.resolvedEffort deep-equals route.resolvedEffort;
    capabilitySummary.admission snapshot identity/body equal admission;
    capabilitySummary.dispatch and latestDispatch are both null or equality-copy
      the same dispatch snapshot ref/source/clocks/body;
    nonnull latestDispatch actionRef/admissionDigest/effectiveConfigurationRef
      equal admission;
    nonnull observation actionRef/admissionDigest equal admission

The receipt's `deployedRouteAdmissionV1`, `capabilitySnapshotSummaryV1`,
`deployedRouteDispatchV1`, `deployedRouteObservationV1`,
`discoverySurfaceRefV1`, `adapterEffectiveConfigurationRefV1` and typed
`observedValueV1` names above are local `$defs`. Their byte shapes are generated
once from the same protocol definitions used by public reads and are embedded
inside the standalone receipt schema. They are not external references and
need no runtime resolver or registry.

~~~

Count objects are fully named:

~~~yaml
messageCounts:
  required: [mailbox, resultDelivery, deliveryWatermark]
  mailboxRequired: [ready, claimed, acknowledged, abandoned, expired]
  resultDeliveryRequired: [pending, claimed, providerAccepted, consumed,
    overdue, abandoned]
  counterType: every displayed leaf is nonnegative

counts:
  required: [taskOwners, agents, providerRoutes, providerReviews,
    findingPages,
    routeIntegrityRecoveries, taskLeases, writeLeases, objectiveChecks,
    providerFailuresAndSubstitutions, operatorInterventions,
    compactionsAndRotations, mailboxTotal, resultDeliveryTotal]
  counterType: every displayed property is nonnegative
~~~

All displayed enum strings are literal local `$defs` enums. This includes objective-check kind, provider failure/substitution event code, review terminal- failure code and every registry- closed operator value used by the receipt. The schema has no resolver, catalogue URI, dynamic reference or runtime registry dependency. A catalogue change regenerates and versions this standalone schema; an unknown future code fails validation. Provider-specific raw codes and detail remain private behind the corresponding evidence digest. Every array item uses its local object definition. Arrays are strictly ascending and unique by these tuples: `taskOwners(taskId)`, `agents(agentId)`, `directInput.attestations(attestationId)`, `providerRoutes(actionRef.adapterId,actionRef.actionId)`, `providerReviews(record.targetGeneration,record.slot,record.attemptGeneration,   recordKind,record.actionRef.adapterId,record.actionRef.actionId)`, `findingPages(pageDigest)`, `routeIntegrityRecoveries(actionRef.adapterId,actionRef.actionId,recoveryGeneration)`, `taskAndWriteLeases(leaseKind,leaseId)`, `objectiveChecks(taskId,checkId)`, `providerFailuresAndSubstitutions(actionRef.adapterId,actionRef.actionId, eventGeneration)`, `operatorInterventions(commandId)` and, by lifecycle union arm, `compactionsAndRotations(agentId,custody,custodyId,targetProviderGeneration)` or `(agentId,generation-loss,generationLossId,newProviderGeneration, newContextRevision)`. `findingPages` contains exactly one full page for every page digest reachable from any receipt evidence/completion finding-set reference, with no orphan or missing page. Every finding page is strictly ascending and unique by `(findingDigest,findingId)` using lowercase UTF-8 byte order, and page ranges are nonoverlapping and strictly ascending. Every digest-only array is strictly ascending and unique by its lowercase digest bytes. Each `findingDigest` is SHA-256 over RFC 8785 JCS of that complete safeFinding object with `findingDigest` omitted, so it is neither caller-selected nor self- referential. Slot order is native, other-primary, cursor-grok, agy-gemini.

`fabric.v1.receipt.export` is bounded two-phase publication. Phase A opens one read snapshot, fixes `eventWatermark`, captures every projection-owner revision and captures the exact external currency tokens used by review completion: Git object format/base/full HEAD/head tree/index tree/worktree-clean state, repository source-state digest and every registered external source/evidence revision/digest. It runs each producer at that watermark and writes only a private temporary candidate. Phase B opens a new read transaction, equality- rechecks all captured database revisions, and reruns the fixed no-follow Git/ external-source token reads before atomic publication. Any drift discards the candidate and boundedly retries or fails; it can never publish `finalReviewComplete:true` for bytes that became stale between projection and write. Only the current slot evidence arm in providerReviews must equal the corresponding resolved reviewCompletion evidence slot. A current terminal- failure arm instead equals the slot action/attempt/failure/result and proves its head/open/repair set digests did not advance. Historical evidence rows form contiguous prior/new-head and prior-evidence chains; historical failure rows leave those chains unchanged. A recovery with `routeState=present` equals one providerRoutes row. `missing` and `integrity-failed` instead require a null route-receipt digest and non-null safe recovery-evidence digest and cannot reconstruct a route. Chair/agent/ task/lease/run identities and every route/result/bundle/head digest otherwise equality-join, and counts equal array lengths and the sums of the explicitly named state counters. Missing, duplicate, extra or crossed rows fail export.

Canonical bytes are RFC 8785 JCS, UTF-8, with no BOM or trailing newline. `stateHash` is omitted during canonicalisation, then set to lowercase `sha256:<64hex>` over those exact bytes; export canonicalises again with the field present. No caller supplies a row. The receipt contains no private answer, diagnostic, usage, bundle byte, prompt or capability. `delivery-run` v1 remains separate and is never a receipt-v2 nested codec.

Herdr panes show provider, model family, role, task, lifecycle, context pressure, unread message count and current lease generation where integrations permit. The operator may pause, steer, cancel or focus an agent through the fabric or Herdr. Every fabric-mediated intervention and every intervention reported by a provider or Herdr integration is journalled. Unattributable direct terminal input is not fabricated as a receipt event.

### Revisioned intake and scoped gates

Task intake is a Fabric entity with stable `intake_id`, monotonically increasing revision and states `draft`, `awaiting-chair`, `discussing`, `awaiting-human`, `accepted`, `deferred` or `cancelled`. Submission commits the intake revision, gate references and artifact digests inside its correlated chair request before any wake-up. A duplicate dedupe key has one effect. Replies, plan revisions and artifact digests update the same intake after daemon, Console or provider restart or provider compaction.

A scoped gate records:

```yaml
scoped_gate:
  gate_id: stable-id
  project_session_id: stable-id
  coordination_run_id: stable-id
  scope_kind: task-or-subtree-or-run-or-release
  affected_task_ids: []
  dependency_revision: compare-and-set-integer
  blocked_operation_ids: []
  enforcement_points: [task-readiness, operation, scoped-barrier]
  question: human-readable
  reason: human-readable
  options: []
  recommendation: human-readable-or-empty
  consequences: []
  evidence_refs: []
  revision: compare-and-set-integer
  created_by_ref: authenticated-operator-or-explicit-policy
  expected_approver_ref: authenticated-operator-or-explicit-policy
  resolved_by_operator_id: optional-until-human-resolution
  deadline: optional
  default: optional-non-approving-action
  status: pending-or-approved-or-rejected-or-deferred-or-cancelled-or-superseded
  release_binding: optional-accepted-delivery-receipt-artifact-digest-action-and-target
```

Gate creation, dependency changes and resolution are transactional. The daemon advances the owning project session's membership revision exactly once whenever gate creation adds required membership or terminal gate resolution/supersession settles it. A stale membership client cannot remain current across either change. The daemon rechecks applicable unresolved gates before task claim, start and resume; before each named consequential operation; and before the matching scoped barrier closes. Dependent descendants block only where the persisted scope and dependency revision require it. Unrelated siblings remain runnable. A timeout may alert or defer but never approves.

An operation enforcement check is always target-bound. Its closed target is either `{kind: run}` for an operation whose effect belongs to the exact `coordinationRunId`, or `{kind: task, taskId}` for an operation whose effect belongs to one exact task in that run. The request also carries the current run-owned `dependencyRevision`. A task/subtree gate matches only the task form when that task occurs in the gate's affected-task binding at the same dependency revision; it never blocks a run target or an unrelated sibling. A run/release gate may match either form inside its exact run. Omission, a task from another run, a stale dependency revision and substitution of the run form for a task-owned effect fail closed rather than widening or bypassing the gate.

`dependency_revision` is the owning coordination run's dependency-graph revision. Every dependency-edge or affected-set mutation increments it and, in the same transaction, recomputes descendants and rebinds every applicable open gate. Newly added descendants become blocked immediately; removed descendants become unblocked only after the rebinding commits. A graph mutation that cannot produce a complete rebind fails and retains the prior graph and gate revision.

Policy may create, defer, cancel or notify about a gate. Spec, one-way-door, release, external-effect, irreversible-action and final-acceptance gates require an authenticated human as both expected approver and resolver; policy can never approve them. Release-scoped gates additionally bind the exact accepted delivery receipt, artifact digest, promotion action and target, directly or by a schema-validated release receipt. Broad session or `external-effect` authority cannot satisfy that binding.

Final acceptance uses the same gate authority rather than a parallel approval record. The public `acceptance_ref` is the digest of the complete sorted array:

```text
sha256(canonical-json({
  kind: "project-session-final-acceptance",
  projectSessionId,
  gates: [
    {
      gateId,
      coordinationRunId,
      gateRevision,
      status: "approved",
      resolution,
      evidenceRefs
    }
  ]
}))
```

The `gates` array is non-empty and sorted by `coordinationRunId`, then `gateId`; it contains exactly one binding for every run currently awaiting acceptance and no binding for terminal history. Only gates satisfying the project-session ownership contract may produce that reference. Historical terminal runs do not need to be reopened for session acceptance: `closed` run memberships are reconciled, `cancelled`/terminal `launch_failed` memberships are explicitly abandoned, and only current `quiescing` runs transition to `awaiting_acceptance`. Reopen and accepted close mutate only runs currently in `awaiting_acceptance`; pre-existing terminal history retains its terminal state. Reopen supersession increments each affected gate revision, retains its prior resolution only as audit evidence, and removes it from the approved candidate set; it does not fabricate a replacement approval. An approved/rejected human-resolved gate and its later superseded audit row remain reconciled history. A cancelled gate, or a pending/deferred gate ended by `system-supersession`, is abandoned with an explicit durable reason.

Identifier-only task gates do not exist in the current baseline. Only scoped gates with explicit enforcement bindings are admitted; no run or release scope is inferred. There is one gate owner. Approver-less gate creation and resolution fail closed.

### Hierarchical resource admission

Budgets extend from project to project session, coordination run, team and agent. Every dimension uses the existing unit-key rules and reports `used`, `reserved`, `remaining` and `unknown`. Admission reserves every affected ancestor atomically before dispatch, so concurrent runs cannot overbook a project or session. Terminal completion releases unused reservation; ambiguous effects retain their stable reservation until reconciliation. Unknown hard usage freezes new reservations when remaining capacity cannot be proven, while already-authorised bounded work may reach a terminal state. Child limits only narrow their parent.

Active writer admission additionally records canonical source prefixes, repository root, repository-owned worktree path and writer generation. The daemon rejects intersecting active prefixes before launch. A worktree does not replace authority, sandbox or predecessor-revocation evidence.

### Atomic request, result and callback delivery

Answer-bearing paired work shall create a task and correlated request message before Herdr wake-up. The request binds task and request revisions, conversation and message IDs, target agent/provider session, expected artifacts, acknowledgement requirement, dedupe key, response deadline and a stable callback ID.

The peer's correlated reply, terminal task result and pending result-delivery obligation commit in one SQLite transaction or one transactionally equivalent outbox transition. None may be externally visible without the others. Result delivery is distinct from mailbox acknowledgement and has states `pending`, `claimed`, `provider-accepted`, `consumed`, `overdue` and `abandoned`. Its claim generation, callback ID, request/reply/task revisions and payload digest make claim, injection and consumption idempotent across daemon, Console, requester restart and compaction.

A deadline moves a still-required result to `overdue`, alerts the requester and keeps its dependent barrier open. Same-action retry, reassignment or abandonment is an explicit revisioned transition; the fabric never blindly redispatches. A late reply remains evidence and cannot complete reassigned or abandoned work. Pane state and scrollback never satisfy result delivery.

`fabric_task_request` commits the task, request, recipient deliveries, response deadline, callback and dependent-barrier link before wake-up. `fabric_task_complete_with_reply` verifies the task owner, lease generation, task/request revisions and callback generation, then atomically commits the reply, terminal result, artifact references and pending callback. Typed claim, provider-accept, consume, same-action retry, reassign and abandon operations complete the result-delivery state machine.

### Chair takeover

Chair loss freezes the old chair generation, delivery and new authority grants. Takeover succeeds only when the old generation is revoked or otherwise fenced, a persisted handoff digest exists, and a takeover-capable operator command matches the project session, run, expected chair generation and revisions. The reassignment and new chair lease commit atomically. Peer presence, pane presence or lease expiry alone cannot promote a chair.

### Public protocol surface

The shared typed client shall expose project-session, membership, intake, operator-command, scoped-gate, resource-reservation, request-result and takeover operations. Agent MCP operations remain principal-scoped; the Console uses a separate operator client and shall not import daemon internals. New operations are absent from a client whose negotiated protocol capability does not include them.

### Added requirements and acceptance scenarios

- **FR-020:** Project-session creation, membership and lifecycle transitions
  shall be revisioned and atomic with their closure predicates.
- **FR-021:** Operator principals and commands shall enforce exact project,
  action, generation, expiry and revision boundaries with idempotent audit.
- **FR-022:** Scoped gates shall block only their persisted task/subtree/run or
  release scope at every declared enforcement point.
- **FR-023:** Intake discussion shall bind the intake revision, gates and
  artifact digests into its correlated request and survive duplicate   submission, restart and compaction with one stable intake identity.
- **FR-024:** Project/session/run/team/agent budgets shall reserve and reconcile
  every configured dimension without overbooking.
- **FR-025:** Correlated reply, terminal task result and result-delivery outbox
  shall be atomic and replay-safe.
- **FR-026:** Chair takeover shall require generation fencing, a bound handoff
  and an exact takeover capability.
- **FR-027:** Result-delivery claim, deadline, retry, reassignment,
  abandonment and consumption shall persist independently of mailbox delivery.
- **NFR-011:** Console and other operator clients shall use only the negotiated
  public protocol and shall never mutate SQLite directly.
- **NFR-012:** Duplicate and crash-replayed session, intake, gate, request,
  completion and delivery commands shall have one durable effect.
- **NFR-013:** Operator audit shall record authenticated actor, provenance,
  command ID, revisions, before/after state and evidence without capability   values.
- **NFR-014:** Project-session protocol shall remain usable without Console,
  Herdr or GitHub.

### Typed repository reads and Activity message binding

The public operator protocol adds `operator-repository-read.v1` with `fabric.v1.operator-repository.read`. The request authenticates an operator, binds the exact project and optional project session, carries the current operator snapshot revision, and selects either the trusted project root or an exact canonical worktree admitted to that session. It accepts only typed diff selectors (`working-tree`, `staged`, or two exact object digests) plus bounded log cursor and limit. It accepts no command, shell, argument vector, arbitrary Git subcommand, environment override or caller-selected repository outside the trusted project.

The daemon derives and rechecks the repository/worktree boundary, invokes only its fixed Git-read port, and returns one `GitRepositoryProjection` containing:

- canonical repository and worktree paths;
- head ref, detached state and exact object digest;
- head, index, worktree and remote state digests compatible with
  `GitRepositoryBinding` mutation fences;
- bounded staged, unstaged, untracked and conflicted paths with an explicit
  truncation marker;
- typed merge, rebase, cherry-pick, bisect or clean operation state;
- optional remote/branch upstream with ahead/behind counts;
- an immutable diff artifact reference and its exact base/target digests;
- a bounded cursor-paged log of object digest, parent digests, subject and
  author timestamp;
- bounded typed branch and worktree records with explicit truncation; and
- a separately fresh `ProjectionFact` for optional hosted checks.

Local Git and hosted GitHub facts have independent source, revision, freshness and observation time. GitHub absence, outage or staleness cannot make the local Git result unavailable or stale. A changed operator snapshot returns `resnapshot-required`; a repository change between observation and response returns a new repository state digest and never fabricates snapshot stability. The v2 Project row carries only bounded status/count/upstream fields and the repository state digest; Project detail and repository-read results carry the full typed projection. Pagination and truncation are explicit and bounded.

The protocol also adds:

```yaml
message_body_ref:
  project_id: exact-project
  project_session_id: exact-session
  coordination_run_id: exact-run
  message_id: exact-message
  expected_revision: positive-integer
```

`ActivityViewItem`, the v2 Activity summary and Activity detail carry `messageBodyRef`. An Activity item of kind `message` requires the exact ref; every other kind forbids it. Row, detail and `MessageBodyReadRequest` preserve the same project/session/run, message and revision. The body read remains separately authorised and revision-fenced; its result must still prove terminal-control neutralisation and capability-value redaction. No event ID is guessed or reinterpreted as a message ID.

Acceptance additionally requires:

- **AC-023:** status, diff, log, branches, worktrees, upstream and checks survive
  Project row-to-detail/repository-read projection with exact state digests,   explicit pagination/truncation and independent Git/GitHub degradation; no   typed or runtime surface accepts arbitrary Git execution; and
- **AC-024:** message Activity rows require and preserve the exact message-body
  reference, non-message rows reject one, stale revisions fail closed, and the   Console can read the full neutralised/redacted body without deriving IDs.

Acceptance adds:

- **AC-015:** the full operator-capability negative matrix and exact takeover
  bindings fail closed, including independent `drain` and `stop` authority;
- **AC-016:** each scoped-gate enforcement point blocks only its affected
  dependency set; added and removed descendants rebind atomically and policy   auto-approval of a human-only gate fails;
- **AC-017:** concurrent resource admission cannot overbook any ancestor and
  unknown usage remains honest after restart;
- **AC-018:** duplicate discussion, restart and compaction retain one revisioned
  intake and one correlated request bound to the exact intake revision, gates   and artifact digests;
- **AC-019:** crash injection exposes either all or none of task/request and
  reply/result/callback composite effects; and
- **AC-020:** safe-boundary delivery, busy/idle requester behaviour, overdue,
  retry, reassignment, abandonment and late reply preserve the dependent   barrier and never use pane state as delivery evidence.

No session or run is imported from an earlier database epoch. Every current session has an explicit current operator origin and every run enters through reviewed launch custody.

### Current-agent MCP parity and launched-chair surface

The canonical MCP descriptor set is the `tool` projection of the active agent-principal operation registry and its closed protocol codecs. The registry, not surrounding MCP prose, is the sole membership and stable-name owner. Every active agent operation is explicitly classified, and adding or removing an operation without a projection classification fails the build. A descriptor owns one stable tool name, protocol operation, input codec, output codec, receipt renderer, optional resource-template URI and required negotiated feature. The run-status, task-list, agent-list and receipt-list resource templates resolve through their generated read descriptors; every projection exposes the read tools even when it cannot expose MCP resources. Standalone proxies and provider- session bridges import those descriptors; neither copies schemas or accepts an arbitrary method name.

The standalone proxy authenticates once, negotiates an agent principal and advertises only descriptors present in the negotiated grant. Every call uses that connection identity and is reauthorised at the daemon. A tool argument cannot substitute another capability, run, chair, session generation or principal. Provider-session bridges apply the same rule and additionally bind the live provider invocation to the launched session reference/generation and retained bridge generation. Secret handoff material is never a tool argument, model-visible descriptor, result, receipt or error.

One V1 descriptor projects one complete operation codec. Constant-bound aliases are prohibited. A later exhaustive variant set may replace that descriptor only when every admitted discriminator value is covered exactly once, every binding is registry-owned and the daemon still parses and reauthorises the complete canonical operation. Result descriptors are exact closed codec projections; provider payload/result fields cannot remain open JSON at the model boundary.

Spawn/attach capability issuance uses the same hash-only, prepare-before-I/O, dispatch-once and lookup-only custody owner as chair launch. Their public results are secret-free. Bridge-capable adapters consume the target credential only through volatile private handoff and retain the exact generation-bound transport; bridge-incapable attach reports `bridgeState: none` while the attach operation remains registry-classified `tool`. No wrapper, calling model, MCP proxy persistence or protocol result relays a child token.

The launched-chair surface shall support real coordination, not only attestation or mailbox probing. After attestation, a later provider-originated turn must successfully perform at least one schema-derived Fabric operation through the same retained bridge. Standalone Claude-labelled and Codex-labelled MCP proxies, Claude SDK in-process MCP and Codex dynamic-tool projections must produce schema-equivalent success and closed failure for the same authorised fixture. Adapter or bridge loss removes/degrades the affected surface without killing the daemon, acknowledging a message, completing a task or fabricating continuity.

- **FR-033:** The current agent MCP surface shall be generated from the shared
  principal-scoped operation registry and closed protocol codecs, and a launched   chair shall receive that same authorised surface through its retained bridge.
- **NFR-018:** MCP proxies and provider-session tool projections shall expose no
  generic arbitrary RPC, capability substitution, duplicated schema owner or   wrapper-originated call evidence.
- **AC-027:** CI conformance uses the real Claude/Codex adapters against fake
  provider transports and proves complete tool-name/schema/resource-read parity,   native provider invocation attribution, wrong-principal/generation/feature   rejection, malformed input/output handling, wrapper-self-call rejection and   bridge-loss fencing. A separate real-provider dogfood gate uses only an   already authenticated installation under explicit run authority, performs a   later-turn current-agent Fabric call through the original retained bridge and   records `not-run` rather than passing when login or provider access would be   required. No test may claim that fake transport proves provider authenticity.
- **FR-036:** Every active agent operation shall have one registry-owned MCP
  projection classification; the generated `tool` set is complete for the   negotiated grant and every `none` operation is absent from all projections.
- **FR-037:** Spawn/attach shall return no bearer secret and shall use shared
  effect custody for any private target-bridge provisioning.
- **NFR-021:** Bootstrap authority, plaintext capabilities, raw provider output
  and open result schemas shall never reach an MCP descriptor, structured   result, text receipt, resource, error, log or persisted proxy state.
- **AC-030:** Descriptor drift tests reject an unclassified operation, copied
  schema, duplicate operation name, secret-bearing result, bootstrap token,   incomplete variant set or projection mismatch. Real-adapter/fake-provider   spawn and attach tests prove secret-free results, exact bridge-state honesty,   later-turn calls over supported retained bridges, post-activation child loss   revocation/generation fencing and no fabricated surface for unsupported   interactive attachment. The launch-attestation descriptor is registry-owned,   grant-scoped to launch and absent from standalone proxies.

### Bounded operator artifact-content reads

the Console contract requires the Console to review the actual immutable artifact, not only its path and digest. The public operator protocol therefore adds negotiated optional feature `artifact-content-read.v1` and read-only operation `fabric.v1.operator-artifact-content.read`. This is a projection read. It does not publish an artifact, acknowledge evidence, resolve Attention, grant authority or create effect custody.

Every spec, ADR, diff, decision, test, review and receipt exposed in the Console has one daemon-owned `EvidenceArtifactRegistration`. This is immutable projection metadata in the canonical `artifacts` relation, not another byte store. It binds an evidence ID/revision/kind, exact project/session/run scope, fixed source owner, artifact ref, publisher provenance and creation time. The three V1 source owners are `project-file`, `run-file` and `git-private-diff`. A content read accepts no caller-selected source kind or root.

Registration also snapshots publication-time provider lineage. The closed snapshot always records publisher kind/reference and, for an agent publisher, the exact publisher agent and principal generation. When one current provider custody is provable it additionally records that custody action and provider- session generation, adapter, admitted model family/model and immutable route receipt digest when available. Its state is `proved` or `unproved` with a closed reason; absent, multiple or crossed custody is `unproved`. The daemon canonicalises and digests this snapshot in the artifact-registration transaction. Later agent rotation, route admission, artifact-kind promotion or registry revision cannot rewrite it.

Negotiated feature `artifact-registry.v1` adds `fabric.v1.evidence.publish` for an authenticated agent to register an exact `project-file` or `run-file` already inside its current artifact-path authority. The closed request includes command identity, run/task binding, requested source kind, evidence kind, canonical relative path and source digest; it accepts no root or locator. The daemon derives the effective source: a requested `run-file` is admitted only below a dedicated strict-descendant run root; for root `.` it becomes an authority-proved `project-file` or is rejected. No active `run-file` registration may resolve to `.`. Base `fabric.v1.artifact.publish` registers `run-file` only beneath a strict-descendant current run root, or an authority-proved `project-file` when the current run root is the project root. The fixed Git-read service alone registers `git-private-diff`; receipt export and result completion use their existing daemon/agent owners. Exact identity replay returns one evidence ID. Changed scope, source, path, digest, publisher or kind conflicts.

A bound intake, gate, decision or acceptance may reference only a current registration from its exact project, session and run. When an intake enters `accepted`, that closed revision adds one `acceptedScopeRef` which must occur exactly once in its registered `artifactRefs`; every other intake state forbids it. Project/session projection derives accepted scope only from this persisted binding. A prose path, launch ref or unbound intake ref is not reviewable evidence.

The closed request is:

```yaml
credential: exact-operator-read-capability
projectId: exact-project
projectSessionId: optional-exact-session
evidenceId: exact-artifact-id
expectedEvidenceRevision: positive-integer
artifactRef:
  path: canonical-relative-path
  digest: sha256:64-lowercase-hex
cursor: null-or-daemon-issued-bounded-cursor
maximumBytes: integer-4-through-131072
maximumLines: positive-integer-at-most-2000
```

The request accepts no caller-selected filesystem root, run directory, media type, transform, command, executable or arbitrary path. `artifactRef` is an exact cross-check against the canonical evidence row; it is never the resolver authority. The current project, optional session, evidence registration revision, artifact ID, path and source digest must all still agree. The opaque cursor carries no authority; it is integrity-bound to that tuple, the safety algorithm version and the next rendered UTF-8 boundary.

The result repeats the exact `artifactRef` and is exactly one closed variant:

```yaml
available: false
artifactRef: {path: canonical-relative-path, digest: sha256:source-digest}
reason: not-found|forbidden|unsupported-media|unsafe-content|stale|oversized
```

or:

```yaml
available: true
artifactRef: {path: canonical-relative-path, digest: sha256:source-digest}
mediaType: text/markdown|application/json|text/x-diff|text/plain
content: bounded-inert-utf8
totalBytes: non-negative-source-byte-count
totalLines: non-negative-source-line-count
renderedTotalBytes: non-negative-rendered-byte-count
renderedTotalLines: non-negative-rendered-line-count
pageIndex: non-negative-integer
lineFragment: whole|start|middle|end
pageContentDigest: sha256:returned-page-digest
renderedArtifactDigest: sha256:complete-rendered-artifact-digest
nextCursor: null-or-daemon-issued-bounded-cursor
transformation: none|terminal-neutralised|capability-redacted|credential-redacted|combined
terminalNeutralised: true
capabilityValuesRedacted: true
credentialValuesRedacted: true
```

`totalBytes`/`totalLines` describe the verified source and `renderedTotalBytes`/`renderedTotalLines` describe the complete inert rendering. An empty value has zero lines; every non-empty value has one plus its LF count. Each page obeys both requested bounds after whole-artifact safety transformation. Page boundaries are monotonic, non-overlapping valid UTF-8 boundaries, prefer the final complete line inside the byte limit, and may split a longer line only at a code-point boundary. The result therefore also carries `lineFragment: whole|start|middle|end`; a fragment counts as one returned page line without changing `renderedTotalLines`. A boundary cannot leave a partial terminal escape or credential token. `nextCursor: null` proves the final page. Every page repeats the same source and complete rendered-artifact digests; the client verifies each `pageContentDigest` and may stream all pages into `renderedArtifactDigest`. For a single-page `none` transformation, both rendered and page digests equal the source `artifactRef.digest`. Otherwise the source digest remains immutable provenance, not a claim about displayed bytes. An absent, repeated, skipped, reordered or cross-artifact cursor fails closed.

The daemon resolves only through the canonical evidence registration and its fixed source owner. `project-file` resolves beneath the canonical trusted project root; `run-file` resolves beneath the exact run's dedicated artifact directory; `git-private-diff` resolves by digest beneath the daemon's canonical private Git-diff root. It opens one regular file without following links, proves canonical containment, rejects any symlink or multiple-link alias, records and rechecks device, inode, size and modification time, bounds the source to 1 MiB and verifies its raw SHA-256 before decoding on every page. A missing registration/source is `not-found`; a project/session/permission mismatch is `forbidden`; a changed registration, row, ref, cursor or file is `stale`; source or inert rendering beyond its hard ceiling is `oversized`; binary, invalid UTF-8/JSON or unsupported media is `unsupported-media`; and content that cannot be safely classified/redacted is `unsafe-content`.

Media classification is daemon-owned and extension allow-listed. `.md` and `.markdown` map to `text/markdown`; `.json` maps to `application/json` only after bounded parsing succeeds; `.diff` and `.patch` map to `text/x-diff`; `.txt`, `.log`, `.yaml`, `.yml`, `.toml`, `.ini` and extensionless UTF-8 map to `text/plain`. Content sniffing cannot widen this list. Before projection the daemon neutralises terminal controls, redacts every registry-owned bearer prefix including `afb_`, `afc_` and `afop_`, and applies the closed daemon-owned credential classifier defined by the operational-hardening contract. It then freshly reauthenticates and rechecks the evidence/source tuple and file identity. Mixed-revision or pre-change content is never returned as current.

Added requirements are:

- **FR-044:** Artifact content shall be read only through the exact current
  operator/project/session/evidence/ref/cursor tuple and daemon-owned evidence   registration plus fixed source owner. Caller path or media claims confer no   filesystem authority.
- **NFR-027:** Artifact reads shall be no-follow, race-rechecked, source-bounded,
  cursor-paged, UTF-8/media allow-listed, terminal-neutralised and credential-   redacted before projection, with separate source, complete-rendering and page   digests.
- **FR-045:** Every Console evidence ref and accepted scope shall bind one
  current, exactly scoped evidence registration before projection or decision;   no prose/path-only reference or unregistered private artifact is reviewable.

Acceptance additionally requires:

- **AC-037:** closed-codec fixtures reject missing, extra, cross-variant or
  incorrectly typed fields and limits outside `4..131072` bytes or `1..2000`   lines. Deterministic reads cover Markdown, JSON, diff and plain text;   untransformed and transformed whole/page digests; empty, exact-bound and   multi-page files; monotonic continuation, restart and duplicate/skip/reorder/   cross-artifact cursor negatives; multibyte/line boundaries; every wrong   project/session/evidence revision/ref/digest; absolute, traversal, symlink and   hard-link aliases; file/registration change during read; binary, invalid   UTF-8/JSON, unsupported/unsafe media and oversize source/rendering; terminal,   bearer and unrelated-credential canaries; absent/disabled feature; and exact   registered scoping intake through Evidence row, detail, all content pages and   accepted-scope projection. Baseline publication, result artifacts, receipts   and private Git diffs register idempotently; every producer's root-equal   `run-file` request reclassifies only with exact project-file authority or   rejects, and direct SQL cannot retain an active root-equal `run-file`. An   unregistered or cross-scope ref cannot enter a bound intake/gate/acceptance.   Every rejected or unavailable   read performs no mutation and creates no liveness, membership,   acknowledgement or custody state.

### Negotiated native-notification projection shape

the Console contract requires native delivery state on Attention without making a Console and daemon built from different compatible revisions reject each other's otherwise valid projection frames. Negotiated result-shape feature `native-notification-projection.v1` therefore extends the existing `fabric.v1.operator-projection.snapshot` and `fabric.v1.operator-projection.page` Attention variant plus `fabric.v1.operator-projection.view-page` Attention result. It grants no operation and cannot widen operator authority.

The summary is part of the already-authorised exact project/session Attention read. It exposes only the fixed `native-desktop` integration identifier and bounded delivery/availability state; it carries no destination, credential, actionable link or unrelated integration data. Negotiation never changes an authorisation decision. A future need to hide or add summary data requires a new closed result-shape feature rather than omission or field-level redaction inside v1. The v1 summary shape is otherwise frozen.

Without that negotiated feature, all three operations retain their pre-extension closed shapes and omit `nativeNotification`. With the feature, every Attention item in a snapshot and every Attention view-row summary requires exactly one closed `nativeNotification` value. Other views never carry it. The value binds the exact Attention item revision to target `native-desktop`, delivery journal state and revision/generation, integration availability and observation time; its Console label is only `available`, `unavailable` or `stale`.

The server derives the result shape from the authenticated connection's negotiated current feature set and omits the extension when the independently optional native-notification feature is unavailable. The Console then renders `feature-unavailable` without implying a delivery-journal observation. When the feature is negotiated, a missing or malformed extension fails closed as a protocol result error; an extension received without negotiation also fails. Every Attention-typed node reachable from one result root, including conflict candidates, uses the same mode. Mixed presence invalidates the whole result. The client consumes no partial projection and the operator receives a typed `protocol-incompatible` connection failure with the rejected operation and closed reason. There is one connection attempt and no alternate-profile retry or result-shape translation.

For future additive features, the amended daemon accepts bounded, unique, well-formed feature names in initialise requests. An unknown required name produces `required-features-unavailable`; an unknown optional name is ignored. Names use the closed lowercase dotted-version grammar, are at most 64 bytes and the required and optional arrays contain at most 64 names combined. The exact ASCII grammar is `^[a-z][a-z0-9]*(?:-[a-z0-9]+)*(?:\.[a-z][a-z0-9]*(?:-[a-z0-9]+)*)*\.v[1-9][0-9]*$`. No exact name may repeat within or across the two arrays. Initialise results still carry only features known to and negotiated by both peers. This forward-tolerance does not grant an unknown operation or relax result validation. A count, duplicate, ASCII-byte-length or grammar violation rejects the entire initialise request as `PROTOCOL_INVALID` before required/optional classification. Parsing uses exact ASCII byte equality without truncation, case folding or Unicode normalisation.

The Console's feature-unavailable presentation has no timestamp, count, empty journal state or synthetic zero. Notification aggregates exclude that branch rather than treating unavailable as zero; Markdown/JSON exports preserve the explicit unknown state.

Every insert, update or delete that can change the projected native delivery summary advances `daemon_global_state.revision` in the same SQLite transaction. This includes `notification_deliveries` and every stored `integration_availability` mutation. Availability writers treat an unchanged state and discovered contract as a complete no-op, retaining the prior `checked_at`; a material change stores its new observation time and advances the revision. The next snapshot/page therefore cannot reuse a revision or state digest after a pending, claimed, sent, failed, deduplicated or ambiguous transition. An eventless resnapshot that otherwise returns the same stable rows preserves selection, focus, scroll, draft and pending command state as required by the Console contract. Load evaluation bounds refresh work under delivery churn; correctness never depends on coalescing multiple row triggers into one revision increment.

The deterministic churn gate starts from 1,000 open Attention rows and one attached Console, applies 2,000 delivery transitions in 200 transactions of 10 across a simulated 10-second interval, and drives exactly twenty 500 ms poll ticks. After warm-up it permits at most twenty completed resnapshots, zero overlapping refreshes, 250 ms p95 refresh latency, five seconds total wall and process CPU time, and 32 MiB additional heap. It records host and Node version.

Added requirements are:

- **FR-046:** Native notification delivery fields shall appear only under the
  exact negotiated result-shape feature and shall be required there, while the   unextended projection shapes remain wire-compatible and closed.
- **NFR-028:** Every database transition visible in a notification delivery
  summary shall atomically invalidate the daemon projection revision.
- **NFR-029:** Protocol initialise shall ignore bounded well-formed unknown
  optional features, report unknown required features as unavailable and never   derive an operation grant from an unknown name.
- **NFR-030:** Result-shape validation shall cover every Attention node at one
  mandatory send/receive boundary, reject mixed or wrong negotiated presence as   a whole-result incompatibility and never expose partial data.

Acceptance additionally requires:

- **AC-038:** current-baseline tests exercise only the current client/daemon
  schema set: unnegotiated base success, negotiated exact-extension success,   negotiated-missing-field, unnegotiated-extra-field and malformed-summary   frames for snapshot, Attention projection-page and view-page. There is no   downgrade, vintage-daemon fixture, retry-as-older-schema or compatibility   export. Unknown bounded optional feature names are ignored only at   initialise; unknown required names are unavailable. Every unknown enum value,   schema version other than the exact current constant, mixed extension   presence, duplicate name, 65-combined-entry, 64-plus-64 entry, cross-array   duplicate, over-64-byte or non-ASCII name fails closed before projection or   mutation. Current-schema migration tests prove insert/update/delete delivery   changes advance global revision exactly as defined, force resnapshot for a   stale page, and cause a polling Console to observe pending-to-terminal state   without an unrelated Fabric event while resize/resnapshot preserves stable UI   state and bounded load. No notification state change   acknowledges, approves, focuses or otherwise mutates its Attention item.

### Exact Console read identity completion

The Console must not guess a preparation identifier, infer a provider route from action-local state or treat run-local task, agent or evidence identifiers as project-global. Three extension operations complete the existing read surface:

- `fabric.v1.review-target-preparation.current.read`;
- `fabric.v1.provider-route.read`; and
- `fabric.v1.provider-route.list`.

They are operator-only reads behind negotiated `console-read-identity.v1`. The current Console lists that feature in `requiredFeatures`; initialize fails closed rather than offering an identity- guessing fallback when it is unavailable. The feature owns exactly the three operations above, and current-Console initialize is incompatible unless its intersected `allowedOperations` contains all three. An active operator seat may invoke them only when its project-bound capability explicitly includes the exact operation. The grant is `read`, never `decide`, `steer` or chair authority. They grant no mutation, routing, review, lifecycle or topology authority. Generated RPC, schema and operator-descriptor registries include them exactly once; the agent MCP set does not. Unknown fields and enum values fail closed.

The current-preparation locator accepts only exact run scope:

~~~yaml
reviewTargetPreparationCurrentReadRequestV1:
  schemaVersion: 1
  credential: exact-operator-read-capability
  projectId: exact-project
  projectSessionId: exact-session
  coordinationRunId: exact-run

reviewTargetPreparationCurrentReadV1:
  commonRequired:
    - schemaVersion
    - projectId
    - projectSessionId
    - coordinationRunId
    - status
    - currentPreparationGeneration
    - preparation
  oneOf:
    - schemaVersion: 1
      status: unavailable
      currentPreparationGeneration: 0
      preparation: null
    - schemaVersion: 1
      status: current
      currentPreparationGeneration: positive-integer
      preparation: reviewTargetPreparationReadV1
~~~

Both arms equality-copy the requested project/session/run. `current` means the greatest durably allocated preparation generation for that exact run, whether nonterminal or terminal. The nested value is byte-shape- identical to the existing `reviewTargetPreparationReadV1`; the locator is not a second preparation codec. The row's preparation generation and wrapper generation equal the high water; the nested accepted receipt reproduces the requested project-session/run and its row's preparation ID. A missing high- water row, or a zero high water with no preparation row, is `unavailable` when the exact run exists and has never allocated a preparation. A missing or zero high water while any preparation row exists for that run is integrity failure, as is a preparation/target/bundle high-water triple that is NULL, negative or unequal. A positive equal triple must equal the run's greatest stored preparation generation, have exactly one matching row, and equal that row's reserved target and bundle generations. All high waters and greatest-row generation are compared in the same read transaction. Missing run, wrong project/session/run pairing and denied authority use the existing closed `reviewTargetPreparationReadErrorV1` codec and are not disguised as unavailable. Its existing `REVIEW_TARGET_PREPARATION_NOT_FOUND`, `AUTHORITY_DENIED`, `SCOPE_MISMATCH` and `INTEGRITY_FAILURE` arms apply unchanged.

the deployed-route contract's full closed route variant is canonically `providerRouteV1` and is implemented by the existing `PROVIDER_ROUTE_V1_CODEC`:

~~~yaml
providerRouteV1:
  actionRef: ProviderActionRefV1
  taskId: exact-task
  route: providerRouteProjectionV1
  admission: deployedRouteAdmissionV1
  capabilitySummary: capabilitySnapshotSummaryV1
  latestDispatch: deployedRouteDispatchV1 | null
  observation: deployedRouteObservationV1 | null
~~~

It is the same full shape already exposed by provider-action read and receipt v2, not the thinner `providerRouteProjectionV1`/`localProviderRoute` member and not a new codec. Provider route reads use the daemon-global action pair and nest that exact codec only in the present arm:

~~~yaml
providerRouteReadRequestV1:
  schemaVersion: 1
  credential: exact-operator-read-capability
  projectId: exact-project
  projectSessionId: exact-session
  coordinationRunId: exact-run
  actionRef:
    adapterId: exact-adapter
    actionId: exact-action

providerRouteReadV1:
  commonRequired:
    - schemaVersion
    - projectId
    - projectSessionId
    - coordinationRunId
    - actionRef
    - taskId
    - routeOrdinal
    - routeRevision
    - createdAt
    - readAt
    - routeState
    - freshness
    - route
    - routeRecoveryEvidenceDigest
  oneOf:
    - schemaVersion: 1
      routeState: present
      freshness: current | stale
      route: providerRouteV1
      routeRecoveryEvidenceDigest: null
    - schemaVersion: 1
      routeState: missing | integrity-failed
      freshness: null
      route: null
      routeRecoveryEvidenceDigest: sha256-prefixed-safe-digest

providerRouteListRequestV1:
  schemaVersion: 1
  credential: exact-operator-read-capability
  projectId: exact-project
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-run-task | null
  targetGeneration: positive-integer | null
  slot: native | other-primary | cursor-grok | agy-gemini | null
  watermarkOrdinal: nonnegative-integer | null
  pageSize: positive-integer-at-most-8
  cursor: opaque-scope-filter-watermark-bound-cursor-at-most-1024-bytes | null

providerRouteListV1:
  schemaVersion: 1
  status: page
  projectId: exact-project
  projectSessionId: exact-session
  coordinationRunId: exact-run
  watermarkOrdinal: nonnegative-integer
  readAt: timestamp
  routes: ordered-providerRouteReadV1-array
  nextCursor: opaque-scope-filter-watermark-bound-cursor-at-most-1024-bytes | null

providerRouteReadErrorV1:
  commonRequired: [schemaVersion, code, evidenceDigest]
  oneOf:
    - schemaVersion: 1
      code: NOT_FOUND | AUTHORITY_DENIED | SCOPE_MISMATCH | STALE_CURSOR
      evidenceDigest: null
    - schemaVersion: 1
      code: INTEGRITY_FAILURE
      evidenceDigest: sha256-prefixed-safe-digest
~~~

The common fields are scalar fields with the indicated names: project/session/ run and action/task identity, positive `routeOrdinal`, positive `routeRevision`, immutable action-list admission `createdAt` and transaction- authored `readAt`. The present arm's nested action/task identity must equal those common fields. The wrapper labels read-time freshness but never changes or duplicates the nested route. `current` requires the admission capability point and, when present, the latest-dispatch capability point to be unexpired at `readAt`; otherwise it is `stale`. Missing/integrity-failed are legitimate recovery-owned states, never read corruption or current routes. Their safe evidence digest is required and freshness is inapplicable. Freshness is operator information only and cannot rewrite historical admission, dispatch, observation or certification evidence. Both operations return the one closed `providerRouteReadErrorV1` on operation failure.

Read requires exact equality among request scope, the daemon-global provider action pair and task run. An exact action pair with null route ordinal is not a route-list member and returns `NOT_FOUND`; its legitimate lack of route/ recovery is not corruption. The present arm additionally equality-binds the provider-action route; missing/integrity-failed instead binds the exact live route-recovery evidence for that pair and task/run. `GenericProviderRouteRecoveryService` is the sole owner for an otherwise-generic task-bound answer-bearing action whose route is missing or integrity-failed; it supplies that exact live evidence but gains no dispatch, reroute or certifying authority. the certifying route-integrity recovery contract's `ProviderRouteIntegrityRecoveryService` remains the sole owner for every certifying action, while lifecycle and launch custody remain with their existing dedicated owners. List enumerates every admitted task-bound answer-bearing action, including recovery-owned missing/ integrity-failed states. Each page scans at most 256 consecutive unfiltered members strictly after the cursor's last-scanned tuple and at or below the watermark. Every scanned member first classifies through one trustworthy present-route or exact recovery-owned missing/integrity arm. An orphaned, crossed or unparseable member fails the whole operation with `INTEGRITY_FAILURE`; filters cannot hide it. List filters are then conjunctive and nullable; a slot or target filter selects only certifying actions whose immutable route row or recovery-custody tuple proves that field. Missing/integrity-failed recovery cannot borrow it from untrusted route bytes.

The first page supplies null watermark and cursor, then captures `provider_route_list_high_water.route_ordinal`; zero is the empty watermark. The action-admission transaction keeps that high water equal to the run's greatest allocated route ordinal. A missing high-water row while any run action has a nonnull route ordinal, or a stored high water that differs from the greatest allocated ordinal (zero when there is no such action), is `INTEGRITY_FAILURE`, never an empty or truncated page. Greatest ordinal is an indexed last-key lookup, not a whole-set count. A missing high-water row when the run has no nonnull route ordinal is exactly watermark zero. Before applying filters, every bounded scan proves contiguity incrementally. A null cursor expects ordinal one; otherwise the authenticated last-scanned ordinal expects exactly its successor. Every scanned row must equal the expected ordinal, and absence of that successor while it is at most the watermark is `INTEGRITY_FAILURE`. The cursor is null immediately for watermark zero; otherwise it becomes null only after last-scanned equals the watermark. The unique/positive/immutable membership constraints complete that proof without a whole-run count. Every later page supplies that exact watermark and the prior opaque cursor. Rows with ordinals above it are excluded, so continuous route/dispatch/observation activity cannot starve pagination. Rows are ordered by immutable `(routeOrdinal,adapterId,actionId)` and every returned ordinal is at most the watermark. State, child data, `routeRevision` and freshness remain truthful at the page's common `readAt`; the list does not falsely claim a cross-page atomic cut. `pageSize` is at most 8. Generated schema bounds prove 8 maximal routes, the actual request ID, envelope and maximal next cursor fit the negotiated 1,048,576-byte frame. The bound uses the exact JSON encoder and worst legal UTF-8-to-JSON expansion, including six wire bytes for an escapable one-byte control character, maximal numbers/timestamps and every key/delimiter/final LF; example values are not a bound. Page construction therefore never discovers a matching row it must withhold for size. Scanning stops before the next member once the requested `pageSize` matches (at most 8) are collected, or after 256 classified members. A nonnull cursor may therefore accompany an empty filtered page and is the only progress/completion authority; it advances across every classified nonmatch and nulls only after the watermark is exhausted (immediately for watermark zero). Every ordinal is classified at most once in one traversal. The current Console requires the 1 MiB maximum during initialize. The opaque cursor binds operation, principal/capability scope, project/session/run, all filters, watermark and the last-scanned ordering tuple. Cursor substitution, replay under another scope, changed filters, null/non-null watermark-cursor mismatch or a non-progressing cursor fails closed. Every malformed, forged, substituted, mismatched or non-progressing cursor/ watermark binding returns `STALE_CURSOR`; true authority, scope and stored-row integrity failures retain their distinct codes.

The operator projection also closes multi-run identity without deleting project/session evidence. Every Work, Agents and Activity view item, summary, detail reference and returned detail carries nonnull `projectId`, `projectSessionId` and `coordinationRunId` in addition to its local identifier. Activity includes `eventId` and its source revision; a message activity's `messageBodyRef` carries the identical project/session/run plus message ID and revision. Task, agent, event and message identity is interpreted only inside that full tuple. Work `parentTaskId`/`ownerAgentId`, Agent `stableTaskId` and every Activity task/agent actor reference inherit the enclosing run tuple; no embedded local ID is a project- or session-wide lookup key.

Every Evidence item, summary, detail reference and detail instead carries one closed `evidenceScope` plus `evidenceId`:

~~~yaml
evidenceScope:
  oneOf:
    - kind: project
      projectId: exact-project
    - kind: session
      projectId: exact-project
      projectSessionId: exact-session
    - kind: run
      projectId: exact-project
      projectSessionId: exact-session
      coordinationRunId: exact-run
~~~

The daemon derives the arm from the stored project/session/run registration: nonnull run requires the exact run arm, null run plus nonnull session requires session, and both null require project. Thus project files and private Git diffs remain Evidence rows while run evidence cannot cross-bind another run. The Evidence `taskId` must be null in project/session arms and, when nonnull, is interpreted only in the exact run arm. The detail request's outer project/session scope and its detail ref must equality- bind wherever that arm contains a session. A stale or crossed tuple returns resnapshot/scope failure, never a row found by local identifier alone.

The stable item-ID prefixes are exactly the view names `work`, `agents`, `activity` and `evidence`. Work/Agents/Activity use `<view>:<base64url-no-padding(SHA-256(UTF-8(JCS([projectId, projectSessionId,coordinationRunId,localId]))))>`; Evidence uses `evidence:<base64url-no-padding(SHA-256(UTF-8(JCS([evidenceScope, evidenceId]))))>`. These 43-character digests plus the pinned prefix fit the existing 128-byte identifier codec even when every tuple member is maximal. Summaries, detail refs, detail payloads, page rows and selection/hit-region state use the same tuple. There is no decoder for the previous local-ID-only shape and no Console-side fallback join.

Work pages use the exact total source order `(projectId,projectSessionId,coordinationRunId,taskId)` and Agents pages use `(projectId,projectSessionId,coordinationRunId,agentId)`. The existing numeric page cursor is a position in that pinned snapshot order, never an order by local ID alone. Activity pages preserve reverse source-revision order and use `(sourceRevision DESC,projectId,projectSessionId,coordinationRunId,eventId)` as the exact total tie-break order. Reused local IDs therefore cannot gap, repeat or exchange position across pages.

Conformance tests start with wrong-reason RED fixtures for two sessions/runs that deliberately reuse the same task and agent IDs. Distinct Activity/message rows in both runs prove summaries, detail refs/details and message-body reads remain inside the exact run tuple. Evidence fixtures retain the globally unique artifact ID but attempt a scope-crossed detail read and prove project/session/run arms remain distinct. They cover absent, active and terminal current preparations; exact pair route reads; generic and certifying route list filters; stable multi-page watermarks; expired capability freshness; crossed session/run/action pairs; cursor and filter substitution; descriptor/schema parity; missing/integrity-failed route arms; and proof that every present route parses through `providerRouteV1` rather than a copy. Boundary oracles cover maximal tuple identifiers, stable digest IDs, a maximal single route RPC frame and an 8-maximal-route page below the negotiated 1 MiB limit, plus an interior ordinal-gap fixture on a later page that must fail before nullable filters are applied. A filtered orphan fixture fails integrity rather than returning an empty page, and multi-page Work/Agents/Activity fixtures reuse local IDs across runs without gaps or replay. Frame oracles use the maximal 1,024-byte route-list cursor in both request and response positions. A selective-filter load oracle traverses empty progress pages and proves no ordinal is classified more than once while each page scans at most 256 members. The zero-watermark fixture returns an empty page with null cursor immediately. Initialize fixtures reject a missing feature, any one missing preissued/ intersected operation and a narrowed frame limit; the positive arm contains all three operations, and a wrong-reason negative proves initialize never expands the credential.

Added requirements are:

- **FR-089:** The Console read-identity feature shall negotiate exactly the
  current-preparation, provider-route read and provider-route list operations as   one all-or-nothing operator-read surface.
- **FR-090:** Current-preparation lookup shall use exact project/session/run
  scope and return only the greatest durably allocated preparation generation   or the closed unavailable arm.
- **FR-091:** Provider-route read shall use the daemon-global adapter/action pair
  and return the existing full `providerRouteV1` only in the present arm.
- **FR-092:** Missing and integrity-failed generic routes shall expose only exact
  live recovery evidence owned by `GenericProviderRouteRecoveryService`.
- **FR-093:** Provider-route list shall classify contiguous immutable ordinals
  through a fixed watermark and authenticated progress cursor before applying   nullable filters.
- **FR-094:** Work, Agents and Activity items, references and details shall carry
  exact project/session/run identity and use it in ordering and lookup.
- **FR-095:** Evidence items, references and details shall carry one closed
  project, session or run scope and use that scope in identity and lookup.
- **NFR-039:** Console reads shall fail closed on missing feature, operation,
  authority, scope or integrity and shall never guess identity or grant mutation.
- **NFR-040:** Route pages shall return at most eight matches, scan at most 256
  consecutive members per page and fit the negotiated 1 MiB frame by construction.
- **NFR-041:** Route-list cursors shall bind operation, authority scope, filters,
  watermark and progress while each returned row remains truthful at its page's   `readAt`.
- **NFR-042:** No Console decoder, fallback join or pagination order shall use a
  local task, agent, event, message or evidence identifier as global identity.

Acceptance additionally requires:

- **AC-064:** Initialize fixtures reject a missing feature, each missing
  operation and a narrowed frame limit without expanding the credential.
- **AC-065:** Current-preparation fixtures cover absent, active and terminal
  generations plus crossed scope, high-water and stored-row integrity failures.
- **AC-066:** Provider-route read fixtures cover exact pairs, present/stale
  routes and generic versus certifying missing/integrity recovery ownership.
- **AC-067:** Route-list fixtures cover stable multi-page watermarks, ordinal
  gaps, filtered corruption, cursor/filter substitution, empty progress pages   and zero watermark.
- **AC-068:** Work, Agents and Activity fixtures reuse local identifiers across
  runs and prove summaries, details, message reads and pagination stay in the   exact tuple.
- **AC-069:** Evidence fixtures cover all three scope arms and reject crossed
  detail scope while retaining project/session evidence.
- **AC-070:** Boundary fixtures prove stable maximal identifiers, maximal
  request/cursor frames, eight maximal routes below 1 MiB and bounded selective   traversal.
