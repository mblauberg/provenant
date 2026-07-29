# Agent Fabric provider actions and adapters

## Provider adapter contract

Each adapter runs behind a versioned process boundary in the first release. Adapter failure must not crash the core.

```yaml
adapter_operations:
  registration_required:
    - capabilities
    - status
    - release
    - lookup_action
    - cancel_action
  managed_session_required:
    - spawn
    - send_turn
    - interrupt
    - resume_reference
    - dispatch
  attached_interactive_required:
    - attach
    - status
    - wakeup
    - resume_reference
  optional:
    - steer
    - follow_up
    - compact
    - fork
    - native_subagents
    - enforced_read_only
    - usage_and_cost
    - shared_session_ui
    - verified_boundary_inject
    - compact_in_place
```

Every side-effecting adapter command uses a fabric-generated `action_id`, exact `adapter_id`, lease generation and immutable payload hash. The daemon-global provider action identity is the pair `(adapter_id, action_id)`; an action ID may repeat on another adapter but the same pair may not identify another run or input. The adapter durably records `prepared`, `dispatched`, `accepted`, `terminal` or `ambiguous`. The core calls `lookup_action` after ambiguity and never automatically replays a side-effecting command unless downstream idempotency for that action ID is proven. Otherwise the task is quarantined for explicit recovery. The action record commits before dispatch; its terminal result commits before message acknowledgement.

Answer-bearing Agy and Cursor CLIs use the canonical 30-minute provider-turn
deadline. The outer adapter response envelope derives from that deadline plus
a fixed five-second settlement and cleanup grace, so the inner boundary can
persist its result before an unresponsive adapter transport is killed. This
grace is not an operator setting. Control requests retain their shorter
deadline, while tests may inject a shorter positive provider-turn deadline.
Expiry after dispatch is ambiguous: the process is cleaned up, the journal
requires reconciliation and Fabric does not automatically retry or substitute
another provider.

`lookup_action` applies to every adapter operation with external effects, including provider-mutating release or wake-up implementations. An adapter may declare an operation core-only and idempotent only when it does not mutate the provider session or external state; that declaration is contract-tested.

The scheduler does not assign a role whose control, delivery or recovery requirements exceed the session's advertised capabilities.

`steer` and `follow_up` execute under the active turn lease held by the turn initiator; they do not acquire a second generation. Only a new `send_turn` acquires a new adapter-turn lease. Interactive targets return `capability_unavailable` for turn-control operations and use mailbox plus wake-up instead.

Planned adapters:

| Adapter | Intended role | Notes |
|---|---|---|
| Claude Agent SDK | Claude primary, leader or worker | Persistent headless sessions; interactive TUI remains a separate profile |
| Codex app-server | Codex primary, leader or worker | Thread and turn lifecycle; contract-test generated protocol schemas |
| Pi SDK or RPC | Generic API and open-provider workers | Model-neutral worker runtime; not the authority store |
| Agy | Gemini or Antigravity access | Adapter only; no separate provider skill |
| Cursor | Composer and Grok only | Model allow-list remains routing policy |
| Kiro or ACP | Open-model runtime | Capability-discovered, optional and non-blocking |
| Herdr | Pane placement, observation and wake-ups | Never authoritative transport |

Unsupported optional capabilities return a typed `capability_unavailable` result. The router may choose a compatible substitute only when the existing model-routing policy permits substitution and records it.

#### Structural routing and admission

The shared model-route.v1 codec contains only closed structural routing data:

~~~yaml
routeRequest:
  schemaVersion: 1
  adapterAlias: configured-nonempty-alias
  modelAlias: configured-nonempty-alias
  explicitModel: null-or-structurally-valid-model
  role: configured-route-role
  leadFamily: canonical-nonempty-model-family
  requireDistinct: true-or-false
  providerEffort: null-or-structurally-valid-effort
~~~

It rejects unknown fields and malformed values but performs no database, artifact, target, effort-applicability, adapter-activation, model-policy or currency read. Null effort requests the configured route default. The Python router and TypeScript daemon validate the same checked-in schema.

A certifying provider-action.dispatch additionally carries this separate closed binding; non-review work carries null:

~~~yaml
certifyingReview:
  oneOf:
    - null
    - targetGeneration: exact-current-target
      slot: native-or-other-primary-or-cursor-grok-or-agy-gemini
      expectedSlotHeadGeneration: nonnegative-CAS-generation
      expectedChairBindingGeneration: positive-active-binding-generation
      expectedOpenFindingSetDigest: sha256-prefixed-digest
      findingWindowMode: normal-or-resolution-only
      findingCapacityReservationDigest: sha256-prefixed-digest
~~~

Every provider-action request names `adapterId` and `actionId`; their pair is the daemon-global `ProviderActionRefV1`. The requested adapter must equal the profile slot adapter and the resolver's adapter result. A route may select family/model/effort within policy but cannot silently change the provider action's adapter. Existing durable command/action replay and immutable input- digest comparison run first. For a new certifying action, the authenticated caller must hold the current chair lease/generation and equal the target's active chair binding. Ordinary answer-bearing work retains its existing task authority.

After immutable command/pair replay classification and before invoking the router, review dispatch validates the exact open-finding set and commits either a normal 32-finding capacity reservation or an admitted zero-new-finding resolution-only reservation. Capacity failure returns the typed blocker with no router, provider, action or budget effect.

The trusted resolver is side-effect-free, reads only pinned routing inputs, writes nothing, performs no provider/network call and emits bounded output. One 5,000 ms deadline covers spawn, parse and validation; timeout/overflow kills the process group with TERM then KILL after 250 ms. It leaves no action, route, command result or budget reservation.

Pre-router durable preflight and in-process single-flight use only the global `(adapterId, actionId)` pair. The owner digest hashes RFC 8785 JCS of the run, authenticated actor/principal identity and complete closed dispatch input, including route request, task, authority, certifying binding and provider payload; command ID is handled by the outer replay journal and is not omitted from any other semantic field. An exact concurrent retry joins that flight. A different owner digest waits for the owner and then returns ACTION_INPUT_CONFLICT before any router call, even after a pre-commit owner failure. Cross-run reuse of the same pair therefore invokes the router at most once and conflicts pre-router; the same `actionId` on a different adapter is legal. A later exact same-owner retry may safely rerun only the side-effect-free router after a pre-commit crash. Durable action, route, recovery and adapter journals use the same pair; no action-ID-only lookup exists.

After a successful router result, one admission transaction rechecks task and authority, complete budget, current target/chair/profile/head generations, delivery/source/artifact currency, certifying-review-packet-only.v1 capability/contract, route aliases, effort applicability, resolved adapter/family/model/tagged effort and lead-family/distinctness. The resolved adapter must equal both the requested adapter and resolved slot snapshot; the complete route must equal that snapshot. These are database admission rules, not codec rules. It then commits action, canonical request/receipt and digests, target/bundle/coverage/profile/head/chair snapshots, final-prompt digest, complete budget reservation and command receipt before provider I/O.

