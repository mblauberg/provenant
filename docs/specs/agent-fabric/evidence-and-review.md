# Agent Fabric evidence and review

## Review evidence receipt definitions

~~~yaml
safeFinding:
  required: [findingDigest, findingId, severity, summary, evidence,
    originTargetGeneration, originActionRef, originResultDigest,
    originDeliveryManifestRef, originDeliveryReviewBasisDigest,
    originBundleDigest, repairCurrency]
  severityEnum: [P0, P1, P2]
  findingDigestType: digest
  findingIdType: byte-validated-finding-id
  summaryType: byte-validated-safe-summary
  evidenceType: byte-validated-safe-evidence
  positive: [originTargetGeneration]
  ids: [originDeliveryManifestRef]
  digests: [originResultDigest, originDeliveryReviewBasisDigest,
    originBundleDigest]
  originActionRef:
    required: [adapterId, actionId]
    ids: [adapterId, actionId]
  repairCurrency:
    required: [kind, originRepositorySourceStateDigest, evidenceRefs]
    kindEnum: [repository-source, registered-evidence, mixed]
    nullableDigests: [originRepositorySourceStateDigest]
    evidenceRefItemsRequired: [evidenceRef, evidenceRevision, contentDigest]
    evidenceRefItemIds: [evidenceRef]
    evidenceRefItemPositive: [evidenceRevision]
    evidenceRefItemDigests: [contentDigest]
    evidenceRefOrdering: evidenceRef-UTF8-then-evidenceRevision
    conditional: repository-source requires nonnull source digest and empty
      evidenceRefs; registered-evidence requires null source digest and nonempty
      evidenceRefs; mixed requires nonnull source digest and nonempty evidenceRefs

coverageSummary:
  required: [mode, mandatoryComplete, groups, byteComplete]
  mode: {const: manifest-complete-risk-directed}
  mandatoryCompleteType: boolean
  byteCompleteType: boolean
  groups:
    itemsRequired: [groupId, totalCount, readCount, unreadCount,
      unreadObjectSetDigest]
    itemIds: [groupId]
    groupIdEnum: [security-auth, protocol-schema, persistence-migration,
      provider-adapter, console-ui, tests-evaluations, documentation,
      generated-other]
    itemNonnegative: [totalCount, readCount, unreadCount]
    itemDigests: [unreadObjectSetDigest]
    itemInvariant: totalCount-equals-readCount-plus-unreadCount
    ordering: strictly-ascending-unique-by-groupId

findingSetRef:
  required: [findingSetDigest, findingCount, pageDigests]
  digests: [findingSetDigest]
  nonnegative: [findingCount]
  digestArrays: [pageDigests]
  invariant: zero count iff pageDigests empty

receiptFindingPage:
  required: [pageDigest, members]
  digests: [pageDigest]
  members: ordered-nonempty-safeFinding-records
  invariant: pageDigest-equals-sha256-of-RFC8785-JCS-reviewFindingPageV1

findingPages:
  type: array
  items: receiptFindingPage

findingWindow:
  required: [mode, maximumNewFindings, maximumNewFindingBytes,
    capacityReservationDigest]
  modeEnum: [normal, resolution-only]
  nonnegative: [maximumNewFindings, maximumNewFindingBytes]
  digests: [capacityReservationDigest]
  conditional: normal requires maximumNewFindings 32 and positive byte bound;
    resolution-only requires both maxima zero

lifecycleCustodyRefV1:
  required: [schemaVersion, runId, agentId, custodyId, custodyRevision]
  schemaVersion: {const: 1}
  ids: [runId, agentId, custodyId]
  positive: [custodyRevision]

reviewCertificationBasis:
  oneOf:
    - kind: active-binding
      required: [kind, actionBindingGeneration, activeBindingGeneration,
        terminalSequence, bindingChainDigest]
      positive: [actionBindingGeneration, activeBindingGeneration,
        terminalSequence]
      digests: [bindingChainDigest]
      invariant: actionBindingGeneration-equals-activeBindingGeneration
    - kind: predecessor-cut
      required: [kind, actionBindingGeneration, firstSuccessorBindingGeneration,
        activeBindingGeneration, terminalSequence, certificationCutSequence,
        certificationCutCustodyRef, certificationCutDigest, bindingChainDigest]
      positive: [actionBindingGeneration, firstSuccessorBindingGeneration,
        activeBindingGeneration, terminalSequence]
      nonnegative: [certificationCutSequence]
      certificationCutCustodyRef: lifecycleCustodyRefV1
      digests: [certificationCutDigest, bindingChainDigest]
      invariant: terminalSequence-less-than-or-equal-certificationCutSequence;
        certificationCutCustodyRef equals first-successor binding predecessor
        certification-cut custody and the referenced cut custody
    - kind: post-cut
      required: [kind, actionBindingGeneration, firstSuccessorBindingGeneration,
        activeBindingGeneration, terminalSequence, certificationCutSequence,
        certificationCutCustodyRef, certificationCutDigest, bindingChainDigest]
      positive: [actionBindingGeneration, firstSuccessorBindingGeneration,
        activeBindingGeneration, terminalSequence]
      nonnegative: [certificationCutSequence]
      certificationCutCustodyRef: lifecycleCustodyRefV1
      digests: [certificationCutDigest, bindingChainDigest]
      invariant: terminalSequence-greater-than-certificationCutSequence;
        certificationCutCustodyRef equals first-successor binding predecessor
        certification-cut custody and the referenced cut custody

reviewEvidenceRecord:
  required: [evidenceId, targetGeneration, slot, taskId, actionRef,
    terminalSequence, terminalKind, verdict, answerSafety, providerAnswerDigest,
    terminalResultDigest, reviewResultDigest, providerFailureCode,
    providerFailureDigest, routeReceiptDigest, routeObservationDigest,
    actualRouteIdentityDigest,
    finalPromptDigest, adapterId, endpointProvider, providerFamily, model, bundleDigest,
    coverageDigest, profileDigest, priorHeadGeneration, newHeadGeneration,
    attemptGeneration, priorEvidenceId, priorOpenFindingSet,
    reportedResolvedFindingDigests, acceptedResolvedFindingDigests,
    findingSet, newOpenFindingSet, repairRequiredFindingSet, findingWindow,
    readCoverageDigest, coverageSummary, reviewerFamilyRelation,
    certificationBasisAtTerminal, mutationReceiptDigest]
  nullOnly: [reviewResultDigest, providerFailureCode, providerFailureDigest,
    routeObservationDigest, actualRouteIdentityDigest, priorEvidenceId]
  nullConstants: [providerFailureCode, providerFailureDigest]
  terminalKindEnum: [safe-answer, unusable-answer]
  verdictEnum: [CLEAN, FINDINGS, UNUSABLE]
  answerSafetyEnum: [safe, unusable]
  conditional: safe-answer requires safe and CLEAN-or-FINDINGS with nonnull
    reviewResultDigest; unusable-answer requires unusable and UNUSABLE with null
    reviewResultDigest
  actionRef: ProviderActionRefV1
  ids: [evidenceId, taskId, adapterId, endpointProvider, providerFamily, model,
    priorEvidenceId]
  invariant: actionRef.adapterId-equals-adapterId
  positive: [targetGeneration, terminalSequence, newHeadGeneration,
    attemptGeneration]
  nonnegative: [priorHeadGeneration]
  digests: [providerAnswerDigest, terminalResultDigest, routeReceiptDigest,
    finalPromptDigest, bundleDigest, coverageDigest, profileDigest,
    readCoverageDigest, mutationReceiptDigest]
  nullableDigests: [reviewResultDigest, routeObservationDigest,
    actualRouteIdentityDigest]
  invariant: nonnull actualRouteIdentityDigest requires nonnull
    routeObservationDigest and proved endpoint-provider/family/model arms
  digestArrays: [reportedResolvedFindingDigests,
    acceptedResolvedFindingDigests]
  slotEnum: [native, other-primary, cursor-grok, agy-gemini]
  coverageSummary: coverageSummary
  reviewerFamilyRelationEnum: reviewerFamilyRelationEnum
  certificationBasisAtTerminal: reviewCertificationBasis
  priorOpenFindingSet: findingSetRef
  findingSet: findingSetRef
  newOpenFindingSet: findingSetRef
  repairRequiredFindingSet: findingSetRef
  findingWindow: findingWindow