That transaction also CAS-increments the slot head's attempt generation and reserves the exact target, slot and evidence-head generation for this action. A partial unique constraint permits one nonterminal attempt for that tuple. Two concurrent actions from one head cannot both commit or reach the provider; a later attempt starts only from the returned current attempt/head state.

Dispatch and read expose the canonical action pair and one closed providerRouteProjectionV1 containing those exact immutable values. No later router/configuration, chair, target or registry change rewrites it.

#### Terminal review results and provider failures

The provider answer must be exact valid UTF-8 and no larger than 65,536 bytes. Its strict review-result.v1 object is:

~~~yaml
reviewResultV1:
  schemaVersion: 1
  targetGeneration: exact-action-target
  coverageDigest: exact-action-coverage-digest
  findingWindowMode: normal-or-resolution-only
  verdict: CLEAN-or-FINDINGS
  resolvedFindingDigests: ordered-unique-subset-of-prior-open-findings
  findings:
    - findingId: unique-safe-id
      severity: P0-or-P1-or-P2
      summary: bounded-safe-text
      evidence: bounded-safe-text
      repairKind: repository-source-or-registered-evidence-or-mixed
      evidenceRefs: ordered-subset-of-bundle-required-evidence-refs
~~~

CLEAN requires no new findings and resolves every prior open finding. FINDINGS requires one through 32 new findings and may resolve any subset of prior open findings. `repository-source` requires empty evidence refs; `registered-evidence` and `mixed` require a nonempty set that the daemon resolves to exact origin registration/revision/content tuples. A resolution- only action requires CLEAN, zero findings and at most its 32 admitted prior digests; it is always noncertifying. Unknown/duplicate fields or IDs, inconsistent target or coverage, malformed JSON, controls, credentials, capabilities or unsafe text produce UNUSABLE. The reject-only safety classifier and immutable secret-set identity remain as defined in the bounded artifact-read contract. Raw answer and raw adapter result stay daemon-private.

A provider may terminate with a proved failure and no answer. Its code is exactly one of `max-turns-exhausted`, `provider-rejected`, `terminal-no-answer` or `adapter-terminal-failure`; no adapter-specific or future value enters this arm. This is not effect ambiguity. Fabric commits terminal state, the closed failure code and a digest of bounded normalised private diagnostics; it exposes no raw error or fabricated answer. Every proved-effect terminal kind -- safe answer, unusable answer or provider-terminal-failure -- settles authenticated complete usage exactly. If that usage is absent or partial, the same terminal transaction conservatively consumes the full remaining spendable reservation. In either case it releases terminal concurrency capacity. Proved terminal-no-effect releases the reservation; ambiguity retains it. The action is never redispatched. Ambiguous means only that provider effect/outcome is not proved.

The closed terminalReview arm is exactly one of safe-answer, unusable-answer, provider-terminal-failure, terminal-no-effect, integrity-terminal or retired-unknown. terminal-no-effect proves no provider review effect; integrity-terminal proves terminal effect/settlement but cannot verify the route/bundle chain; retired-unknown is the direct-human retirement of a permanently ambiguous effect after full-ceiling charge. None certifies except a safe-answer whose verdict is CLEAN or FINDINGS. resultDigest is SHA-256 of one exact arm below.

A safe-answer also carries daemon-derived readCoverageDigest and the closed coverage summary: mode manifest-complete-risk-directed, mandatory predicate, per-risk-group total/read/unread counts and unread object-set digests, and byteComplete:false unless every object was fully read. Coverage gaps are therefore explicit and cannot be provider/chair-edited. Certification never claims byte-for-byte review.

Mandatory-read failure is classified before the public terminal arm is committed. A syntactically valid CLEAN with insufficient coverage becomes unusable-answer and reports/accepts no resolution. A safely parsed FINDINGS remains a safe-answer with its safe findings and reported resolution set, but is noncertifying, accepts no resolution and adds every new P0-P2 finding. Raw unsafe or unparseable output is unusable-answer. Provider repetition of a coverage digest is never read proof.

Actual-route proof is classified at the same terminal boundary. A missing or unavailable required provider/family/model observation leaves a safe answer noncertifying with `actual-route-unproved`; any observed route value unequal to admission, or any required value unequal to the resolved profile, uses `actual-route-mismatch`. The observation digest and, only when the required identity is proved, the complete closed `actualRouteIdentityDigest` are stored in the immutable evidence record. Neither blocker discards a safe FINDINGS payload: resolutions remain unaccepted while all adverse findings retain normal custody. Generic, non-review provider actions do not acquire this certification predicate.

~~~yaml
terminalResultIdentityV1:
  commonRequired: [schemaVersion, actionRef, terminalSequence, terminalKind]
  schemaVersion: 1
  actionRef: ProviderActionRefV1
  terminalSequence: positive-run-sequence
  oneOf:
    - terminalKind: safe-answer
      required: [providerAnswerDigest, reviewResultDigest, answerSafety,
        readCoverageDigest, coverageSummaryDigest]
      answerSafety: safe
    - terminalKind: unusable-answer
      required: [providerAnswerDigest, reviewResultDigest, answerSafety,
        readCoverageDigest, coverageSummaryDigest]
      reviewResultDigest: null
      answerSafety: unusable
    - terminalKind: provider-terminal-failure
      required: [providerFailureCode, providerFailureDigest]
      providerFailureCodeEnum: [max-turns-exhausted, provider-rejected,
        terminal-no-answer, adapter-terminal-failure]
    - terminalKind: terminal-no-effect
      required: [noEffectEvidenceDigest]
    - terminalKind: integrity-terminal
      required: [integrityEvidenceDigest]
    - terminalKind: retired-unknown
      required: [retirementEvidenceDigest]
~~~

Every arm rejects every field owned by another arm. `terminalResultDigest` is SHA-256 of RFC 8785 JCS of exactly that arm with no omitted field; the digest is stored outside the object. Usage, cost, timestamps, history, lookup attempts and settlement are excluded. A later usage reconciliation cannot change it. Checked-in golden vectors cover all six arms, both action-ID forms and every failure code; permutation, extra-field and crossed-arm negatives must fail.

The immutable route and terminal read shapes are closed and shared by agent, operator and Console projections. `providerRouteProjectionV1` is byte-shape- identical to receipt `$defs.localProviderRoute`; implementation defines it once and reuses it:

~~~yaml
providerRouteProjectionV1:
  schemaVersion: 1
  routeRequestDigest: sha256-prefixed-digest
  routeReceiptDigest: sha256-prefixed-digest
  adapterId: exact-adapter
  adapterContractDigest: sha256-prefixed-digest
  providerFamily: canonical-family
  resolvedModel: exact-model
  requestedEffort: null-or-exact-effort
  resolvedEffort:
    oneOf:
      - kind: applied
        value: exact-effort
      - kind: inapplicable
  targetGeneration: null-or-positive-generation
  slot: null-or-native-or-other-primary-or-cursor-grok-or-agy-gemini
  reviewedArtifactRef: null-or-exact-artifact-revision
  publicationLineageDigest: null-or-sha256-prefixed-digest
  bundleDigest: null-or-sha256-prefixed-digest
  manifestRootDigest: null-or-sha256-prefixed-digest
  coverageDigest: null-or-sha256-prefixed-digest
  bundleSearchIndexDigest: null-or-sha256-prefixed-digest
  riskReadMapDigest: null-or-sha256-prefixed-digest
  mandatoryReadSetDigest: null-or-sha256-prefixed-digest
  finalPromptDigest: null-or-sha256-prefixed-digest
  targetChair:
    oneOf:
      - null
      - agentId: exact-agent
        bindingGeneration: positive-generation
        principalGeneration: positive-generation
        chairLeaseGeneration: positive-generation
        providerSessionGeneration: positive-generation
        bridgeGeneration: positive-generation
        adapterId: exact-adapter
        adapterContractDigest: sha256-prefixed-digest
        modelFamily: canonical-family
        model: exact-model
        routeReceiptDigest: null-or-sha256-prefixed-digest
  profileDigest: null-or-sha256-prefixed-digest
  slotHeadGeneration: null-or-nonnegative-generation
  attemptGeneration: null-or-positive-generation

providerActionTerminalProjectionV1:
  schemaVersion: 1
  actionRef:
    adapterId: exact-adapter
    actionId: exact-action
  status: prepared-or-dispatched-or-accepted-or-ambiguous-or-terminal
  originalDispatchReceiptDigest: sha256-prefixed-immutable-digest
  routeState: present-or-missing-or-integrity-failed
  route: null-or-providerRouteProjectionV1
  routeRecoveryEvidenceDigest: null-or-sha256-prefixed-digest
  terminalReview:
    oneOf:
      - null
      - kind: safe-answer-or-unusable-answer-or-provider-terminal-failure-or-terminal-no-effect-or-integrity-terminal-or-retired-unknown
        terminalSequence: positive-run-sequence
        terminalResultDigest: sha256-prefixed-digest
        providerAnswerDigest: null-or-sha256-prefixed-digest
        reviewResultDigest: null-or-sha256-prefixed-digest
        verdict: null-or-CLEAN-or-FINDINGS-or-UNUSABLE
        failureCode: null-or-max-turns-exhausted-or-provider-rejected-or-terminal-no-answer-or-adapter-terminal-failure
        noEffectEvidenceDigest: null-or-sha256-prefixed-digest
        integrityEvidenceDigest: null-or-sha256-prefixed-digest
        retirementEvidenceDigest: null-or-sha256-prefixed-digest
        readCoverageDigest: null-or-sha256-prefixed-digest
        coverageSummaryDigest: null-or-sha256-prefixed-digest
        currentCertificationBasis: null-or-reviewCertificationBasis
        certifying: true-or-false
  evidenceMutationReceipt: null-or-reviewEvidenceMutationReceiptV1
~~~

`resolvedEffort.kind=inapplicable` requires `requestedEffort:null` and is the only legal value when the adapter/model has no effort control. `applied` carries the exact admitted value; a null request then means the configured default. There is no sentinel, model-label inference or free-form effective-effort string.

The original dispatch projection never morphs. Only provider-action.read joins the immutable dispatch projection to the current terminal projection. Kind and route invariants reject crossed shapes: route is non-null and recovery digest null iff routeState is present; missing/integrity-failed has null route and a non-null safe recovery digest. It never uses the all-null non-review route arm to disguise a missing certifying binding. Kind invariants reject irrelevant non-null fields: only safe/UNUSABLE answers may carry answer/coverage/evidence; only provider-terminal-failure carries a   failure code; no-effect/integrity/retired carry exactly their corresponding   arm evidence-digest field. `terminalResultDigest` always remains the SHA-256   of the complete six-arm identity preimage and never aliases that evidence   digest. Only a current sufficient safe answer can set certifying true. The projection rejects an `actionRef.adapterId` that differs from the route adapter, terminal journal pair, recovery pair or evidence pair.

#### Certifying-action and route-integrity recovery owner

ProviderRouteIntegrityRecoveryService is the only startup and ambiguity owner of every certifying action, including an otherwise intact dispatched action whose provider effect is unknown and an action whose route, target, bundle, prompt, profile or lineage join is missing or contradictory. Every certifying route/action is excluded from generic provider-action recovery and prepared- action re-enqueue. Its daemon-internal operation fabric.internal.provider-route-integrity.reconcile runs under the current daemon recovery generation; no agent/operator/chair may invoke it or repair a route.

Its durable state machine is:

~~~text
detected -> inspecting
  -> terminal-proved-no-effect
  -> terminal-proved-usage
  -> awaiting-human-retire -> terminal-retired-unknown
~~~

The live scoped read surface is closed:

~~~yaml
providerRouteIntegrityRecoveryReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  actionRef: ProviderActionRefV1

providerRouteIntegrityRecoveryProjectionV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-task
  actionRef: ProviderActionRefV1
  targetGeneration: positive-generation
  slot: native-or-other-primary-or-cursor-grok-or-agy-gemini
  attemptGeneration: positive-generation
  recoveryGeneration: positive-generation
  state: detected-or-inspecting-or-terminal-proved-no-effect-or-terminal-proved-usage-or-awaiting-human-retire-or-terminal-retired-unknown
  reason: intact-effect-ambiguity-or-route-row-missing-or-route-row-conflict-or-route-receipt-mismatch-or-target-binding-invalid-or-bundle-binding-invalid-or-prompt-binding-invalid-or-profile-binding-invalid-or-lineage-binding-invalid
  reservationDigest: sha256-prefixed-digest
  routeState: present-or-missing-or-integrity-failed
  routeReceiptDigest: null-or-sha256-prefixed-digest
  lookupState: not-attempted-or-in-flight-or-completed
  lookupEvidenceDigest: null-or-sha256-prefixed-digest
  disposition: null-or-proved-no-effect-release-or-exact-usage-settled-or-conservative-full-ceiling-settled-or-full-ceiling-retired
  settlementDigest: null-or-sha256-prefixed-digest
  recoveryEvidenceDigest: sha256-prefixed-digest
  retirementEligible: true-or-false
~~~

`fabric.v1.provider-route-integrity-recovery.read` accepts exactly the request above and returns exactly the current projection. Errors are `NOT_FOUND|AUTHORITY_DENIED|SCOPE_MISMATCH|INTEGRITY_FAILURE`; they carry only `{schemaVersion:1,code,evidenceDigest:null-or-digest}`. Retirement eligibility is true iff state is `awaiting-human-retire` and the reservation/action joins are intact. This live projection, not the receipt recovery array, supplies CAS authority. Receipt recovery rows are immutable audit snapshots and are explicitly forbidden as Preview/Commit inputs.