reviewTerminalFailureRecord:
  required: [targetGeneration, slot, taskId, actionRef,
    terminalSequence, terminalKind, providerFailureCode,
    providerFailureDigest, terminalResultDigest, routeReceiptDigest,
    finalPromptDigest, adapterId, endpointProvider, providerFamily, model, bundleDigest,
    coverageDigest, profileDigest, attemptGeneration, unchangedHeadGeneration,
    unchangedOpenFindingSetDigest, unchangedRepairRequiredFindingSetDigest,
    reviewerFamilyRelation]
  terminalKind: {const: provider-terminal-failure}
  providerFailureCodeEnum: providerTerminalFailureEnum
  actionRef: ProviderActionRefV1
  slotEnum: [native, other-primary, cursor-grok, agy-gemini]
  ids: [taskId, adapterId, endpointProvider, providerFamily, model]
  positive: [targetGeneration, terminalSequence, attemptGeneration]
  nonnegative: [unchangedHeadGeneration]
  digests: [providerFailureDigest, terminalResultDigest, routeReceiptDigest,
    finalPromptDigest, bundleDigest, coverageDigest, profileDigest,
    unchangedOpenFindingSetDigest, unchangedRepairRequiredFindingSetDigest]
  reviewerFamilyRelationEnum: reviewerFamilyRelationEnum
  invariant: actionRef.adapterId-equals-adapterId

reviewCurrency:
  required: [target, source, chair, profile, currentCertificationBasis,
    certifying, blockerCodes]
  targetEnum: [current, stale, superseded]
  sourceChairProfileEnum: [current, stale]
  currentCertificationBasis: null-or-reviewCertificationBasis
  certifyingType: boolean
  blockerCodes: ordered-reviewCurrencyBlockerEnum

providerReview:
  required: [recordKind, record, currencyAtWatermark]
  recordKindEnum: [evidence, terminal-failure]
  conditional: evidence requires reviewEvidenceRecord; terminal-failure
    requires reviewTerminalFailureRecord
  currencyAtWatermark: reviewCurrency

~~~

`reviewCompletion` is a local blocker-dependent union, not an external `$ref`:

~~~yaml
reviewCompletion:
  required: [schemaVersion, blockers, targetGeneration, targetChair, reviewedArtifactRef,
    publicationLineageDigest, bundleDigest, manifestRootDigest, coverageDigest,
    riskReadMapDigest, mandatoryReadSetDigest, profileDigest, unavailableSlots, slots,
    finalReviewComplete]
  schemaVersion: {const: 1}
  blockerItems: topReviewBlockerEnum
  nullablePositive: [targetGeneration]
  nullableIds: [reviewedArtifactRef]
  targetChairType: targetChair-or-null
  nullableDigests: [publicationLineageDigest, bundleDigest,
    manifestRootDigest, coverageDigest, riskReadMapDigest,
    mandatoryReadSetDigest, profileDigest]
  finalReviewCompleteType: boolean
  oneOf:
    - when: required-certifying-capability-unavailable
      blockersFirstAndContains: certifying-review-capability-unavailable
      unavailableSlots: nonempty-ordered-certifyingSlotUnavailable-items
      targetProjection: exact-trustworthy-target-immutable-fields-or-all-null
      slots: []
      finalReviewComplete: false
    - when: finding-capacity-unavailable
      blockersFirstAndContains: finding-capacity-exhausted
      unavailableSlots: []
      targetProjection: exact-trustworthy-target-immutable-fields-or-all-null
      slots: []
      finalReviewComplete: false
    - when: targetGeneration-null-and-blockers-exactly-missing-target
      blockers: [missing-target]
      unavailableSlots: []
      targetFields: all-null
      profileDigest: null
      slots: []
      finalReviewComplete: false
    - when: targetGeneration-null-and-blockers-exactly-integrity-failure
      blockers: [integrity-failure]
      unavailableSlots: []
      targetFields: all-null
      profileDigest: null
      slots: []
      finalReviewComplete: false
    - when: trustworthy-target-present-but-structural-integrity-failed
      blockers: [integrity-failure]
      requiredTargetFields: [targetGeneration, reviewedArtifactRef,
        publicationLineageDigest, bundleDigest, manifestRootDigest,
        coverageDigest, riskReadMapDigest, mandatoryReadSetDigest]
      targetChair: null
      profileDigest: null
      unavailableSlots: []
      slots: []
      finalReviewComplete: false
    - when: targetGeneration-nonnull-and-profileDigest-null
      requiredTargetFields: [targetGeneration, targetChair,
        reviewedArtifactRef, publicationLineageDigest, bundleDigest,
        manifestRootDigest, coverageDigest, riskReadMapDigest,
        mandatoryReadSetDigest]
      blockersContains: profile-unavailable
      unavailableSlots: []
      profileDigest: null
      slots: []
      finalReviewComplete: false
    - when: targetGeneration-and-profileDigest-nonnull
      requiredTargetFields: [targetGeneration, targetChair,
        reviewedArtifactRef, publicationLineageDigest, bundleDigest,
        manifestRootDigest, coverageDigest, riskReadMapDigest,
        mandatoryReadSetDigest, profileDigest]
      slots: exactly-four-reviewSlot-objects
      unavailableSlots: []
      finalReviewCompleteIff: top-blockers-empty-and-every-slot-blockers-empty

certifyingSlotUnavailable:
  required: [projectSessionId, profileId, profileSchemaDigest,
    targetChairFamily, slot, adapterId, adapterContractDigest, providerFamily,
    model, sourceMode, runtimeIdentityDigest, platformIdentityDigest,
    availabilityRevision, reason]
  slotEnum: [native, other-primary, cursor-grok, agy-gemini]
  reasonEnum: [adapter-inactive, contract-mismatch, confinement-unproved,
    portal-unavailable, provider-runtime-unavailable]
  ids: [projectSessionId, profileId, targetChairFamily, adapterId,
    providerFamily, model, sourceMode]
  positive: [availabilityRevision]
  digests: [profileSchemaDigest, adapterContractDigest,
    runtimeIdentityDigest, platformIdentityDigest]
  ordering: profile-slot-order

reviewSlot:
  required: [slot, headGeneration, attemptGeneration, actionRef, evidenceId,
    terminalKind, verdict, resultDigest, providerFailureCode,
    providerFailureDigest, routeReceiptDigest, adapterId, providerFamily,
    model, readCoverageDigest, reviewerFamilyRelation, currentCertificationBasis,
    certifying, openFindingSet, blockers]
  nullOnly: [actionRef, evidenceId, terminalKind, verdict, resultDigest,
    providerFailureCode, providerFailureDigest, routeReceiptDigest,
    readCoverageDigest, currentCertificationBasis]
  slotEnum: [native, other-primary, cursor-grok, agy-gemini]
  terminalKindEnum: [safe-answer, unusable-answer,
    provider-terminal-failure, terminal-no-effect, integrity-terminal,
    retired-unknown, null]
  verdictEnum: [CLEAN, FINDINGS, UNUSABLE, null]
  providerFailureCodeEnum: [max-turns-exhausted, provider-rejected,
    terminal-no-answer, adapter-terminal-failure, null]
  reviewerFamilyRelationEnum: reviewerFamilyRelationEnum
  ids: [adapterId, providerFamily, model]
  actionRef: ProviderActionRefV1-or-null
  nullableIds: [evidenceId]
  nonnegative: [headGeneration, attemptGeneration]
  nullableDigests: [resultDigest, routeReceiptDigest, readCoverageDigest]
  certifyingType: boolean
  openFindingSet: findingSetRef
  blockerItems: slotReviewBlockerEnum
  conditional: provider-terminal-failure requires evidenceId/verdict/
    readCoverageDigest/currentCertificationBasis null, nonnull result/failure fields
    and headGeneration unchanged from its action snapshot; every other terminal
    requires provider failure fields null
~~~
#### Complete review bundle and current target

Fabric owns a separate accepted requirement projection; delivery-run v1 is not silently extended. `fabric.v1.delivery-requirement-map.seal` is its sole producer and only the authenticated current chair may invoke it. The request is an optimistic lock, not content selection:

~~~yaml
deliveryRequirementMapSealRequestV1:
  schemaVersion: 1
  commandId: stable-command-id
  projectSessionId: exact-project-session
  coordinationRunId: exact-coordination-run
  deliveryRunId: exact-delivery-run
  expectedMapGeneration: zero-or-positive-current-generation
  expectedAcceptedScopeRevision: positive-current-revision
  expectedCatalogueDigest: sha256-prefixed-current-checked-in-digest
~~~

`expectedMapGeneration` is zero iff none exists and otherwise equals the one current generation; every wrong zero/positive/current combination conflicts before derivation. From the active accepted scope and checked-in `console-acceptance-delivery-requirements.v1` catalogue, the daemon produces:

~~~yaml
consoleAcceptanceDeliveryRequirementsV1:
  schemaVersion: 1
  catalogueId: console-acceptance-delivery-requirements-v1
  entries:
    - requirementId: exact-stable-binding-id
      sourceRole: spec-or-adr-or-decision
      sourceRef: exact-registered-source-identity
      evidenceSelectors:
        - role: accepted-scope-or-test-or-evaluation-or-load-or-migration-or-generated-contract-or-gate-decision
          registryKind: exact-closed-evidence-kind
          selectorId: exact-check/evaluation/gate/profile-owned-id
          cardinality: exactly-one-or-complete-nonempty-current-set
          requiredStatus: pass-or-approved

deliveryRequirementMapV1:
  schemaVersion: 1
  artifactKind: delivery-requirement-map.v1
  projectSessionId: exact-project-session
  coordinationRunId: exact-coordination-run
  deliveryRunId: exact-delivery-run
  mapGeneration: positive-generation
  closureDigest: sha256-prefixed-generation-free-map-closure-digest
  catalogueDigest: sha256-prefixed-checked-in-catalogue-digest
  acceptedScope:
    artifactRef: exact-active-scope-registration
    artifactRevision: positive-revision
    contentDigest: sha256-prefixed-digest
  bindingSources:
    - role: spec-or-adr-or-decision
      artifactRef: exact-active-registration
      artifactRevision: positive-revision
      contentDigest: sha256-prefixed-digest
      requirementIds: ordered-unique-binding-ids
  requirements:
    - requirementId: exact-catalogue-id
      sourceRef: exact-binding-source
      disposition: proved
      evidenceRefs: ordered-exact-registration/revision/digest-records
~~~

Every checked-in binding ID appears exactly once, every listed source ID is catalogued and each entry has at least one selector. Selectors match immutable registry kind plus the exact owning check, evaluation, gate or profile ID; they never parse prose, path globs or caller labels. `exactly-one` rejects zero/multiple current rows; `complete-nonempty-current-set` rejects zero and sorts every current same-source row by registration/ revision/digest. `proved` requires every selector at its required status and current source-state digest. Final human acceptance/release is not a pre-review requirement and is excluded, rather than represented as pending technical work. Phase A captures catalogue/scope/source/evidence revisions; phase B equality- CASes all of them. The daemon rejects missing/extra/duplicate IDs, stale rows or unproved evidence and registers immutable content; caller-authored entries are unavailable. Stable single-flight is keyed by run/delivery run before phase A. Exact command replay returns its immutable result and changed replay conflicts; different commands serialize. Before allocating a generation, the daemon computes `closureDigest` as SHA-256 over RFC 8785 JCS of the complete prospective deliveryRequirementMapV1 with only `mapGeneration` and `closureDigest` omitted. An equal current closure digest returns that existing bytes/registration/ generation without inserting or superseding anything. A changed catalogue, accepted-scope/binding-source or selected evidence closure allocates exactly current generation plus one and then hashes/registers the complete map. An arbitrary command ID cannot churn current state or stale a completed review basis. Manifest seal consumes the one current map and embeds its complete source/requirement projection.

The run-to-plan binding contract lives in [Run plan declaration](run-plan-declaration.md).

The eligible-root request is exactly command ID, project-session/run/delivery-run IDs, expected coordination/checkpoint generation and expected current full HEAD as an optimistic lock. It accepts no path, artifact bytes, Git base, evidence role/list, summary, bundle/profile/provider field, gate snapshot or publication lineage. The daemon derives HEAD again and produces/registers these closed bytes on behalf of the authenticated current chair:

~~~yaml
implementationDeliverySealRequestV1:
  schemaVersion: 1
  commandId: stable-command-id
  projectSessionId: exact-project-session
  coordinationRunId: exact-coordination-run
  deliveryRunId: exact-delivery-run
  expectedCoordinationRevision: positive-revision
  expectedCheckpointGeneration: positive-generation
  expectedHeadObjectId: full-current-object-id-optimistic-lock

implementationDeliveryManifestV1:
  schemaVersion: 1
  artifactKind: implementation-delivery-manifest.v1
  projectSessionId: exact-project-session
  coordinationRunId: exact-coordination-run
  deliveryRunId: exact-delivery-run
  sealGeneration: positive-generation
  profile:
    profileId: certifying-review-four-slot-v1
    profileSchemaDigest: sha256-prefixed-digest
    riskTier: exact-current-risk-tier
  acceptedScope:
    artifactRef: exact-active-scope-registration
    artifactRevision: positive-revision
    contentDigest: sha256-prefixed-digest
  bindingSources:
    - role: spec-or-adr-or-decision
      artifactRef: exact-active-registration
      artifactRevision: positive-revision
      contentDigest: sha256-prefixed-digest
  requirementMap:
    - requirementId: exact-binding-id
      sourceRef: exact-binding-source
      disposition: proved
      evidenceRefs: ordered-exact-registration/revision/digest-records
  preReviewEvidence:
    - role: test-or-evaluation-or-load-or-migration-or-generated-contract
      evidenceRef: exact-current-registration
      evidenceRevision: positive-revision
      contentDigest: sha256-prefixed-digest
      status: pass
  repository:
    objectFormat: exact-format
    baseObjectId: full-approved-run-start-object-id
    headObjectId: full-object-id
    headTreeId: full-object-id
    indexTreeId: exact-head-tree-id
    worktreeState: clean
    sourceStateDigest: sha256-prefixed-digest
  coordinationGateSnapshot:
    artifactRef: exact-snapshot-registration
    snapshotGeneration: positive-generation
    contentDigest: sha256-prefixed-digest
  requirementMapDigest: sha256-prefixed-complete-map-digest
  evidenceClosureDigest: sha256-prefixed-complete-closure-digest
~~~

These load-bearing digests have exact RFC 8785 JCS preimages:

~~~yaml
deliveryEvidenceClosureV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  deliveryRunId: exact-delivery-run
  entries:
    - role: accepted-scope-or-binding-source-or-requirement-map-or-test-or-evaluation-or-load-or-migration-or-generated-contract-or-gate-decision-or-coordination-gate-snapshot
      evidenceRef: exact-registration
      evidenceRevision: positive-revision
      contentDigest: sha256-prefixed-digest
      status: pass-or-approved-or-current

repositorySourceStateV1:
  schemaVersion: 1
  objectFormat: exact-format
  baseObjectId: full-approved-run-start-object-id
  headObjectId: full-object-id
  headTreeId: full-object-id
  indexTreeId: exact-head-tree-id
  worktreeState: clean
~~~

`requirementMapDigest` is SHA-256 of the exact stored RFC 8785 JCS bytes of `deliveryRequirementMapV1`, including its generation and generation-free `closureDigest`. `evidenceClosureDigest` is SHA-256 of exact `deliveryEvidenceClosureV1` bytes. `sourceStateDigest` is SHA-256 of exact `repositorySourceStateV1` bytes. Evidence-closure entries sort uniquely by role rank, `evidenceRef`, revision and content digest. Requirement-map binding sources sort uniquely by role rank, artifact ref and revision; requirements sort by `requirementId`; each requirement's evidence refs sort uniquely by evidence ref, revision and digest. The manifest equality-copies these three digests. Its requirement-map object and every evidence-closure entry must byte-equal the exact registered content they hash; there is no path/name-based substitution.

The manifest contains neither its own registration/digest nor the review-basis row/digest that will bind it; those are seal results, avoiding a hash cycle. It also contains no mutable/final RUN receipt reference. The immutable `delivery_run_start` row is created from approved launch/run authority before implementation and owns the Git base. For AFAB-004 it is `c2fc623a2529f87feca27982e1a140969ab5a258`; neither caller nor seal may replace it with a merge base, current ancestor or shorter range. A different delivery run uses its own approved immutable run-start row.

The current Fabric-owned deliveryRequirementMapV1, not mutable RUN.json or Markdown discovery, is the completeness root. Every binding requirement occurs exactly once. The resolved profile adds required scope/spec/ADR/decision sources and security/profile checks; the closure unions all referenced evidence plus completed test, evaluation, load, migration, generated-contract and coordination-snapshot evidence. Each resolves to exactly one active registration/revision/digest. Missing, duplicate, pending pre-review or failed evidence prevents seal; extra caller-selected evidence is impossible. Lists sort by their displayed keys and byte content de-duplicates only by digest.

The coordination snapshot is not a Fabric receipt:

~~~yaml
coordinationGateSnapshotV1:
  schemaVersion: 1
  artifactKind: coordination-gate-snapshot.v1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  deliveryRunId: exact-delivery-run
  snapshotGeneration: positive-generation
  eventWatermark: nonnegative-committed-sequence
  chair:
    agentId: exact-current-chair
    principalGeneration: positive-generation
    chairLeaseGeneration: positive-generation
    bridgeGeneration: positive-generation
  authorityDigest: sha256-prefixed-digest
  sessionRevision: positive-revision
  runRevision: positive-revision
  membershipRevision: positive-revision
  acceptedScopeDigest: sha256-prefixed-digest
  requirementMapDigest: sha256-prefixed-digest
  requiredGateRows:
    - gateId: exact-id
      gateRevision: positive-revision
      state: approved-or-not-applicable
      decisionDigest: sha256-prefixed-digest
      evidenceDigests: ordered-sha256-digests
      dependencyDigest: sha256-prefixed-digest
      blockedOperations: ordered-closed-operation-ids
  objectiveEvidenceRows:
    - role: test-or-evaluation-or-load-or-migration-or-generated-contract
      evidenceRef: exact-registration/revision
      status: pass
      contentDigest: sha256-prefixed-digest
  openPreReviewBlockers: empty