The row records a closed reason: intact-effect-ambiguity or one exact broken- binding reason. Detection fences further provider I/O, marks the action noncertifying while unresolved and freezes only its reservation dimensions. If durable preparation and the dispatch journal prove dispatch never began, recovery terminalises no-effect, returns its full capacity and writes reservation state `settled`. A dispatched or accepted action receives at most one bounded pair-keyed lookup when its adapter contract can identify it. An exact safe/unusable/failure terminal result flows through the ordinary action-bound terminaliser and its canonical terminal-input digest; complete authenticated usage settles exactly and absent/partial usage charges the remaining spendable reservation. An authenticated closed no-effect lookup also returns full capacity under `settled`. The disposition name `proved-no-effect-release` describes capacity accounting, not the reservation row state: `released` is pre-admission-only with null attempt and every attached terminal has immutable positive attempt plus state `settled`. A terminal effect with an unverifiable binding becomes integrity-terminal and conservatively settles. Absent, timed- out, malformed, conflicting or permanently unavailable lookup never proves no effect and enters awaiting-human-retire with the reservation retained.

The only retirement path is a typed provider-route-integrity-retire intent through fabric.v1.operator-action.preview/commit. It requires an operator with external-effect authority, the exact action/recovery generation and reservation digest, a persisted consequential gate and independently attested direct-human confirmation. Confirmed Commit performs no provider call. It consumes the full remaining spendable reservation, releases only terminal concurrency capacity, records terminal-retired-unknown and terminalises the action noncertifying. This cannot overbook, fabricate outcome or leave an unresolvable route freeze.

Preview and Commit equality-bind the live `(actionRef,recoveryGeneration,state=awaiting-human-retire,reservationDigest)`. Any changed value rejects. The Console may offer retirement only from a live `recovery-action` row with `retirementEligible:true`.

Every terminal branch commits action, reservation, authority-unknown flags, recovery evidence digest and run recovery-state exit in one transaction. Dimensions unfreeze when no other unknown owner remains. Recovery never reconstructs a route/bundle/prompt, dispatches or redispatches the provider, or converts a no-effect/integrity/retired action into review evidence. A valid answer is evidence only through the ordinary automatic terminal transaction. Store/catalogue corruption that prevents identifying the reservation follows the existing store-corruption stop; it is not silently represented as a route freeze.

### Capability-backed deployed routes and operational telemetry

This contract incorporates the durable findings from the [July 2026 continuity and routing research](../../research/evidence-snapshots/agent-continuity-routing-2026-07.md) into the adapter, route and lifecycle owners. It adds no autonomous route learner, context-pressure controller, compaction threshold, global model preference, native deep-mode registry or OpenCode activation.

Every activated adapter publishes one current immutable `adapterCapabilitySnapshotV1`. Every object below is closed; every array is ordered as stated and duplicate-free, and a bound exists only where explicitly stated. Timestamps are RFC 3339 UTC, digests use lowercase `sha256:<64 hex>`, and IDs are nonempty UTF-8 strings of at most 256 bytes.

~~~yaml
adapterCapabilitySnapshotV1:
  schemaVersion: 1
  snapshotId: stable-id
  snapshotGeneration: positive-integer
  adapterId: exact-adapter-id
  adapterContractDigest: sha256-prefixed-digest
  hostId: exact-host-id
  hostVersion: exact-host-version
  source: runtime-discovery | capability-fixture | unavailable
  observedAt: timestamp
  expiresAt: timestamp
  capabilities:
    oneOf:
      - kind: available
        modelCatalog:
          - family: canonical-family
            model: exact-provider-model
            effort:
              oneOf:
                - kind: applied
                  normalizations:
                    - rawProviderEffort: exact-provider-value
                      normalizedReasoningEffort: none | low | medium | high | xhigh | max
                - kind: inapplicable
            nativeModeNormalizations:
              - rawNativeMode: exact-provider-value | null
                orchestrationMode: single | native-subagents | dynamic-workflow | provider-multi-agent
        context:
          reporting: reported | estimated | unavailable
          compactInPlace: true | false | unknown
          freshSession: true | false | unknown
          boundaryInjection: verified | unverified | unavailable
        orchestration:
          nativeSubagents: none | bounded | recursive | unknown
          maxDepth: nonnegative-integer | null
          maxConcurrency: positive-integer | null
        safety:
          enforcedReadOnly: true | false | unknown
          permissionSource: adapter | host | config-overlay | unknown
      - kind: unavailable
        reason: exact-safe-unavailable-reason
  capabilityBodyDigest: sha256-prefixed-digest
  snapshotDigest: sha256-prefixed-digest

capabilitySnapshotRefV1:
  snapshotId: stable-id
  snapshotGeneration: positive-integer
  snapshotDigest: sha256-prefixed-digest
  capabilityBodyDigest: sha256-prefixed-digest

capabilitySnapshotSummaryV1:
  admission:
    snapshotRef: capabilitySnapshotRefV1
    source: runtime-discovery | capability-fixture | unavailable
    observedAt: timestamp
    expiresAt: timestamp
  dispatch:
    oneOf:
      - null
      - snapshotRef: capabilitySnapshotRefV1
        source: runtime-discovery | capability-fixture | unavailable
        observedAt: timestamp
        expiresAt: timestamp
~~~

The two `capabilities` arms are disjoint. `source: unavailable` requires the `kind: unavailable` arm; the available arm requires runtime discovery or a capability fixture. `modelCatalog` is nonempty, has at most 256 entries and is sorted by `(family, model)`. Applied effort normalisations and native-mode rows each have 1..64 entries, are sorted by raw provider value with null native mode first, and are unique. The inapplicable effort arm has no mappings. Each raw value maps to exactly one normalised value. Null depth/concurrency and explicit `unknown` mean unknown, not unlimited or false. Runtime discovery and capability fixtures are distinct sources. A capability fixture records verified behaviour, not provider version equality. Product prose, a model alias or a prior successful call is not a capability snapshot.

`capabilityBodyDigest` is SHA-256 of RFC 8785 JCS of exactly `{schemaVersion,adapterId,adapterContractDigest,hostId,hostVersion,source,capabilities}`. Snapshot ID, generation, observation/expiry clocks and both digest fields are excluded. `snapshotDigest` is SHA-256 of RFC 8785 JCS of the complete snapshot with only `snapshotDigest` omitted. Thus a refreshed immutable snapshot may advance its instance identity and clocks while retaining the same body digest. Every ref equality-copies all four fields from its snapshot row; no digest-only or generation-only reference is valid.

Every answer-bearing provider action binds one immutable `deployedRouteAdmissionV1`; terminal evidence may append one `deployedRouteObservationV1`. They supplement, and do not replace, the existing `model-route.v1`, `ProviderActionRefV1`, route-event and certifying-review contracts in the certifying-review contract.

~~~yaml
discoverySurfaceManifestV1:
  schemaVersion: 1
  hostId: exact-host-id
  hostVersion: exact-host-version
  providerProfile: exact-profile-id
  rawNativeMode: exact-provider-value | null
  principalScopeDigest: sha256-prefixed-digest
  permissionProfileDigest: sha256-prefixed-digest
  negotiatedFeatureSetDigest: sha256-prefixed-digest
  rendererVersion: exact-version
  bootstrapText: exact-rendered-bootstrap-text
  skills:
    - name: exact-visible-skill-name
      description: exact-visible-skill-description
  tools:
    - name: exact-visible-tool-name
      description: exact-visible-tool-description
      inputSchema: exact-canonical-JSON-Schema
  agentCommands:
    - name: exact-visible-agent-or-command-name
      description: exact-visible-description
  nativePreambleText: exact-rendered-native-preamble
  bootstrapDigest: sha256-prefixed-digest
  skillCatalogueDigest: sha256-prefixed-digest
  toolRegistryDigest: sha256-prefixed-digest
  agentCommandRegistryDigest: sha256-prefixed-digest
  nativePreambleDigest: sha256-prefixed-digest

discoverySurfaceRefV1:
  evidenceId: exact-EvidenceArtifactRegistration-id
  evidenceRevision: positive-integer
  artifactRef:
    path: canonical-relative-path
    digest: sha256-prefixed-digest
  hostId: exact-host-id
  hostVersion: exact-host-version
  providerProfile: exact-profile-id
  rawNativeMode: exact-provider-value | null
  evidenceKind: discovery-surface.v1
  producer: fabric-daemon
  manifestDigest: sha256-prefixed-digest

adapterEffectiveConfigurationRefV1:
  configurationId: stable-id
  configurationRevision: positive-integer
  configurationDigest: sha256-prefixed-digest

deployedRouteAdmissionV1:
  schemaVersion: 1
  actionRef: ProviderActionRefV1
  routeRequestDigest: sha256-prefixed-digest
  routeReceiptDigest: sha256-prefixed-digest
  requested:
    adapterAlias: exact-configured-alias
    modelAlias: exact-configured-alias
    explicitModel: exact-provider-model | null
    rawProviderEffort: exact-provider-value | null
    rawNativeMode: exact-provider-value | null
  admitted:
    hostId: exact-host-id
    adapterId: exact-adapter-id
    adapterContractDigest: sha256-prefixed-digest
    endpointProvider: exact-provider-id
    family: canonical-family
    model: exact-provider-model
    resolvedEffort: resolvedEffortV1
    normalizedReasoningEffort: none | low | medium | high | xhigh | max | null
    rawNativeMode: exact-provider-value | null
    orchestrationMode: single | native-subagents | dynamic-workflow | provider-multi-agent
    capabilitySnapshotRef: capabilitySnapshotRefV1
    effectiveConfigurationRef: adapterEffectiveConfigurationRefV1
    requestedConfigurationDigest: sha256-prefixed-digest
    effectiveConfigurationDigest: sha256-prefixed-digest
    permissionProfileDigest: sha256-prefixed-digest
    discoverySurfaceRef: discoverySurfaceRefV1
  routePolicyRevision: exact-revision
  harnessRevision: exact-revision
  harnessDigest: sha256-prefixed-digest
  contextPolicyRevision: exact-revision
  contextPolicyDigest: sha256-prefixed-digest
  admissionDigest: sha256-prefixed-digest

deployedRouteDispatchV1:
  schemaVersion: 1
  actionRef: ProviderActionRefV1
  admissionDigest: sha256-prefixed-digest
  dispatchOrdinal: positive-contiguous-integer
  capabilitySnapshotRef: capabilitySnapshotRefV1
  effectiveConfigurationRef: adapterEffectiveConfigurationRefV1
  permissionProfileDigest: sha256-prefixed-digest
  discoverySurfaceRef: discoverySurfaceRefV1
  dispatchedAt: timestamp
  dispatchDigest: sha256-prefixed-digest

observedValueV1:
  oneOf:
    - state: observed
      value: exact-type-specific-value
      source: provider-result | adapter-attestation
      confidence: exact | attested
    - state: unavailable
      value: null
      source: unavailable
      confidence: unknown

deployedRouteObservationV1:
  schemaVersion: 1
  actionRef: ProviderActionRefV1
  admissionDigest: sha256-prefixed-digest
  hostId: observedValueV1<nonempty-id>
  adapterId: observedValueV1<nonempty-id>
  endpointProvider: observedValueV1<nonempty-id>
  family: observedValueV1<canonical-family>
  model: observedValueV1<exact-provider-model>
  resolvedEffort: observedValueV1<resolvedEffortV1>
  normalizedReasoningEffort: observedValueV1<none-or-low-or-medium-or-high-or-xhigh-or-max-or-null>
  rawNativeMode: observedValueV1<exact-provider-value-or-null>
  orchestrationMode: observedValueV1<single-or-native-subagents-or-dynamic-workflow-or-provider-multi-agent>
  observedAt: timestamp
  observationDigest: sha256-prefixed-digest

actualReviewRouteIdentityV1:
  schemaVersion: 1
  admissionDigest: sha256-prefixed-digest
  observationDigest: sha256-prefixed-digest
  hostId: observedValueV1<nonempty-id>
  adapterId: observedValueV1<nonempty-id>
  endpointProvider: observedValueV1<nonempty-id-required-observed>
  family: observedValueV1<canonical-family-required-observed>
  model: observedValueV1<exact-provider-model-required-observed>
  resolvedEffort: observedValueV1<resolvedEffortV1>
  normalizedReasoningEffort: observedValueV1<none-or-low-or-medium-or-high-or-xhigh-or-max-or-null>
  rawNativeMode: observedValueV1<exact-provider-value-or-null>
  orchestrationMode: observedValueV1<single-or-native-subagents-or-dynamic-workflow-or-provider-multi-agent>
  actualRouteIdentityDigest: sha256-prefixed-digest
~~~