~~~

The artifact excludes every review target/route/action, slot head, review evidence/completion/recovery, RUN review section, human final acceptance/release, final Fabric receipt and volatile/in-flight checkpoint state. Phase A captures chair principal/lease/bridge/session lineage, run start, delivery RUN/scope/map, authority/gates/evidence/artifact revisions, profile and Git tokens. Outside SQLite, no-follow readers hash and validate the exhaustive closure and clean Git state, then create-exclusive, fsync and re-read the snapshot/manifest bytes. Phase B reauthenticates and equality-CASes every captured row/token, then atomically inserts snapshot, manifest registration with agent publication lineage, delivery review basis and command receipt. Producer kind is `fabric-seal`; publisher remains the authenticated chair. CAS failure inserts no database row; unreferenced CAS bytes are run-owned GC candidates.

Replay and stable single-flight key run/delivery-run before phase A. Exact replay returns immutable manifest/snapshot refs and digests, publication- lineage digest, review-basis revision/digest and repository source-state digest; changed input conflicts and different commands serialize. Source/head/index/ worktree, scope/map/profile, required evidence, gate/check, chair lineage or registration revision change makes the sealed basis logically stale. Only a new seal supersedes it; history is immutable. The closed artifact-kind catalogue therefore contains explicit delivery-requirement-map.v1, implementation-delivery-manifest.v1, coordination-gate-snapshot.v1 and discovery-surface.v1 entries. The first three are owned by `fabric-seal`; discovery-surface.v1 is owned only by the deployed-route daemon renderer. Eligibility is never inferred by parsing generic receipt JSON.

The current chair requests target preparation with `fabric.v1.review-target.prepare` under provider-review-evidence.v1. This public operation is durable asynchronous admission, not bundle construction. It performs only bounded authentication, current-row/profile-capability checks, idempotency classification, high-water reservation and SQLite writes, and returns an immutable accepted receipt before the 30-second protocol deadline. It performs no Git/evidence/CAS file read and no provider or network I/O. `expectedTargetGeneration` is `0` only when the run has no target and otherwise is the exact positive current generation. Zero with an existing target, positive with none, or a stale positive value conflicts.

~~~yaml
reviewTargetPrepareV1:
  schemaVersion: 1
  commandId: stable-command-id
  taskId: exact-current-review-task
  expectedTargetGeneration: zero-or-positive-current-generation
  deliveryManifestRef: exact-current-implementation-delivery-manifest.v1-revision
~~~

It accepts no summary, changed-file list, packet bytes, provider identity, route, prompt, profile override or lineage assertion.

The accepted result is closed:

~~~yaml
reviewTargetPreparationAcceptedV1:
  schemaVersion: 1
  preparationId: daemon-generated-stable-id
  ownerCommandId: first-admitted-command-id
  inputDigest: sha256-prefixed-digest-of-run/actor/full-request
  projectSessionId: exact-session
  coordinationRunId: exact-run
  taskId: exact-review-task
  expectedTargetGeneration: zero-or-positive-generation
  reservedTargetGeneration: positive-never-reused-high-water-generation
  reservedBundleGeneration: positive-never-reused-high-water-generation
  deliveryManifestRef: exact-artifact-revision
  state: prepared
  acceptedReceiptDigest: sha256-prefixed-canonical-result-digest
~~~

One active preparation may exist per run. The global command replay check runs first: exact replay returns its committed result and changed replay conflicts. For another command, the stable run-scoped semantic key is the canonical input digest with command ID omitted. The same digest joins the active preparation and records a command result pointing to the same accepted receipt; a different digest returns `REVIEW_TARGET_PREPARATION_CONFLICT`. No conflicting request invokes a filesystem reader or allocates a generation. The acceptance transaction increments `review_target_preparation_high_water`, inserts the immutable preparation with its complete captured database preconditions and reserves both target and bundle generations. A failed, conflicted or abandoned build never returns either generation to the allocator.

`fabric.v1.review-target-preparation.read` remains the per-ID public progress read. the Console read-identity contract's `fabric.v1.review-target-preparation.current.read` is the sole high-water locator and reuses this exact nested progress codec. The per-ID read accepts this closed request; no revision sentinel is needed for the first or later poll:

~~~yaml
reviewTargetPreparationReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  preparationId: exact-preparation

reviewTargetPreparationReadErrorV1:
  schemaVersion: 1
  code: REVIEW_TARGET_PREPARATION_NOT_FOUND-or-AUTHORITY_DENIED-or-SCOPE_MISMATCH-or-INTEGRITY_FAILURE
~~~

The daemon authenticates the caller's exact session/run scope and returns the accepted receipt plus exactly one current state:

~~~yaml
reviewTargetPreparationReadV1:
  schemaVersion: 1
  accepted: reviewTargetPreparationAcceptedV1
  revision: positive-CAS-revision
  state: prepared-or-building-or-built-or-succeeded-or-conflicted-or-failed
  phase: Preparing-or-Building-or-Committing-or-Succeeded-or-Conflicted-or-Failed
  progress:
    oneOf:
      - kind: phase-only
      - kind: finite
        unit: verified-build-items
        completed: nonnegative-integer
        total: positive-integer
        planDigest: sha256-prefixed-immutable-build-plan-digest
  terminal:
    oneOf:
      - null
      - kind: succeeded
        targetRef: exact-review-target-generation
      - kind: conflicted
        code: target-generation-changed-or-chair-binding-changed-or-task-or-authority-changed-or-delivery-basis-changed-or-repository-source-changed-or-profile-changed-or-predecessor-head-changed-or-predecessor-action-nonterminal
        evidenceDigest: sha256-prefixed-digest
      - kind: failed
        code: bundle-too-large-or-unsupported-repository-state-or-source-read-failed-or-content-integrity-failed-or-certifying-capability-unavailable
        evidenceDigest: sha256-prefixed-digest
~~~

`prepared`, `building` and `built` map respectively to Preparing, Building and Committing. The only state edges are `prepared -> building`, `building -> built|failed` and `built -> succeeded|conflicted|failed`; terminal states are immutable. `targetRef` is non-null exactly for succeeded. A terminal code and evidence digest exist only in their closed conflicted/failed arms. Nonterminal states require terminal null; each terminal state requires its same-kind arm. `phase-only` is always legal. A worker may select `finite` only after it has persisted the complete build plan: `total` and `planDigest` then remain immutable, `completed` is monotonic and advances only after each declared item is written, fsynced and re-read successfully. It cannot downgrade to phase- only. Built/succeeded require `completed=total` if finite was selected. The Console renders only the phase or exact `completed/total verified build items`, never a percentage or ETA.

A bounded daemon worker claims one preparation by a generation-bearing lease, then resolves the manifest to one sealed delivery review basis. That basis binds the delivery run, accepted scope, requirement/acceptance mapping, required pre-review checks and evidence, and a clean canonical Git state: repository/object format, base object, head object, head tree, index tree, worktree-clean marker and their canonical source-state digest. Review evidence itself does not advance the review-basis revision. Any source, base/head, index/worktree, accepted-scope, required-check, evaluation, load or pre-review evidence change does.

From the trusted Git and evidence services, that worker constructs the complete closed review-bundle.v1. The caller cannot omit an entry. The manifest contains:

~~~yaml
reviewBundleBodyV1:
  schemaVersion: 1
  bundleGeneration: positive-run-generation
  delivery:
    deliveryRunId: exact-run
    reviewBasisRevision: positive-revision
    reviewBasisDigest: sha256-prefixed-digest
    deliveryManifestRef: exact-eligible-artifact
    deliveryManifestObjectDigest: sha256-prefixed-bundle-object
    deliveryRequirementMapObjectDigest: sha256-prefixed-bundle-object
  repository:
    objectFormat: exact-registered-format
    baseObjectId: full-object-id
    headObjectId: full-object-id
    headTreeId: full-object-id
    indexTreeId: exact-head-tree-id
    worktreeState: clean
    sourceStateDigest: sha256-prefixed-digest
    reviewDiffCodecDigest: sha256-prefixed-checked-in-codec-digest
    reviewDiffRulesDigest: sha256-prefixed-checked-in-rules-digest
    reviewDiffSetDigest: sha256-prefixed-complete-diff-set-digest
  changedFiles:
    - ordinal: contiguous-zero-based-integer
      path: canonical-relative-path
      status: added-or-modified-or-deleted-or-renamed-or-mode-changed
      oldPath: null-or-canonical-relative-path
      beforeMode: null-or-100644-or-100755-or-120000-or-160000
      afterMode: null-or-100644-or-100755-or-120000-or-160000
      beforeObjectDigest: null-or-sha256-prefixed-bundle-object
      afterObjectDigest: null-or-sha256-prefixed-bundle-object
      diffObjectDigest: sha256-prefixed-bundle-object
  requiredEvidence:
    - ordinal: contiguous-zero-based-integer
      role: delivery-manifest-or-delivery-requirement-map-or-accepted-scope-or-spec-or-adr-or-decision-or-test-or-evaluation-or-load-or-migration-or-generated-contract-or-coordination-gate-snapshot
      evidenceRef: exact-current-registration
      evidenceRevision: positive-revision
      registeredContentDigest: sha256-prefixed-digest
      objectDigest: sha256-prefixed-bundle-object
  carriedFindingSet:
    findingSetDigest: sha256-prefixed-reviewFindingSetV1-digest
    findingCount: nonnegative-integer
    pages:
      - ordinal: contiguous-zero-based-integer
        pageDigest: sha256-prefixed-reviewFindingPageV1-digest
        memberCount: positive-integer
        firstFindingDigest: sha256-prefixed-digest
        lastFindingDigest: sha256-prefixed-digest
  objects:
    - ordinal: contiguous-zero-based-integer
      objectDigest: sha256-prefixed-exact-object-bytes
      mediaType: reviewBundleMediaTypeV1
      byteLength: nonnegative-integer
      chunkDigests: ordered-sha256-digests
  bundleSearchIndexDigest: sha256-prefixed-immutable-index
  riskReadMapDigest: sha256-prefixed-checked-in-rules/output
  coverageDigest: sha256-prefixed-canonical-delivery-repository-coverage-digest