`admissionDigest`, `dispatchDigest` and `observationDigest` are separate SHA-256 digests over RFC 8785 JCS of their complete closed objects with only their own digest field omitted. Admission never changes after action commit. Each actual dispatch appends one immutable contiguous-ordinal dispatch row immediately before provider I/O; it parent-binds the admission and exact snapshot, effective-configuration, permission and discovery-surface identities used for that attempt. Every dispatch row also enters the existing ordered route-event journal and receipt history; the joined public route uses `latestDispatch` only as its labelled current detail, not as a replacement for history. Observation is absent before terminal evidence and is inserted at most once; it parent- binds the immutable admission digest. `observedValueV1` expands in the checked-in schema to a closed type-specific union. Required identity values cannot be null in the observed arm. Exactly two typed cases admit an observed null: raw native mode, meaning the provider proved no raw native mode, and normalised reasoning when resolved effort is observed `inapplicable`. `state: unavailable` remains distinguishable. Every field has its own evidence source/confidence, so a provider may prove model while effort remains honestly unavailable. `provider-result` and `exact` require a field directly present in the authenticated provider result. `adapter-attestation` and `attested` require a contract-defined adapter observation. Crossed source, confidence, state or null combinations reject. `actualRouteIdentityDigest` is SHA-256 of RFC 8785 JCS of the complete closed `actualReviewRouteIdentityV1` with only that digest omitted. Every route field equality-copies the corresponding observation arm, and its admission/observation digests equality-bind the exact route pair. Endpoint provider, family and model must be proved observed arms; the remaining arms may be honestly unavailable. Any observed host/adapter/provider/family/model/effort/native-mode/orchestration value unequal to admission is `actual-route-mismatch`, even when all three profile-required identity values match. An unavailable required provider/ family/model arm cannot form this object and is `actual-route-unproved`. An observed null raw native mode requires observed `single` orchestration; non-single orchestration requires an observed non-null raw native mode. An unavailable native-mode field cannot be used to infer orchestration. When both effort fields are observed, applied resolved effort requires the snapshot-mapped nonnull normalised value, while inapplicable requires an observed null normalised value. An unavailable effort arm cannot be filled from admission.

An applied admitted `resolvedEffortV1.value` is the raw provider effort, must equal one applied capability normalisation and requires its corresponding non-null normalised reasoning value. `inapplicable` requires the snapshot's inapplicable arm, null requested effort and null normalised reasoning, as the certifying-review contract already specifies. Raw native mode and orchestration mode must equal one mapping row in the same model snapshot. The raw effort/native-mode values pass unchanged to the adapter; policy fields cannot reconstruct or overwrite them. Requested, admitted and observed values remain separate even when equal; substitutions stay in the existing ordered event journal.

`discoverySurfaceRefV1` points to the existing immutable `EvidenceArtifactRegistration` from the bounded artifact-read contract, but only the daemon-internal discovery renderer may create evidence kind `discovery-surface.v1` with existing `publisherKind: fabric` and `producer: fabric-daemon`. Public/agent evidence publication rejects that kind. After resolving exact host/version/profile/native mode and the active generated skill/tool/agent-command registries, the daemon renders the session-start manifest. The exact artifact bytes are RFC 8785 JCS of the closed `discoverySurfaceManifestV1`, which deliberately contains no digest of itself. `manifestDigest` is SHA-256 of those exact bytes and must byte-equal `artifactRef.digest`; the registered artifact bytes must reproduce it. The ref's host/version/profile/raw-mode tuple equality-copies the manifest and is also bound to route resolution, capability host/body and the adapter launch envelope. The manifest binds principal scope, permission profile, negotiated features, renderer version, the exact rendered bootstrap/preamble and exact ordered skill/tool/command catalogues. Each of the five component digests is SHA-256 of the corresponding exact canonical text/array value and rejects a content/digest mismatch. No artificial item or byte ceiling is added here; existing run-artifact/storage authority remains the resource boundary. Admission requires its permission digest and admitted native mode to match the manifest/ ref; requested native mode is either the same raw value or null under the recorded configured-default policy. The launch envelope equality-binds the same ref and effective configuration; an adapter that cannot prove application leaves the route unavailable. This records the actual rendered surface and creates no target, catalogue-count limit or other hard ceiling.

At new-action admission, snapshot expiry or adapter-contract/model/effort/mode incompatibility rejects before provider I/O. Admission immutably binds both the snapshot instance ref and its body digest. Immediately before every initial dispatch or permitted retry, the daemon reads the current snapshot and requires it to be unexpired, adapter/contract/host compatible and body-equal to the admitted body. A newer instance with identical body is permitted and is written to that attempt's dispatch row, so harmless refresh cannot starve an action. Body, permission-profile or discovery-surface drift terminalises the zero-effect action and resolves afresh under a new pair. The per-action effective- configuration ref must still identify the same adapter/contract/executable, snapshot body, requested/effective configuration, permission and surface at every dispatch; any mismatch is likewise no-effect. Admission is never rewritten. Ambiguous effect stays with the original action/recovery owner and cannot reroute or replay.

The existing `fabric.v1.provider-action.read`, generated agent/MCP read and scoped operator Evidence projection expose one closed route variant containing `admission: deployedRouteAdmissionV1`, `capabilitySummary: capabilitySnapshotSummaryV1`, `latestDispatch: null | deployedRouteDispatchV1` and `observation: null | deployedRouteObservationV1`. The summary's separately labelled admission and dispatch arms each equality-copy their own snapshot ref, source, observed/expiry clocks and body digest; when present the dispatch arm also equals `latestDispatch.capabilitySnapshotRef`. A refreshed dispatch instance can never inherit the admission snapshot's clocks. Snapshot/route/action joins are exact, not a latest-timestamp choice. Receipt-v2 `providerRoutes` uses the same closed shape. No separate Console codec or action-ID-only lookup exists.

Context pressure has one public, non-authoritative wire:

~~~yaml
providerContextPressureV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  agentId: exact-agent
  adapterId: exact-adapter
  providerGeneration: positive-integer
  contextRevision: nonnegative-integer
  observationAuditRef:
    sourceEventId: exact-lifecycle-observation-event
    providerGeneration: exact-parent-generation
    contextRevision: exact-parent-revision
    evidenceDigest: sha256-prefixed-digest
  pressure: low | medium | high | unknown
  source: native-exact | native-estimated | hook-boundary | unavailable
  confidence: exact | estimated | unknown
  windowTokens: nonnegative-integer | null
  usedTokens: nonnegative-integer | null
  remainingTokens: nonnegative-integer | null
  observedAt: timestamp
  expiresAt: timestamp
  evidenceDigest: sha256-prefixed-digest
  revision: positive-integer

providerContextPressureReadV1:
  oneOf:
    - schemaVersion: 1
      currency: current
      pressure: providerContextPressureV1
      readAt: timestamp-at-or-after-observedAt-and-before-expiresAt
      ageSeconds: nonnegative-integer
    - schemaVersion: 1
      currency: stale
      pressure: providerContextPressureV1
      readAt: timestamp-at-or-after-pressure-expiresAt
      ageSeconds: nonnegative-integer
    - schemaVersion: 1
      currency: unavailable
      pressure: null
      readAt: timestamp
      ageSeconds: null

providerContextPressureReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  agentId: exact-agent
~~~

`fabric.v1.provider-context-pressure.read` and the scoped operator System projection accept exact project-session/run/agent scope and return only `providerContextPressureReadV1`. The record equality-binds the exact lifecycle observation audit tuple; no orphan or best-effort join is valid. Token values are all null for unavailable source, which also requires `pressure: unknown` and `confidence: unknown`. `native-exact` requires exact confidence and three nonnull token fields satisfying `usedTokens + remainingTokens = windowTokens`; `native-estimated` requires estimated confidence and the same nonnull arithmetic. `hook-boundary` requires exact or estimated confidence; its token triple is all null or all nonnull with the same arithmetic. Unknown confidence requires unknown pressure. `expiresAt` is later than `observedAt`. `ageSeconds` and stale currency derive at the read snapshot from `readAt`, `observedAt` and `expiresAt`; age is the nonnegative whole-second difference between read and observation time. Reads never mutate a row and never expose a percentage. This record reserves no spend, grants no authority and triggers no lifecycle action.

the lifecycle-custody contract remains the only lifecycle authority. A policy-required rotation starts a genuinely fresh provider context and injects only the bounded, daemon-validated checkpoint/handoff. Same-history attach/resume is crash recovery only. Checkpoint identity binds canonical task, authority, lease, mailbox, child, open-work, evidence, artifact and repository revisions already owned by lifecycle custody; model narrative may describe but cannot author those values. Parent rotation never implies child rotation, completion or identity. A native child is independent only when the adapter provides the stable identity mapping required by the existing child-custody contracts; otherwise its native graph remains one opaque bounded task. Automatic pressure thresholds, hysteresis, maximum compaction counts and successor selection are explicitly outside this contract.

Coordination topology planning has one closed, revisioned advisory record:

~~~yaml
topologyWavePlanRefV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-coordination-root-task
  waveId: stable-wave-id
  waveRevision: positive-integer
  planDigest: sha256-prefixed-digest

topologyWavePlanV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-coordination-root-task
  waveId: stable-wave-id
  waveRevision: positive-contiguous-integer
  predecessor: topologyWavePlanRefV1 | null
  dependencies:
    - dependencyTaskId: exact-task
      requiredState: ready | completed
      evidenceRef: exact-existing-task-or-evidence-ref
  decomposability:
    kind: atomic | decomposable | conditionally-decomposable
    evidenceRef: exact-existing-evidence-ref
  topology:
    executionShape: single-owner | fabric-explicit | host-native
    mode: serial | parallel | fan-out-fan-in | dynamic
    maximumConcurrentAgents: positive-integer
  chair:
    agentId: exact-current-chair
    principalGeneration: positive-integer
    chairLeaseGeneration: positive-integer
  stageOwners:
    - stageId: stable-stage-id
      taskId: exact-task
      ownerAgentId: exact-agent
      writePartitionId: stable-partition-id | null
  writePartitions:
    - partitionId: stable-partition-id
      ownerAgentId: exact-stage-owner-agent
      mode: exclusive-write | shared-read
      pathSetDigest: sha256-prefixed-digest
      authorityRef: exact-existing-authority-ref
  contention:
    mode: none | serialized | disjoint-partitions
    serializationOwnerAgentId: exact-agent | null
    evidenceRef: exact-existing-evidence-ref
  budget:
    providerTurns: nonnegative-integer
    toolCalls: nonnegative-integer
    wallClockSeconds: nonnegative-integer
    maximumParallelAgents: positive-integer
  stopConditions:
    - conditionId: stable-id
      kind: objective-complete | gate-failed | budget-exhausted | human-gate
      predicateRef: exact-existing-policy-or-gate-ref
  authority:
    authorityRevision: positive-integer
    authorityRef: exact-existing-run-authority-ref
    authorityDigest: sha256-prefixed-digest
  policy:
    policyRevision: positive-integer
    policyRef: exact-existing-coordination-policy-ref
    policyDigest: sha256-prefixed-digest
  state: proposed | approved | started | completed | superseded | cancelled
  rationaleRef: exact-registered-evidence-artifact-ref
  createdAt: timestamp
  planDigest: sha256-prefixed-digest

topologyWavePlanCurrentV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-coordination-root-task
  waveId: stable-wave-id
  waveRevision: positive-integer
  planDigest: sha256-prefixed-digest
  revision: positive-CAS-revision

topologyWavePlanInputV1:
  schemaVersion: 1
  taskId: exact-coordination-root-task
  waveId: stable-wave-id
  dependencies: exact-topologyWavePlanV1-dependencies
  decomposability: exact-topologyWavePlanV1-decomposability
  topology: exact-topologyWavePlanV1-topology
  stageOwners: exact-topologyWavePlanV1-stageOwners
  writePartitions: exact-topologyWavePlanV1-writePartitions
  contention: exact-topologyWavePlanV1-contention
  budget: exact-topologyWavePlanV1-budget
  stopConditions: exact-topologyWavePlanV1-stopConditions
  state: proposed | approved | started | completed | superseded | cancelled
  rationaleRef: exact-registered-evidence-artifact-ref

topologyWaveAppendRequestV1:
  schemaVersion: 1
  commandId: stable-command-id
  projectSessionId: exact-session
  coordinationRunId: exact-run
  expectedCurrent:
    oneOf:
      - kind: none
        expectedPointerRevision: 0
      - kind: current
        planRef: topologyWavePlanRefV1
        expectedPointerRevision: positive-CAS-revision
  plan: topologyWavePlanInputV1

topologyWaveAppendReceiptV1:
  schemaVersion: 1
  commandId: exact-command-id
  status: appended
  priorPlanRef: topologyWavePlanRefV1 | null
  planRef: topologyWavePlanRefV1
  pointer: topologyWavePlanCurrentV1
  receiptDigest: sha256-prefixed-digest

topologyWaveCurrentReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-coordination-root-task

topologyWaveCurrentReadV1:
  oneOf:
    - schemaVersion: 1
      currency: current
      plan: topologyWavePlanV1
      pointer: topologyWavePlanCurrentV1
    - schemaVersion: 1
      currency: stale
      plan: topologyWavePlanV1
      pointer: topologyWavePlanCurrentV1
    - schemaVersion: 1
      currency: unavailable
      plan: null
      pointer: null

topologyWaveListRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-coordination-root-task
  pageSize: positive-integer-at-most-200
  cursor: opaque-scope-and-watermark-bound-cursor | null

topologyWaveListV1:
  schemaVersion: 1
  plans: ordered-topologyWavePlanV1-array
  nextCursor: opaque-scope-and-watermark-bound-cursor | null
  watermarkRevision: nonnegative-integer
~~~

All arrays are canonically sorted and duplicate-free; dependencies, stageOwners, writePartitions and stopConditions are nonempty where their mode requires them. `planDigest` is SHA-256 of RFC 8785 JCS of the complete plan with only `planDigest` omitted. Every change, state advance or rationale change appends the next wave revision and advances the sole current pointer by CAS; plans and rationale artifacts are immutable. A successor equality-binds its predecessor, run/task, existing authority/policy refs and their current revisions at append. It cannot mint authority, expand a write partition, change the one coordination chair or choose agents automatically.