reviewFindingPageV1:
  schemaVersion: 1
  members: ordered-nonempty-safeFinding-records

reviewFindingSetV1:
  schemaVersion: 1
  findingCount: nonnegative-integer
  pages:
    - ordinal: contiguous-zero-based-integer
      pageDigest: sha256-prefixed-exact-reviewFindingPageV1-bytes
      memberCount: positive-integer
      firstFindingDigest: sha256-prefixed-digest
      lastFindingDigest: sha256-prefixed-digest

reviewBundleRootV1:
  schemaVersion: 1
  bodyMediaType: application/vnd.agent-fabric.review-bundle-body.v1+json
  bodyByteLength: positive-bounded-count
  manifestBodyDigest: sha256-prefixed-exact-reviewBundleBodyV1-bytes
  coverageDigest: sha256-prefixed-body-equal-digest
  pages:
    - ordinal: contiguous-zero-based-integer
      pageDigest: sha256-prefixed-exact-page-bytes
      byteLength: positive-count-at-most-65536

reviewBundleRefV1:
  schemaVersion: 1
  bundleGeneration: positive-run-generation
  manifestBodyDigest: sha256-prefixed-digest
  manifestRootDigest: sha256-prefixed-exact-reviewBundleRootV1-bytes
  coverageDigest: sha256-prefixed-body/root-equal-digest
  bundleSearchIndexDigest: sha256-prefixed-body-equal-digest
  riskReadMapDigest: sha256-prefixed-body-equal-digest
  mandatoryReadSetDigest: sha256-prefixed-root/pages/specs/gates/findings/risk-sample-set
  mandatoryReadCount: positive-bounded-count
  mandatoryReadBytes: positive-bounded-canonical-wire-bytes
~~~

`coverageDigest` is SHA-256 of RFC 8785 JCS of `reviewBundleCoverageV1 {schemaVersion:1, repository:{objectFormat, baseObjectId,headObjectId,reviewDiffCodecDigest,reviewDiffRulesDigest, reviewDiffSetDigest}, changedFiles:[the complete body changed-file records], requiredEvidence:[the complete body evidence records], carriedFindingSet:the complete body finding-set ref, objects:[the complete body object records], bundleSearchIndexDigest,riskReadMapDigest}`. It contains no coverage, body, page, root, mandatory-set or bundle digest. `mandatoryReadSetDigest` is SHA-256 of RFC 8785 JCS of `mandatoryReadSetV1 {schemaVersion:1,entries:[{kind,ordinal,parentDigest, payloadDigest}]}`. Kinds are exactly `manifest-root`, `manifest-body-page`, `delivery-manifest`, `delivery-requirement-map`, `required-evidence`, `finding-set`, `finding-page` and `risk-sample-chunk`; nullable parent digest is null only for the root. Entries sort uniquely by that rank, parent digest, ordinal and payload digest.

Every body array uses contiguous zero-based ordinals. Changed files additionally obey the review-diff order; required evidence sorts by role rank, evidence ref, revision and object digest; finding pages sort by ordinal with strictly increasing, nonoverlapping first/last finding-digest ranges; objects sort by object digest and their ordinal must equal that position. A required evidence `registeredContentDigest` equals its `objectDigest`. The delivery-manifest and delivery-requirement-map object digests equal the exact manifest/map registered content digests, including `requirementMapDigest`; repository source state equals the manifest's exact `sourceStateDigest`. Coverage/root/ref equality copies are byte-for-byte, not semantically re-derived variants.

Checked-in golden fixtures freeze each JCS preimage and SHA-256 result plus body-array permutation negatives. Reordering, duplicated ordinals, duplicate keys, equal members in different pages, changed media type or any copied-digest mismatch fails before target commit.

The coordination-gate-snapshot role is the exact immutable artifact defined above. `fabric-receipt.json` is never an input to a review bundle; its later export cannot advance or stale the delivery basis or target.

Changed-file coverage is the complete sorted base-to-head Git change set. Added, deleted, renamed and modified paths include the exact applicable before, after and deterministic diff objects. Required-evidence coverage is the complete sorted set derived from the sealed delivery review basis. The daemon rejects a duplicate, omission, unexpected entry, dirty index/worktree, unsupported Git state, unavailable object or evidence revision, or any coverage-digest mismatch. It never substitutes a prose summary or truncated diff.

`review-diff.v1` is the sole changed-file codec. Its checked-in schema, canonical rules document and conformance fixture are digest-bound by the activated review-bundle contract. It reads the two exact committed trees by full object ID with system/global/repository diff configuration disabled; it does not consume porcelain text, mutable rename settings or working-tree bytes. A path is its exact valid UTF-8 Git-tree byte spelling with `/` separators, no leading/trailing slash, empty/`.`/`..` component, NUL, C0/C1 control, Unicode normalisation or case folding. An unrepresentable path makes the delivery `unsupported-repository-state` before target commit.

The admitted tree modes are regular `100644`, executable `100755`, symlink `120000` and gitlink `160000`. Blob and symlink source bytes come from the Git object database without following a link; a gitlink source is the exact full object ID encoded as lowercase ASCII. Exact rename detection is codec-owned: after same-path comparison, deleted and added entries with equal source-object bytes are grouped by object digest, sorted by old then new UTF-8 path bytes and paired by ordinal. It performs no similarity heuristic. Remaining entries are added or deleted. A same-path byte change is `modified`; a same-path mode-only change is `mode-changed`; a paired path change is `renamed`, including any mode change. Each arm requires exact before/after object and mode nullability. This precedence gives every tree delta exactly one arm.

The exact diff object bytes are RFC 8785 JCS of this closed union:

~~~yaml
reviewDiffObjectV1:
  schemaVersion: 1
  status: added-or-modified-or-deleted-or-renamed-or-mode-changed
  path: exact-new-or-deleted-path
  oldPath: nonnull-only-for-renamed
  before:
    oneOf: [null, {mode: exact-git-mode, objectDigest: sha256-prefixed-digest,
      byteLength: nonnegative-integer}]
  after:
    oneOf: [null, {mode: exact-git-mode, objectDigest: sha256-prefixed-digest,
      byteLength: nonnegative-integer}]
  payload:
    oneOf:
      - kind: text-edits
        operations:
          - kind: equal-or-delete-or-insert
            oldStart: nonnegative-line-index
            oldCount: nonnegative-line-count
            newStart: nonnegative-line-index
            newCount: nonnegative-line-count
            segmentDigest: sha256-prefixed-exact-segment-bytes
      - kind: binary-summary
        beforeDigest: null-or-sha256-prefixed-digest
        afterDigest: null-or-sha256-prefixed-digest
        beforeBytes: null-or-nonnegative-integer
        afterBytes: null-or-nonnegative-integer
~~~

Text means a regular blob that is valid strict UTF-8, contains no NUL and is within the existing per-object bound. Lines retain their LF byte; a final unterminated line remains one line. The checked-in algorithm is Myers shortest-edit over exact line bytes with delete-before-insert, then lowest old index, then lowest new index as tie-breaks. Maximal adjacent operations of one kind are coalesced; zero-length operations are forbidden. `segmentDigest` hashes the exact concatenated old bytes for equal/delete and new bytes for insert. Added/deleted/mode-only text uses the same normal form. Every other mode/content uses `binary-summary`. `diffObjectDigest` is SHA-256 of the exact JCS bytes above.

The changed-file array sorts by new/deleted `path` UTF-8 bytes, then status rank `added, modified, deleted, renamed, mode-changed`, then nullable `oldPath`; it is unique by that tuple. `reviewDiffSetDigest` hashes RFC 8785 JCS of `{schemaVersion:1,objectFormat,baseObjectId,headObjectId,codecDigest,rulesDigest, entries:[the complete ordered changed-file records]}`. No timestamp, Git version, command output or host path enters a digest domain.

The immutable `review-diff-fixture.v1` manifest binds full base/head object IDs, object format, codec/rules/source-object-set digests and exact expected change count, unique object count, total unique object bytes, largest object bytes and diff-set digest. A fixture is regenerated only by intentionally selecting a new immutable base/head pair and reviewing the changed manifest; the final delivery gate never reuses its counts. For sizing only, the pre-codec enumeration of `c2fc623a2529f87feca27982e1a140969ab5a258..0a04d161c5d4fa027c96410b3cc0cf887e1c6e42` observed 601 changes, 1,434 unique objects, 27,766,213 total bytes and a 4,097,314-byte largest object. Those numbers are not a codec oracle or final- HEAD acceptance threshold. Each target recomputes the dynamic approved run-start-to-current-head set and applies only the stated count/byte ceilings.

`carriedFindingSet` preserves every complete immutable safe record from the predecessor slot heads. The set root and all pages are content-addressed bundle objects and mandatory reads; an opaque digest, count-only summary or truncated prefix is insufficient. A page contains as many whole ordered records as fit the 65,536-byte object bound, never splits a record, and is immutable. Set/page digests use exact RFC 8785 JCS bytes with the digest stored only by the parent. Every provider P0-P2 finding is repair-required automatically. Chair annotation cannot remove, downgrade or make one same-target-resolvable.

Repair currency is exact per finding. Every repair requires a later delivery- manifest revision, different delivery-review-basis digest and different bundle digest. `repository-source` additionally requires a changed repository source- state digest. `registered-evidence` requires every named origin evidence ref to remain the same registration identity with a greater revision and changed content digest; a Git-only change is insufficient. `mixed` requires both sets of predicates. Missing, replaced, lower/equal or same-content evidence never counts as repair. Repreparing identical bytes cannot make a finding resolvable.

Each nonempty object is split deterministically into 65,536-byte ordered content-addressed chunks except for one final chunk of 1 through 65,536 bytes; an empty object has no chunks. Object digest is over exact source bytes, and each chunk digest is over its exact bytes. Digest construction is acyclic and uses these exact domains:

1. RFC 8785 JCS of reviewBundleBodyV1 contains no body/page/root/bundle digest
   or page list. `manifestBodyDigest` is SHA-256 of those exact UTF-8 bytes.
2. Those already-hashed body bytes are split into consecutive 65,536-byte
   ranges (one final shorter range); each page digest hashes only its exact raw    range. Concatenating pages in ordinal order must reproduce the body bytes.
3. RFC 8785 JCS of reviewBundleRootV1 binds body digest/length, coverage digest
   and the complete ordinal/page-digest/length vector.    `manifestRootDigest` is SHA-256 of those exact root bytes and is absent from    the root itself.
4. RFC 8785 JCS of reviewBundleRefV1 binds generation, body/root/coverage,
   search/risk and mandatory-set/count/byte values. `bundleDigest` is SHA-256    of those exact ref bytes and is stored beside, never inside, the ref.

Before step 4, mandatory wire reservation uses the generated closed response templates, fixed unescaped 71-byte lowercase `sha256:` placeholders and the maximum permitted 64-byte string JSON-RPC ID sentinel. After the bundle digest is known, every response is materialised with that sentinel and may not exceed the reserved count before commit; mismatch fails rather than iterating a digest cycle. Runtime debit uses the actual admitted ID and exact response bytes.

No digest domain contains itself or a later digest. The canonical body uses at most 16 immutable pages/1 MiB; the root is at most 49,152 bytes. V1 limits are 4,096 changed paths, 1,024 required-evidence entries, 16,384 unique objects, 32,768 chunks, 16 MiB per object, 64 MiB total unique object bytes, 1 MiB total manifest-page bytes and 4 MiB for the immutable search index. The checked-in risk-map output is at most 256 KiB. Repeated references to one object digest count its bytes once while every manifest reference remains present. Finding set/page objects count normally against object/byte and mandatory-read ceilings; no inline duplicate or fixed finding-count cap exists. Each safe ID is at most 64 UTF-8 bytes, summary 256 and evidence 768. Certification requires the root, every body page, finding-set root/page, delivery manifest/map objects and all required accepted- scope/spec/ADR/decision/gate- decision/coordination-snapshot objects. That mandatory set is limited to 80 complete reads and 6 MiB exact canonical wire bytes; target preparation rejects a delivery exceeding either bound. Complete changed-file diffs, before/after objects, checks, evaluations, load evidence and generated contracts remain available but are not all byte-mandatory for a large delivery.

The daemon builds a content-addressed bundle-search.v1 index and applies the checked-in review-risk-map.v1 to the complete manifest. Closed nonempty groups are security/auth, protocol/schema, persistence/migration, provider/adapter, Console/UI, tests/evaluations, documentation and generated/other. The checked- in rules score by evidence kind/path/operation sensitivity, sort by descending score then canonical path/digest, and select exact diff chunks from the highest- risk changed objects in every nonempty group. The combined deterministic sample is at most 32 chunks/2 MiB and joins the mandatory read set; caller/provider cannot choose it. Target prepare fails if it cannot form one sample per group inside the bound.

The action-bound portal additionally accepts only literal substring or token search over that index: at most 16 search calls and 1 MiB aggregate search- response bytes per action, with at most 256 query bytes, 100 results, 65,536 result bytes and 250 ms CPU per call. It returns exact object digests/offsets/snippets plus a result digest. Search has no regex, live path or caller-selected index. Search supports deeper exploration but does not replace the mandatory deterministic sample. The target stores the exact risk/sample map and achievable read/byte budgets. Exceeding a limit is bundle-too-large and requires a smaller complete delivery, not a partial bundle.

Finding capacity is admitted independently before every review action. A normal action reserves durable set/page/object and byte capacity for all 32 possible safe findings plus the resulting open/repair set roots before any router, portal or provider I/O. The reservation belongs to the action pair and is settled atomically with terminal result; unused capacity releases. If the complete current set plus that maximum cannot fit the physical private-store, bundle-object or mandatory-read ceilings, admission performs zero router/ provider I/O, creates no action/budget row and returns `FINDING_CAPACITY_EXHAUSTED`. Completion exposes the target-wide `finding-capacity-exhausted` blocker.

While that blocker is current, the chair may dispatch only a bounded `resolution-only` recovery window against an existing open set. It reserves no new-finding bytes, admits at most 32 prior finding digests, requires the result to contain zero new findings and is permanently noncertifying even if it resolves all named digests. A response with a new finding is unusable and resolves nothing. Resolution-only may shrink open/repair sets until a later normal 32-finding reservation succeeds; final completion still requires a fresh normal certifying CLEAN. Exhaustion of the minimum root/page storage needed to record even resolution-only state is a genuine typed operator gate for private- store capacity remediation. Fabric never drops, overwrites or evicts a finding to clear it, and run-owned GC may remove only unreferenced objects.

Each target has one logical bundle and one manifest root. Pages/chunks are internal addressing only; CAS content may be reused across targets, but no bundle chain, parent bundle or partial successor exists. The body transitively binds the complete object set and search/risk digests; the root binds that body and every page; the final ref binds root/search/risk/mandatory values. Thus `bundleDigest` covers every component without a self-reference and none can be substituted independently.

Manifest, objects and chunks are written create-exclusive beneath the daemon-private content-addressed review store, fsynced, re-read and digest verified before target commit. Existing same-digest content is byte-verified. A collision is an integrity failure. The two-phase no-follow source checks from the bounded artifact-read contract apply, and the delivery basis, Git state, registrations and publication lineage are rechecked in the target transaction.

The action-pair-only review-bundle.portal.v1 is exactly one MCP stdio server named `agent-fabric-review-bundle`. Discovery returns exactly two tools, `review_bundle_read` and `review_bundle_search`, and zero resources, resource templates, prompts or other tools. `review_bundle_read` names the manifest root, one listed page, or exact object and chunk digests; `review_bundle_search` takes only the closed literal/token query and bounds above. The action capability is out-of-band and binds both tools to one action, target, bundle, coverage digest and expiry. The daemon verifies the complete parent chain.

~~~yaml
reviewBundleMediaTypeV1: [application/octet-stream,
  application/vnd.agent-fabric.review-bundle-root.v1+json,
  application/vnd.agent-fabric.review-bundle-body.v1+json,
  application/vnd.agent-fabric.review-diff.v1+json,
  application/vnd.agent-fabric.review-finding-page.v1+json,
  application/vnd.agent-fabric.review-finding-set.v1+json]

reviewBundleReadArgsV1:
  schemaVersion: 1
  bundleDigest: exact-action-bundle-digest
  kind: manifest-root-or-manifest-body-page-or-object-or-chunk
  parentDigest: null-or-exact-listed-parent-digest
  payloadDigest: exact-listed-payload-digest
  ordinal: nonnegative-listed-ordinal