`fabric.v1.topology-wave.append` is the sole public mutation. It requires the current chair capability for the exact session/run and accepts only the closed request above. The daemon derives project/run, contiguous wave revision, current chair binding, current authority/policy tuples, `createdAt` and `planDigest`; none is caller-authored. The `none` arm is legal only when no pointer exists and revision is zero. The `current` arm must equality-match the pointed ref and revision. One transaction derives plan predecessor as null for `none` or exactly the current arm's `planRef`; the caller cannot author or fork it. It validates task/dependency/ owner/write authority, inserts the next immutable plan, CAS-advances the pointer and records the receipt. Exact command replay returns that receipt before current-state checks; changed replay or pointer conflict mutates nothing. `receiptDigest` is SHA-256 of exact receipt JCS with only that field omitted. This operation is for the chair/harness, not the Console.

`fabric.v1.topology-wave.current.read` accepts exact project-session/run/task scope in `topologyWaveCurrentReadRequestV1` and returns only `topologyWaveCurrentReadV1`; `fabric.v1.topology-wave.list` accepts/returns the closed list pair above in stable plan-digest order. A plan is read-derived stale when its authority, policy, chair binding or dependency is no longer current, or when its immutable predecessor chain is missing, noncontiguous or digest- invalid. A predecessor is historical by definition and need not be the current pointer; an exact intact immediately preceding revision/wave link does not make its successor stale. The current/stale arms require plan and pointer to equality- bind the same exact row; absent pointer requires the unavailable/null arm. Reads never rewrite plan state. Console uses this same current projection before a wave starts and keeps stale plans visible; no Console-only planner, second authority ledger or automatic topology policy exists.

The fabric may export privacy-minimised `fabricOperationalSpanV1` rows:

~~~yaml
fabricOperationalSpanV1:
  schemaVersion: 1
  spanId: stable-id
  parentSpanId: stable-id | null
  runId: exact-run-id
  taskId: exact-task-id | null
  agentId: exact-agent-id | null
  actionRef: ProviderActionRefV1 | null
  routeAdmissionDigest: sha256-prefixed-digest | null
  operation: exact-registered-operation
  status: ok | error | cancelled | unknown
  durationMs: nonnegative-integer
  inputTokens: nonnegative-integer | null
  outputTokens: nonnegative-integer | null
  retryCount: nonnegative-integer
  errorCode: exact-safe-code | null
  observedAt: timestamp
~~~

Spans contain no prompt, answer, tool argument/result, artifact bytes, private message, capability or absolute path. Generic telemetry is operational evidence only; the richer receipt remains authoritative for authority, disclosure, reviewer relation, gates and artifact evidence. Conformance tests cover closed codec and unknown-enum rejection; exact non-self-referential discovery-manifest bytes/digest/registration equality; snapshot expiry and identical-body refresh; body/permission/surface drift before effect; raw/normalised round-trip and honest unknown versus observed-null native mode; actual review-route proof/mismatch and observed effort/native mismatch with adverse-finding retention; ambiguous-action non-rerouting; context-pressure audit joins/discriminated stale read/cross-arm rejection/no-percentage; topology append/CAS/discriminated currency/predecessor- chain/authority fencing; fresh rotation versus same-history recovery; independent child custody; and content-free telemetry.

Added requirements are:

- **FR-077:** Each activated adapter shall publish one current immutable closed
  capability snapshot whose source and available/unavailable arm agree.
- **FR-078:** Capability bodies, snapshot instances and references shall use the
  exact digest preimages and equality bindings defined above.
- **FR-079:** Every answer-bearing provider action shall bind one immutable route
  admission; each dispatch shall append its exact attempt row and terminal   evidence may append at most one observation.
- **FR-080:** Actual review-route identity shall come only from the exact
  admission/observation pair and shall preserve unavailable and observed-null as   distinct states.
- **FR-081:** Route admission and launch shall bind the daemon-rendered discovery
  surface manifest, its registered artifact and every displayed component   digest.
- **FR-082:** Every provider dispatch shall revalidate the current capability
  body, effective configuration, permission profile and discovery surface;   incompatible drift shall end the zero-effect action and require a new pair.
- **FR-083:** Provider-action reads, receipts and operator Evidence shall reuse
  the one closed route shape and the daemon-global provider action pair.
- **FR-084:** Context pressure shall use the one exact, expiring,
  non-authoritative projection and shall remain incapable of reserving spend or   triggering lifecycle action.
- **FR-085:** Policy-required rotation shall create a fresh provider context and
  inject only the daemon-validated lifecycle checkpoint; same-history resume   shall remain recovery-only.
- **FR-086:** Coordination topology shall use one append-only, revisioned wave
  plan and one CAS current pointer under the existing chair, authority and   policy records.
- **FR-087:** Topology current/list reads shall expose the closed plan and
  pointer shapes and derive stale currency without rewriting plan state.
- **FR-088:** Operational telemetry shall use only the privacy-minimised span
  shape and shall remain non-authoritative.
- **NFR-034:** Capability, route, topology, pressure and telemetry records shall
  be closed, canonically ordered and deterministically digestible.
- **NFR-035:** Unknown capability and context values shall remain explicitly
  unknown, never be widened to unlimited, false or inferred support.
- **NFR-036:** Capability evidence shall not create an autonomous route learner,
  pressure controller, compaction threshold or ambiguous-effect redispatch.
- **NFR-037:** Topology planning shall not mint authority, expand write scope,
  replace the chair or choose agents automatically.
- **NFR-038:** Generic telemetry shall contain no prompt, answer, tool payload,
  artifact bytes, private message, capability or absolute path.

Acceptance additionally requires:

- **AC-056:** Capability fixtures cover closed codecs, source/arm parity, exact
  digest preimages, immutable refresh, expiry and unknown-enum rejection.
- **AC-057:** Route fixtures cover immutable admission, contiguous dispatch,
  one observation, actual-route proof/mismatch, honest unavailable values and   zero-effect drift before provider I/O.
- **AC-058:** Discovery fixtures reproduce exact registered manifest bytes and
  reject crossed host, version, profile, mode, permission, registry or launch   bindings.
- **AC-059:** Context-pressure fixtures cover every source/confidence arm,
  token arithmetic, exact observation joins, expiry, stale reads and the   absence of percentage or lifecycle authority.
- **AC-060:** Topology append fixtures cover exact replay, changed replay,
  contiguous predecessors, CAS conflicts and authority/policy/dependency   fencing.
- **AC-061:** Topology read fixtures cover current, stale and unavailable arms,
  intact historical predecessors, stable ordering and immutable plans.
- **AC-062:** Lifecycle fixtures distinguish fresh rotation from same-history
  recovery and prove independent child custody.
- **AC-063:** Telemetry fixtures prove the closed codec and absence of content,
  secrets and authority-bearing fields.