reviewBundleReadResultV1:
  schemaVersion: 1
  bundleDigest: exact-action-bundle-digest
  kind: manifest-root-or-manifest-body-page-or-object-or-chunk
  parentDigest: null-or-exact-listed-parent-digest
  payloadDigest: exact-listed-payload-digest
  ordinal: nonnegative-listed-ordinal
  offset: nonnegative-byte-offset
  rawByteLength: nonnegative-integer-at-most-65536
  mediaType: reviewBundleMediaTypeV1
  encoding: base64
  payload: RFC4648-padded-base64
  resultDigest: sha256-prefixed-digest

reviewBundleSearchArgsV1:
  schemaVersion: 1
  bundleDigest: exact-action-bundle-digest
  queryKind: literal-or-token
  query: UTF8-1-through-256-bytes
  maximumResults: integer-1-through-100

reviewBundleSearchResultV1:
  schemaVersion: 1
  bundleDigest: exact-action-bundle-digest
  entries:
    - objectDigest: exact-bundle-object-digest
      offset: nonnegative-byte-offset
      rawByteLength: positive-integer
      encoding: base64
      snippet: RFC4648-padded-base64
  resultDigest: sha256-prefixed-digest

reviewBundlePortalErrorV1:
  schemaVersion: 1
  code: INVALID_REQUEST-or-UNAUTHENTICATED-or-BUNDLE_MISMATCH-or-NOT_LISTED-or-CROSS_BUNDLE-or-BUDGET_EXHAUSTED-or-RESULT_TOO_LARGE-or-INTEGRITY_FAILURE
  evidenceDigest: null-or-sha256-prefixed-digest
~~~

The read root alone requires null parent and ordinal zero; every other read requires the exact listed parent/ordinal. Search entries sort uniquely by object digest, offset and raw length. Each result digest hashes RFC 8785 JCS of the complete result with only `resultDigest` omitted. Tools accept only these arguments and return only these results or the closed error. Unknown fields, unlisted media types and snippets beyond the separate search limit reject.

Every portal request is exactly one UTF-8 JSON object followed by one LF. BOM, CRLF, JSON batch, trailing bytes, duplicate object keys and non-object roots are invalid. JSON-RPC `id` is either an integer in `0..2147483647` or an ASCII string matching `^[A-Za-z0-9._:-]{1,64}$`; every other ID is rejected before a tool or ledger effect. The TypeScript broker is the sole JSON/JSON-RPC/MCP parser. A successful or error response is RFC 8785 JCS of exactly one closed JSON object followed by LF and repeats the exact admitted ID.

`review_bundle_read` uses RFC 4648 padded base64 for one exact raw root/page/ chunk payload. A closed result includes schema version, bundle/kind/parent/ payload digests, ordinal/offset/raw length, `encoding: base64`, payload and result digest. That digest hashes RFC 8785 JCS of the closed tool result with only `resultDigest` omitted. Raw payload is at most 65,536 bytes, so base64 is at most 87,384 ASCII bytes. There is no independent prose metadata allowance. Checked-in generated request/result templates enumerate every field, maximum value and escaping rule and prove the complete response, including JSON-RPC envelope and LF, is at most 98,304 bytes.

Target preparation reserves each mandatory response from the generated template instantiated with the maximum 64-byte string ID sentinel. Once the bundle digest is known it materialises and checks every reserved response. At runtime the ledger debits the exact canonical response bytes for the actual admitted ID; that value may not exceed the reservation. Direct Claude/Codex dynamic-tool calls use the same equivalent JSON-RPC charge: the adapter exposes its allowed correlation ID or Fabric assigns a deterministic action-local integer ID, then charges the identical canonical envelope. No transport gets a zero-cost or estimated-byte path. Mandatory/exploration ledgers count those complete bytes, not decoded payload. Search keeps its separate 65,536-byte response ceiling. Exact-bound generated fixtures cover both ID forms, a 64-byte ID, full 65,536-byte binary chunk, full body page, maximum root, empty object, every error and maximum search result before capability activation. Neither tool accepts an arbitrary command, caller path/root, glob, URL, server name or mutable cursor. Cross-bundle, reordered, missing or substituted pages/chunks fail closed.

The MCP method allowlist is initialize/initialized, ping, tools/list and tools/call for those exact tools. resources/list, resources/templates/list and prompts/list are permitted and return exact empty arrays because clients may probe them. resources/read, subscribe/unsubscribe, prompts/get and all sampling, roots, completion, elicitation and logging methods are denied. Unknown methods fail closed without provider/source effect.

The adapter prompt is a bounded envelope containing the fixed review rubric, review instruction, target generation, slot, prior open-finding digests, bundle/coverage/profile digests and portal contract. It contains no caller-selected source summary. It binds the complete carried-finding set and, where bounded, includes its safe text; otherwise those records are mandatory reads. Its exact UTF-8 bytes remain at most 65,536 and its digest is action-bound; bundle content remains behind the portal, so source size cannot overflow the prompt. The strict provider result repeats the target generation and coverage digest. A partial or stale summary therefore cannot masquerade as complete bundle review, and every source read is digest-bound. The portal journal must prove the mandatory set including every deterministic risk sample, then derives readCoverageDigest and one coverage summary. Zero or insufficient mandatory reads are noncertifying insufficient-read-coverage. A syntactic CLEAN becomes public UNUSABLE and resolves nothing; a safely parsed FINDINGS remains visible/noncertifying, resolves nothing and adds every new P0-P2 to open repair-required state. Provider repetition of coverageDigest is not consumption proof. Complete means complete bundle availability and manifest awareness, not byte-for-byte review. The daemon declares every unread object group/count/set digest as a coverage gap; neither provider nor chair can hide it.

The committed target is:

~~~yaml
reviewSubjectV1:
  schemaVersion: 1
  taskId: exact-review-task
  reviewedArtifactRef: exact-eligible-delivery-manifest
  publicationLineageDigest: exact-registration-snapshot
  deliveryReviewBasisRevision: positive-revision
  deliveryReviewBasisDigest: sha256-prefixed-digest
  repositorySourceStateDigest: sha256-prefixed-digest
  reviewBundleBinding: exact-reviewBundleBinding-object-below
  completionProfile: exact-completionProfile-object-below

reviewTargetV1:
  schemaVersion: 1
  preparationId: exact-succeeded-preparation
  targetGeneration: positive-run-CAS-generation
  reviewSubjectDigest: sha256-prefixed-reviewSubjectV1-digest
  taskId: exact-review-task
  reviewedArtifactRef: exact-eligible-delivery-manifest
  publicationLineageDigest: exact-registration-snapshot
  deliveryReviewBasisRevision: positive-revision
  deliveryReviewBasisDigest: sha256-prefixed-digest
  repositorySourceStateDigest: sha256-prefixed-digest
  reviewBundleBinding:
    bundleGeneration: positive-generation
    bundleDigest: sha256-prefixed-digest
    manifestBodyDigest: sha256-prefixed-digest
    manifestRootDigest: sha256-prefixed-digest
    coverageDigest: sha256-prefixed-digest
    bundleSearchIndexDigest: sha256-prefixed-digest
    riskReadMapDigest: sha256-prefixed-digest
    mandatoryReadSetDigest: sha256-prefixed-digest
    mandatoryReadCount: positive-count-at-most-80
    mandatoryReadBytes: positive-count-at-most-6291456
    objectCount: bounded-count
    chunkCount: bounded-count
    totalObjectBytes: bounded-count
  initialChairBindingGeneration: 1
  initialChairBindingDigest: sha256-prefixed-reviewTargetChairBindingV1-digest
  completionProfile:
    profileId: certifying-review-four-slot-v1
    profileSchemaDigest: sha256-prefixed-checked-in-schema-profile-catalogue
    resolvedProfileDigest: sha256-prefixed-target-snapshot
    slots: exact-four-resolved-slots
~~~

`reviewSubjectDigest` is SHA-256 of RFC 8785 JCS of exactly `reviewSubjectV1`. It excludes preparation/target generations and every mutable chair-binding field. Every displayed subject field in `reviewTargetV1` equality- copies that preimage. Checked-in golden and field-permutation vectors cover the complete preimage; omission, extra field, changed nested bundle/profile value or a target equality-copy mismatch fails before target commit or rebind.

`reviewBundleBinding` is a target projection, not the hashed reviewBundleRefV1 document: `bundleDigest` hashes the exact stored ref bytes; the remaining digest/budget fields equality-copy that ref and counts equality- derive from its bound body.

Chair custody is not part of the immutable review subject. Target commit also appends the initial generation-one binding and installs its active pointer:

~~~yaml
reviewTargetChairBindingV1:
  schemaVersion: 1
  targetGeneration: exact-target
  bindingGeneration: positive-contiguous-generation
  predecessorBindingGeneration: null-for-one-otherwise-exact-prior
  predecessorBindingDigest: null-for-one-otherwise-exact-prior-digest
  predecessorCertificationCutSequence: null-for-one-otherwise-nonnegative-sequence
  predecessorCertificationCutDigest: null-for-one-otherwise-exact-cut-digest
  predecessorCertificationCutCustodyRef: null | lifecycleCustodyRefV1
  agentId: exact-same-chair-agent
  principalGeneration: positive-generation
  chairLeaseGeneration: positive-generation
  providerSessionGeneration: positive-generation
  bridgeGeneration: positive-generation
  adapterId: exact-adapter
  adapterContractDigest: sha256-prefixed-digest
  modelFamily: openai-or-anthropic
  model: exact-model
  routeReceiptDigest: null-or-sha256-prefixed-digest
  profileDigest: exact-target-profile-digest
  taskId: exact-target-task
  reviewedArtifactRef: exact-target-artifact
  deliveryReviewBasisDigest: exact-target-basis-digest
  repositorySourceStateDigest: exact-target-source-digest
  bundleDigest: exact-target-bundle-digest
  lifecycleCustodyRef: null | lifecycleCustodyRefV1
  checkpointDigest: null-for-one-otherwise-exact-custody-checkpoint
  lifecycleAdoptionEvidenceDigest: null-for-one-otherwise-sha256-prefixed-digest
  bindingDigest: sha256-prefixed-canonical-binding-digest

reviewCertificationCutV1:
  schemaVersion: 1
  runId: exact-run
  targetGeneration: exact-target
  predecessorBindingGeneration: exact-active-binding-at-adoption
  predecessorBindingDigest: exact-active-binding-digest
  terminalSequenceHighWater: nonnegative-run-sequence
  lifecycleCustodyRef: lifecycleCustodyRefV1
  lifecycleAdoptionEvidenceDigest: sha256-prefixed-digest
  cutDigest: sha256-prefixed-canonical-cut-digest

reviewTargetChairBindingPointerV1:
  targetGeneration: exact-target
  activeBindingGeneration: positive-generation
  revision: positive-CAS-revision
~~~

Every nonnull custody ref above uses the exact closed `lifecycleCustodyRefV1`; its run equals the cut/target run, its agent equals the chair binding agent and its revision identifies the immutable finalized `adopted` custody row. Crossed agent, custody ID or revision rejects.

The public idempotent execution/replay surface for this transition is:

~~~yaml
reviewTargetRebindV1:
  schemaVersion: 1
  commandId: stable-command-id
  targetGeneration: exact-current-target
  expectedChairBindingGeneration: exact-current-active-binding
  lifecycleCustodyRef: lifecycleCustodyRefV1

reviewTargetRebindReceiptV1:
  schemaVersion: 1
  status: rebound
  targetGeneration: unchanged-exact-target
  reviewSubjectDigest: unchanged-exact-subject-digest
  priorBindingGeneration: exact-requested-generation
  newBindingGeneration: prior-plus-one
  priorBindingDigest: exact-prior-digest
  newBindingDigest: exact-new-digest
  lifecycleAdoptionDigest: exact-adoption-digest
  bundleDigest: unchanged-exact-bundle-digest
  profileDigest: unchanged-exact-profile-digest
  slotHeadSetDigest: unchanged-exact-four-head-set-digest
  openAndRepairFindingSetDigest: unchanged-exact-set-digest
  rebindReceiptDigest: sha256-prefixed-canonical-receipt-digest
~~~

`fabric.v1.review-target.rebind` accepts no family, model, profile, source, bundle, head, evidence or lineage claim. It derives all of them from the target, active pointer and exact finalized adopted custody. Admission requires the same chair agent; exact custody source tuple equal to the prior binding; exact custody successor tuple equal to the current chair; and unchanged adapter, contract, family, model, task, artifact, review basis, repository source, bundle/profile and four head/open/repair tuples. It performs no router, provider, portal or lookup I/O. Wrong agent, non-adopted custody, changed immutable subject, crossed generation or pointer/head CAS fails without mutation and requires a fresh target preparation where applicable.

True-chair lifecycle adoption invokes the same deterministic mutation inside its serialization transaction when these predicates hold. A later exact public request or daemon retry returns the already-committed receipt for that target/ custody semantic key; a changed command replay conflicts. If adoption left the target stale, the operation may append the binding only while the exact prior pointer and all immutable/head predicates still match. Multiple rotations form one contiguous digest chain with no reused generation or ABA.

Binding rows are append-only and immutable; only the one active pointer may advance by contiguous compare-and-set. Generation one snapshots the chair used by preparation Phase B and has all predecessor/cut fields null. A later binding is legal only when one finalized `adopted` lifecycle custody proves the same `agentId`, an unbroken predecessor binding, and exact equality of adapter, contract, family, model, profile, task, artifact, basis, source and bundle. Only principal, chair-lease, provider-session, bridge and route-receipt generations may advance. Its predecessor binding digest and predecessor certification-cut custody/sequence/digest are nonnull and exact.

Every first terminalisation of a certifying provider action atomically reserves one stable positive `terminalSequence` from the run high-water; replay returns that sequence. At the same serialization point as each true-chair lifecycle adoption, the daemon snapshots the current terminal-sequence high-water into one exact `reviewCertificationCutV1` keyed by the adopting lifecycle custody. A stale target may therefore accumulate multiple distinct custody-keyed cuts for the same target and predecessor binding. The unique cut digest prevents duplicate identity without collapsing those adoptions. If the same-subject predicates above hold, that adoption transaction appends the successor binding, equality-binds the exact custody/cut, copies the cut sequence/digest into its predecessor fields and advances the pointer. A successor can never cite a cut from another custody. Review state never waits, rejects, rolls back or creates a human gate for adoption. If any predicate does not hold, lifecycle adoption still succeeds and the unchanged target becomes read-derived stale.

An action retains its dispatch binding generation. Evidence under binding `b` uses normal current predicates while `b` is active and has no successor. Once a successor exists, that evidence can certify only when its stable terminal sequence is no greater than the first successor's predecessor cut and every binding from `b` to the active binding is contiguous and digest-valid. A post- cut terminal is permanently noncertifying, accepts no resolutions, still settles and retains every adverse safe finding. Prepared old-binding actions cannot pass their worker currency check and the ordinary recovery owner closes them proved no-effect; dispatched, accepted and ambiguous actions recover normally without delaying lifecycle. Target, heads, existing evidence IDs, bundle and findings are never rewritten by binding advance.

`cutDigest` is SHA-256 of RFC 8785 JCS of the complete `reviewCertificationCutV1` with `cutDigest` omitted. `bindingDigest` is SHA-256 of RFC 8785 JCS of every displayed binding field with only `bindingDigest` omitted, including predecessor binding/cut fields. Public evidence and completion expose the closed `reviewCertificationBasis` arm, not an inferred watermark.

Preparation and lifecycle rotation serialize at commit, not by a long-lived lock. Preparation Phase B rechecks the adopted current chair. It may commit the initial binding against a same-agent adopted generation only when every non-generation binding field and all source/profile preconditions remain exact; otherwise it terminalises the preparation as conflicted. A rotation that adopts after target commit performs the automatic cut/rebind-or-stale decision above. Existing effect ambiguity remains owned by route recovery; a preparation may remain built/Committing while that fence exists and Phase B cannot leapfrog it. A changed predecessor action tuple conflicts as `predecessor-action-nonterminal`. No transaction exposes a target with a missing active binding.

The succeeded preparation Phase B atomically inserts its reserved bundle metadata, supersedes the former target, creates the immutable target, initial chair binding/pointer and four slot heads, then commits the preparation `built -> succeeded` transition. A CAS mismatch commits no target and marks the preparation conflicted with evidence. Exact prepare-command replay continues to return its immutable accepted receipt; progress/target are read separately. A delivery review-basis/source-state advance, root artifact advance, an unrebindable chair change, or any resolved adapter-contract/profile change makes the current target stale without mutation. A dispatch or annotation mutation rejects it without changing target state. The action-bound terminal evidence transaction always settles/persists even when currency changed. Only a newly succeeded preparation Phase B inserts a successor and persists the prior row as superseded. No read advances global revision.

`ReviewTargetPreparationRecoveryService` runs before review CAS garbage collection and generic job recovery. It reclaims an expired worker lease by incrementing the same preparation's worker-claim generation; it never allocates another target/bundle generation or inserts a duplicate target. `prepared` restarts at build. `building` revalidates/reuses only digest-verified create-exclusive CAS bytes and continues the same build; `built` reruns only Phase B. If captured state changed, recovery commits conflicted/failed rather than silently rebuilding against another source. CAS garbage collection may delete bytes only after proving no active preparation, target or bundle row references their digest. Crash tests at every file write, fsync, state edge and Phase-B statement expose either the same resumable preparation or one complete target, never a partial/duplicate target.
