# Agent Fabric lifecycle and gates

### Externally authenticated lifecycle receipts

Lifecycle snapshot integrity does not rely on a digest that the same snapshot can reseal. Every externally admitted project/run lifecycle scope, including a generation-loss-only scope with no custody, receives a mandatory `LifecycleIntegrityReceiptAuthorityPort` whose storage, authentication material and append head remain external to lifecycle domain state. It exposes exactly:

The production local adapter opens, but never creates or repairs, `lifecycle-receipts.sqlite3` and a separate raw 32-byte `lifecycle-receipts.hmac.key` in the configured Fabric state directory. The directory must be owned by the daemon user with mode `0700`; both regular, non-symlink files must be owned by that user with mode `0600`. Its fixed initial DDL is `runtime/agent-fabric/schemas/lifecycle-receipt-authority-v1.sql`. Provisioning must insert the configured immutable authority ID into the single `authority_metadata` row before startup. Startup rejects missing or crossed identity, ownership, mode, file type, key length, schema, database integrity, HMAC, receipt chain and authenticated-membership state. It performs no migration, identity generation or key rotation.

~~~text
admitScope(exactScopeAdmissionRequest) -> authenticatedScopeAdmissionResolution
readScopeAdmission(projectId, projectSessionId, runId, authorityId)
  -> authenticatedScopeAdmissionResolution | null
appendReceipt(intentDigest, exactSubject) -> authenticatedReceipt
readReceipt(kind, projectSessionId, runId, agentId, ownerRefDigest, ownerRevision)
  -> { exactSubject, authenticatedReceipt } | null
readScopeCheckpoint(projectSessionId, runId) -> authenticatedScopeCheckpoint
readScopeCheckpointAt(checkpointDigest) -> authenticatedScopeCheckpoint
readScopePageAt(checkpointDigest, afterAuthoritySequence, limit=256)
  -> { orderedRecords, nextAfter | null }
readNamespaceCheckpoint(projectId) -> authenticatedNamespaceCheckpoint
readNamespacePageAt(checkpointDigest, afterScopeKey, limit=256)
  -> { orderedScopeHeads, nextAfter | null }
verifyReceipt(exactSubject, authenticatedReceipt) -> boolean
verifyScopeCheckpoint(authenticatedScopeCheckpoint) -> boolean
verifyNamespaceCheckpoint(authenticatedNamespaceCheckpoint) -> boolean
~~~

Scope admission and append are idempotent for exact bytes. Scope admission atomically creates the external scope, its authenticated zero-receipt checkpoint and a project-namespace member before returning. Reusing the same project/session/run/authority key with changed scope bytes conflicts. Reusing the same kind/project-session/run/agent/owner-ref/revision key with changed subject bytes conflicts; authority records cannot be updated or deleted. `ownerRefDigest=LD("receipt- owner-ref",ownerRef)` over the exact selected closed owner-ref arm. Custody and generation-loss refs carry their immutable positive revision and semantic/source digests; a recovery-retirement ref carries its immutable plan revision one and plan digest. Revision is canonical decimal. Receipt sequence is positive, contiguous and append-only within one authority/project-session/run scope.

The five exact closed subjects and their exact source union are:

~~~yaml
lifecycleCustodyTerminalReceiptSubjectV1:
  schemaVersion: 1
  kind: custody-terminal
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  ownerRef:
    kind: custody
    custodyRef: exact-lifecycle-custody-ref-v1
    sourceRefDigest: exact-custody-source-ref-digest
  admissionDigest: exact-lifecycle-admission-digest
  providerActionRef: exact-provider-action-ref
  fromState: exact-legal-nonterminal-custody-state
  disposition: adopted | no-effect | superseded | quarantined | abandoned
  terminalProofKind: exact-proof-kind-for-edge
  terminalEvidenceDigest: exact-digest
  recoverySource: exact-lifecycle-recovery-source-ref-v1
  recoverySourceDecisionDigest: exact-digest | null
  linkedLossEffectDigest: exact-digest | null
  transitionReplayDigest: exact-digest

lifecycleGenerationLossTerminalReceiptSubjectV1:
  schemaVersion: 1
  kind: generation-loss-terminal
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  ownerRef:
    kind: generation-loss
    generationLossRef: exact-generation-loss-ref-v1
    sourceRefDigest: exact-generation-loss-source-ref-digest
  admissionDigest: exact-lifecycle-admission-digest
  lossKind: generation-advance | context-advance
  fromState: open
  terminalState: abandoned
  abandonKind: direct-open
  recoveryCustodyRef: null
  recoveryActionRef: null
  operatorDecisionDigest: exact-digest
  terminalEvidenceDigest: exact-digest
  transitionReplayDigest: exact-digest

lifecycleCustodyRecoveryRetirementReceiptSubjectV1:
  schemaVersion: 1
  kind: custody-recovery-retirement
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  ownerRef:
    kind: recovery-retirement
    retirementRef: exact-immutable-retirement-plan-ref-v1
    retirementPlanDigest: exact-retirement-plan-digest
  finalizedCustodyRef: exact-already-finalized-custody-ref-v1
  finalizedCustodySourceRefDigest: exact-finalized-custody-source-ref-digest
  finalizedCustodyJournalDigest: exact-finalized-custody-journal-digest
  admissionDigest: exact-confirmed-abandon-admission-digest
  finalizedDisposition: no-effect | superseded | quarantined
  finalizedTerminalEvidenceDigest: exact-digest
  terminalProofKind: confirmed-abandon
  transitionProofDigest: exact-digest
  mutationPlanDigest: exact-digest
  retirementEvidenceDigest: exact-digest
  transitionReplayDigest: exact-digest

lifecycleFreshOriginReceiptSubjectV1:
  schemaVersion: 1
  kind: fresh-origin
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  ownerRef:
    kind: custody
    custodyRef: exact-new-revision-one-custody-ref-v1
    sourceRefDigest: exact-new-custody-source-ref-digest
  sourceMode: terminalize-nonfinal-custody | reuse-final-custody | open-generation-loss
  recoverySource: exact-lifecycle-recovery-source-ref-v1
  sourceJournalDigest: exact-handoff-source-journal-digest
  admissionDigest: exact-fresh-recovery-admission-digest
  freshHandoffDigest: exact-handoff-digest
  freshApplyPlanDigest: exact-handoff-plan-digest
  affectedGenerationLossBeforeRef: exact-before-ref | null
  affectedGenerationLossBeforeJournalDigest: exact-before-journal | null
  affectedGenerationLossAfterRef: exact-after-ref | null
  affectedGenerationLossAfterSemanticDigest: exact-after-semantic | null
  freshOriginEffectDigest: exact-fresh-origin-effect-digest
  transitionReplayDigest: exact-same-batch-replay-digest

lifecycleReviewDecisionReceiptSubjectV1:
  schemaVersion: 1
  kind: review-adoption-decision
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  ownerRef:
    kind: custody
    custodyRef: exact-lifecycle-custody-ref-v1
    sourceRefDigest: exact-custody-source-ref-digest
  custodyTerminalSubjectDigest: exact-ordinal-one-subject-digest
  lifecycleAdoptionEvidenceDigest: exact-digest
  reviewReservationDigest: exact-digest
  reviewDecisionDigest: exact-digest
  certificationCutDigest: exact-digest | null
  recoverySource: exact-lifecycle-recovery-source-ref-v1
  recoverySourceDecisionDigest: exact-digest | null
  transitionReplayDigest: exact-same-batch-replay-digest
~~~

`lifecycleReceiptOwnerRefV1` is exactly one closed arm: `{kind:"custody",custodyRef,sourceRefDigest}`; `{kind:"generation-loss",generationLossRef,sourceRefDigest}`; or `{kind:"recovery-retirement",retirementRef,retirementPlanDigest}`. The first two refs bind their immutable semantic revision. `retirementRef` is exactly `{retirementId,revisionDec:"1"}` and its plan digest binds the finalized custody, admission, proof and complete archival mutation plan before authority append.

`lifecycleRecoverySourceRefV1` is exactly one closed arm: `{kind:"none"}`; `{kind:"custody",custodyRef,sourceRefDigest}`; or `{kind:"generation-loss",generationLossRef,sourceRefDigest}`. Both refs contain their positive immutable revision. No selected arm omits a required member or contains a member from another arm. The source-ref digest binds the complete immutable semantic revision, not the mutable head or post-authority journal wrapper. Custody semantic digest is `LD("custody-semantic",closedSemantic)`; generation-loss semantic digest uses `LD("generation-loss-semantic", closedSemantic)`. Both semantic objects exclude batch, intent, authority receipt, authorization, apply and journal digests.

The standalone generation-loss subject has only the exact `open -> abandoned/direct-open` null-action arm. A recovery-in-progress loss is owned by its exact active custody: adopted custody produces linked `recovered-adopted`, abandoned custody produces linked `abandoned/recovery-attempt`, and no-effect/superseded/quarantined custody returns it to open with immutable attempt history, except the proved zero-dispatch fresh-handoff supersession that atomically transfers its active recovery owner to the newly created custody. Those exact before/after loss revisions and their effect digest occur in the custody terminal subject and authenticated transition replay, so no second subject represents the same semantic transition. Absence of a provider action never exempts standalone direct-open abandonment from external integrity evidence.

Confirmed abandonment of an already-finalized nonadopted custody is the distinct `custody-recovery-retirement` subject. Preparation first persists its immutable revision-one retirement plan. Apply never rewrites the finalized custody or appends another custody revision: the subject equality-copies that exact custody ref, source and journal, while its replay covers the destructive archival, lease, delivery, obligation, membership, barrier, grant and freeze writes. Adopted or already-abandoned custody cannot select this path.

Every fresh-created custody is authenticated by exactly one `fresh-origin` subject before its apply. For `reuse-final-custody` and `open-generation-loss` it is ordinal one of a one-intent `fresh-origin` batch. For `terminalize-nonfinal-custody` it is ordinal two, after the old custody terminal subject, of a two-intent custody-terminal batch. Its owner is the planned new custody revision one; its handoff, source, admission, plan, affected-loss arm, effect and replay equality-copy the same immutable handoff and batch. A terminal fresh batch cannot also select review adoption: the seven legal batch arms are closed and never require a third intent.

`reviewDecisionDigest=LD("review-adoption-decision",decision)` for the final closed arm defined below. `recoverySourceDecisionDigest=LD("recovery-source- decision",decision)` for a linked prior-custody terminal decision or generation- loss recovery decision and is nonnull exactly when recovery source is not `none`. The cut digest/null arm must match the selected final decision. None of these preimages contains a receipt, intent, batch or apply digest. Thus custody, adoption evidence, decision, cut and linked recovery outcome cannot be rewritten together inside a resealed snapshot.

Every custody and generation-loss mutation appends one immutable revision row, then moves a small head pointer to that exact row. Revision one is creation and each legal edge increments by exactly one. Terminal owner revision is therefore the immutable final journal revision, not a hardcoded creation revision. Both receipt subjects and every review cut/binding equality-copy that exact ref. Authority lookup uses `(kind,projectSessionId,runId,agentId,ownerRefDigest, ownerRevision)`. Goldens include revision two and the maximum safe integer; creation/final confusion, leading-zero, mutable-head and crossed-owner forms fail.

`admissionDigest=LD("admission",lifecycleAdmissionV1)`. The exact admission is one of two closed arms: `fresh-recovery` contains the accepted fresh-rotate handoff request, exact issue, preparation and apply plan, but no batch/intent/receipt/apply/final commit; `confirmed-abandon` contains the full accepted operator commit, source, gate and direct-human confirmation. Each arm also contains schema version, project/session/run, actor principal and stable command ID. Command/attempt replay stores that same digest. No terminal subject accepts a caller-supplied or separately reconstructed digest.

The authenticated receipt has exactly `schemaVersion=1`, matching `kind`, nonempty `authorityId`, positive safe-integer `authoritySequence`, `previousReceiptDigest|null`, `subjectDigest`, `intentDigest`, `receiptDigest` and nonempty opaque `attestation`. Receipt sequence one has null previous digest; every later sequence has the exact preceding receipt digest.

Lifecycle canonical JSON is RFC 8785 JCS UTF-8 with no BOM, prefix, suffix or trailing newline. Every object is closed and every selected-arm member is present; `undefined` is inadmissible and null appears only where the displayed codec says null. A JSON integer is an integer in `0..9007199254740991`; a positive integer excludes zero. A `*Dec` member is a JSON string equal to `"0"` or `[1-9][0-9]{0,15}` and its numeric value is at most 9007199254740991. IDs are nonempty UTF-8 strings of at most 256 bytes without NUL; digests match `sha256:[0-9a-f]{64}`; timestamps, where admitted, are RFC 3339 UTC with exactly three fractional digits. Arrays preserve the specified order and are duplicate- free. No non-finite, fractional, negative or exponent-form lifecycle number is admissible.

Every digest below uses one function:

~~~text
LD(domain, value) =
  "sha256:" + lowerhex(SHA256(
    UTF8("agent-fabric.lifecycle.v1\u0000" + domain + "\u0000") ||
    RFC8785_JCS_UTF8(value)))
~~~

`domain` is the exact lowercase ASCII literal named below. The two NUL bytes are literal single bytes; `||` is byte concatenation. A digest member is never inside its own preimage. `subjectDigest=LD("receipt-subject", exactSubject)`. `receiptDigest=LD("authenticated-receipt", receiptBodyV1)`, where `receiptBodyV1` contains exactly schema version, kind, authority ID/sequence, previous receipt digest, intent digest and subject digest; it excludes `receiptDigest` and attestation. Attestation algorithm and key custody are authority-adapter owned and never enter the snapshot. Closed-shape/digest validation, `verifyReceipt` and authoritative ledger membership are all required.

The lifecycle digest registry is exact:

| Value | `LD` domain |
| --- | --- |
| lifecycle admission | `admission` |
| custody semantic revision | `custody-semantic` |
| custody journal wrapper | `custody-journal` |
| generation-loss semantic revision | `generation-loss-semantic` |
| generation-loss journal wrapper | `generation-loss-journal` |
| recovery source ref | `recovery-source-ref` |
| receipt owner ref | `receipt-owner-ref` |
| recovery retirement plan | `recovery-retirement-plan` |
| review adoption decision | `review-adoption-decision` |
| review adoption reservation | `review-adoption-reservation` |
| recovery source decision | `recovery-source-decision` |
| transition proof | `transition-proof` |
| affected-row mutation plan | `mutation-plan` |
| one lifecycle owner effect | `lifecycle-effect` |
| ordered lifecycle effect set | `effect-set` |
| transition replay | `transition-replay` |
| receipt subject | `receipt-subject` |
| ordered receipt subject set | `receipt-subject-set` |
| receipt batch ID body | `receipt-batch-id` |
| receipt intent body | `receipt-intent` |
| authenticated receipt body | `authenticated-receipt` |
| ordered authority receipt set | `authority-receipt-set` |
| receipt batch completion | `batch-completion` |
| receipt batch authorization | `batch-authorization` |
| transition apply body | `transition-apply` |
| ordered scope record set | `scope-record-set` |
| scope checkpoint body | `scope-checkpoint` |
| namespace scope-head set | `namespace-scope-head-set` |
| namespace checkpoint body | `namespace-checkpoint` |
| fresh preparation | `fresh-preparation` |
| fresh handoff reservation | `fresh-handoff` |
| fresh commit | `fresh-commit` |
| lifecycle domain snapshot | `lifecycle-domain-snapshot` |
| admitted lifecycle scope | `admitted-scope` |
| scope-admission outbox ID | `scope-admission-outbox` |
| scope-admission resolution | `scope-admission-resolution` |

The accepted request codecs used by `lifecycleAdmissionV1` are exact:

~~~yaml
lifecycleCheckpointV1:
  relativePath: canonical-relative-path
  sha256: exact-digest
  mailboxWatermark: nonnegative-safe-integer
  acknowledgedAboveWatermark: strictly-ascending-unique-nonnegative-safe-integers
  inFlightChildren: strictly-ascending-unique-ids
  openWork: strictly-ascending-unique-ids
  nextAction: nonempty-bounded-string
  providerResumeReference: nonempty-bounded-string

rotationRequestV1:
  schemaVersion: 1
  action: compact | rotate
  agentId: exact-agent
  taskId: exact-task
  taskRevision: positive-safe-integer
  checkpoint: exact-lifecycleCheckpointV1
  commandId: stable-command-id

confirmedAbandonCommitRequestV1:
  schemaVersion: 1
  commandId: stable-command-id
  projectId: exact-project
  previewId: exact-preview
  expectedPreviewRevision: positive-safe-integer
  previewDigest: exact-digest
  expectedIntentDigest: exact-digest
  confirmation: {kind: explicit, confirmationId: exact-id} | {kind: echo, echoedPreviewDigest: exact-equal-preview-digest}
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  recoverySource: exact-lifecycle-recovery-source-ref-v1
  gateId: exact-approved-gate
  gateRevision: positive-safe-integer
  reason: nonempty-bounded-string
  directHumanAttestationDigest: exact-digest

lifecycleAdmissionV1:
  oneOf:
    - schemaVersion: 1
      admissionKind: fresh-recovery
      projectId: exact-project
      projectSessionId: exact-project-session
      runId: exact-run
      actorPrincipal: {kind: operator, operatorId: exact-operator, sessionGeneration: positive-safe-integer}
      commandId: exact-request-command
      request:
        schemaVersion: 1
        commandId: stable-command-id
        projectId: exact-project
        previewId: exact-preview
        expectedPreviewRevision: positive-safe-integer
        previewDigest: exact-digest
        expectedIntentDigest: exact-digest
        confirmation: {kind: explicit, confirmationId: exact-id} | {kind: echo, echoedPreviewDigest: exact-equal-preview-digest}
        attemptId: exact-attempt
        issueId: exact-active-issue
        projectSessionId: exact-project-session
        runId: exact-run
        agentId: exact-agent
        recoverySource: exact-lifecycle-recovery-source-ref-v1
        replacementAdapterId: exact-adapter
        replacementContractDigest: exact-digest
        replacementActionRef: exact-new-provider-action-ref
        checkpointRef: exact-checkpoint-ref
        checkpointDigest: exact-digest
        checkpointValidationReceiptDigest: exact-digest | null
        preparationDigest: exact-fresh-preparation-digest
        handoffId: exact-handoff
        plannedApplyId: exact-transition-apply
        handoffApplyPlanDigest: exact-lifecycle-mutation-plan-digest
    - schemaVersion: 1
      admissionKind: confirmed-abandon
      projectId: exact-project
      projectSessionId: exact-project-session
      runId: exact-run
      actorPrincipal: {kind: operator, operatorId: exact-operator, sessionGeneration: positive-safe-integer}
      commandId: exact-request-command
      request: exact-confirmedAbandonCommitRequestV1
~~~

Every brace-form union above is a closed object, not shorthand for an open map. Outer scope, command and actor equality-copy the selected request and authenticated principal. Echo confirmation requires byte-equal preview digest. Checkpoint-validation receipt is nonnull exactly when the selected source lacked a currently valid checkpoint and the separate validator supplied one. The fresh-recovery handoff/apply plan is fixed before any authority append, and the final record can only equality-copy its admission digest after apply.

Every scope page carries one byte-identical checkpoint:

~~~yaml
lifecycleScopeAdmissionRequestV1:
  schemaVersion: 1
  admissionRequestId: exact-digest-derived-id
  projectId: exact-project
  projectSessionId: exact-project-session
  runId: exact-run
  authorityId: exact-authority
  admissionDigest: exact-first-lifecycle-admission
  admittedAt: exact-timestamp
  scopeDigest: exact-admitted-scope-digest

lifecycleScopeAdmissionResolutionV1:
  schemaVersion: 1
  admissionRequestId: exact-request
  scopeDigest: exact-request-scope-digest
  initialScopeCheckpoint: exact-authenticated-zero-receipt-checkpoint
  namespaceCheckpointDigest: exact-authenticated-namespace-checkpoint
  namespaceMember:
    projectSessionId: exact-project-session
    runId: exact-run
    authorityId: exact-authority
    scopeCheckpointDigest: exact-initial-checkpoint
    receiptCountDec: "0"
    headReceiptDigest: null
  verifiedAt: exact-timestamp
  resolutionDigest: exact-digest

lifecycleAdmittedRunScopeV1:
  schemaVersion: 1
  admissionRequestId: exact-request
  projectId: exact-project
  projectSessionId: exact-project-session
  runId: exact-run
  authorityId: exact-authority
  admissionDigest: exact-first-lifecycle-admission
  scopeDigest: exact-request-scope-digest
  initialScopeCheckpointDigest: exact-resolution-checkpoint
  admissionResolutionDigest: exact-resolution-digest
  admittedAt: exact-timestamp

lifecycleReceiptScopeCheckpointV1:
  schemaVersion: 1
  authorityId: exact-authority
  projectSessionId: exact-project-session
  runId: exact-run
  receiptCountDec: nonnegative
  headAuthoritySequenceDec: nonnegative
  headReceiptDigest: exact-digest | null
  orderedRecordSetDigest: exact-digest
  checkpointDigest: exact-digest
  attestation: nonempty-opaque

lifecycleReceiptNamespaceCheckpointV1:
  schemaVersion: 1
  authorityId: exact-authority
  projectId: exact-project
  scopeCountDec: nonnegative
  orderedScopeHeadSetDigest: exact-digest
  checkpointDigest: exact-digest
  attestation: nonempty-opaque
~~~

`scopeDigest=LD("admitted-scope",requestBody)` over the request fields from schema version through `admittedAt`; `admissionRequestId=LD("scope-admission- outbox",{schemaVersion:1,scopeDigest})`. `resolutionDigest=LD("scope-admission- resolution",body)` over every displayed resolution member except itself. Before any local lifecycle identity, issue, handoff, batch or apply write, one immutable outbox row is the only permitted local write. A recovery worker point-reads the external admission, calls `admitScope` only on authoritative absence and point-reads again after return, throw or timeout. It verifies the resolution and zero checkpoint, then pins the returned namespace checkpoint and pages its complete ordered member set under the bounds below. Before writing locally it verifies the checkpoint attestation, exact member count, contiguous ordinals, sort order and `orderedScopeHeadSetDigest`, including the exact zero-receipt member for the admitted scope. The local finalization transaction contains exactly `5 + N` writes for a namespace checkpoint with `N` complete ordered members: the admitted scope, authenticated zero-receipt scope checkpoint and head, verified namespace checkpoint, all `N` exact namespace members and the immutable resolution. Existing admitted scopes and checkpoints provide the parents for prior namespace members. These local rows retain the verified evidence; they do not repeat the separate external authority operation. Exact replay returns the existing rows; changed bytes conflict. The outbox is retained immutable, so no response- loss or local crash can erase the obligation to finish admission.

Count and head sequence are equal; both zero exactly with null head and an empty record set. `orderedRecordSetDigest=LD("scope-record-set",records)` for the complete authority-sequence-ordered array of exactly `[authoritySequenceDec,receiptDigest,intentDigest,kind,agentId,ownerKind, ownerId,ownerRevisionDec]`. `checkpointDigest=LD("scope-checkpoint",body)` for the complete checkpoint object with checkpoint digest and attestation omitted. The first read pins that immutable checkpoint; every continuation is `readScopePageAt(checkpointDigest,after,256)`. Normal live-head advancement does not change or starve that pinned view. Only an inconsistent, absent or unsupported pinned checkpoint restarts the scan, with at most three bounded restarts. Pages contain at most 256 contiguous records; `nextAfter` is the last returned sequence or null only at the pinned head. Hydration rejects gap, duplicate/reorder/head drift and more than 65,536 receipts per run, and calls `verifyScopeCheckpoint`. A point lookup alone is never completeness evidence.

The namespace checkpoint covers every externally admitted authority scope in the project, including a scope with zero lifecycle receipts. Its ordered members are exactly `[projectSessionId,runId,authorityId,scopeCheckpointDigest,receiptCountDec, headReceiptDigest]`, strictly sorted by project-session ID then run ID. `orderedScopeHeadSetDigest=LD("namespace-scope-head-set",members)` and `checkpointDigest=LD("namespace-checkpoint",body)` with checkpoint digest and attestation omitted. Namespace pages are pinned and bounded exactly like scope pages, with at most 256 members and at most 65,536 scopes. Hydration starts from this authenticated directory, not locally discovered custody/run rows, so a whole run removed from an older resealed snapshot remains externally visible. Every namespace member is resolved through `readScopeCheckpointAt(scopeCheckpointDigest)` and must exact-match that separately verified historical checkpoint even when the live scope head advances. Missing, extra, crossed or unverifiable scope membership is `SNAPSHOT_INVALID`. For a zero-receipt member, hydration verifies the empty checkpoint and requires the exact local admission outbox/resolution/scope tuple; it never infers the scope from local custody rows. Hydration compares every stored namespace-checkpoint column -- project, authority, count, ordered-set digest, canonical checkpoint JSON, checkpoint digest and attestation -- and the complete ordinal member set with the verified external snapshot. Comparing digest identifiers alone is insufficient.

External append is driven by a durable local outbox, never by already-mutated lifecycle state. One immutable batch and all its immutable intents are persisted before any external append or terminal/adoption/archive mutation:

~~~yaml
lifecycleRecoveryRetirementPlanV1:
  schemaVersion: 1
  retirementId: exact-stable-retirement
  revisionDec: "1"
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  finalizedCustodyRef: exact-finalized-nonadopted-custody
  finalizedCustodySourceRefDigest: exact-source-ref-digest
  finalizedCustodyJournalDigest: exact-journal-digest
  finalizedDisposition: no-effect | superseded | quarantined
  finalizedTerminalEvidenceDigest: exact-digest
  admissionDigest: exact-confirmed-abandon-admission
  transitionProof: exact-confirmed-abandon-proof
  transitionProofDigest: exact-digest
  mutationPlan: exact-complete-archival-plan
  mutationPlanDigest: exact-digest
  retirementEvidenceDigest: exact-digest
  plannedApplyId: exact-transition-apply
  recordedAt: exact-prepared-timestamp
  retirementPlanDigest: exact-digest

lifecycleIntegrityReceiptBatchV1:
  schemaVersion: 1
  batchId: exact-digest-derived-id
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  plannedApplyId: exact-transition-transaction-id
  transitionKind: custody-terminal | generation-loss-terminal | custody-recovery-retirement | fresh-origin
  primaryOwnerBeforeRef: exact-custody-or-generation-loss-or-retirement-plan-or-fresh-handoff-ref
  primaryOwnerAfterRef: exact-terminal-revision-or-same-retirement-plan-or-new-custody-ref
  primaryOwnerBeforeJournalDigest: exact-current-journal-digest
  primaryOwnerAfterSemanticDigest: exact-planned-semantic-digest
  effectsSetDigest: exact-primary-plus-linked-effect-set-digest
  transitionReplay: exact-closed-object
  transitionReplayDigest: exact-digest
  orderedSubjectSetDigest: exact-digest
  receiptIntentCountDec: "1" | "2"
  secondaryIntentKind: none | fresh-origin | review-adoption-decision
  reviewReservationRef: exact-review-reservation-ref | null
  freshHandoffRef: exact-fresh-handoff-ref | null
  intents:
    - ordinalDec: "1" | "2"
      kind: custody-terminal | generation-loss-terminal | custody-recovery-retirement | fresh-origin | review-adoption-decision
      subject: exact-closed-subject-above
      subjectDigest: exact-digest
      intentDigest: exact-digest

lifecycleAuthorityReceiptV1:
  schemaVersion: 1
  batchId: exact-batch
  ordinalDec: exact-intent-ordinal
  intentDigest: exact-intent
  authorityReceipt: exact-authenticated-receipt

lifecycleReceiptBatchCompletionV1:
  schemaVersion: 1
  batchId: exact-batch
  transitionKind: exact-batch-transition-kind
  receiptIntentCountDec: "1" | "2"
  ordinalOne: {intentDigest: exact-intent, subjectDigest: exact-subject, authorityReceiptDigest: exact-receipt}
  ordinalTwo: {intentDigest: exact-intent, subjectDigest: exact-subject, authorityReceiptDigest: exact-receipt} | null
  primaryEffect: {kind: custody | generation-loss | recovery-retirement | fresh-origin, effectDigest: exact-effect}
  linkedLossEffectDigest: exact-effect | null
  secondaryEffect: {kind: fresh-origin, effectDigest: exact-effect} | null
  orderedAuthorityReceiptSetDigest: exact-digest
  completionDigest: exact-digest

lifecycleReceiptBatchAuthorizationV1:
  schemaVersion: 1
  batchId: exact-batch
  batchCompletionDigest: exact-completion
  orderedAuthorityReceiptSetDigest: exact-digest
  verifiedScopeCheckpointDigest: exact-scope-checkpoint
  authorizedAt: exact-prepared-timestamp
  authorizationDigest: exact-digest

lifecycleTransitionApplyV1:
  oneOf:
    - schemaVersion: 1
      applyKind: terminal
      applyId: exact-batch-planned-apply-id
      receiptBatchId: exact-batch
      batchCompletionDigest: exact-batch-completion
      transitionReplayDigest: exact-batch-replay
      orderedAuthorityReceiptSetDigest: exact-digest
      verifiedScopeCheckpointDigest: exact-scope-checkpoint
      primaryOwnerAfterRef: exact-batch-after-ref
      freshHandoffRef: null
      freshSourceMode: null
      freshApplyPlanDigest: null
      newCustodyRef: null
      generationLossAfterRef: null
      appliedMutationPlanDigest: exact-batch-plan
      localWriteSetDigest: exact-complete-write-set
      applyDigest: exact-digest
    - schemaVersion: 1
      applyKind: terminal-fresh
      applyId: exact-batch-planned-apply-id
      receiptBatchId: exact-batch
      batchCompletionDigest: exact-batch-completion
      transitionReplayDigest: exact-batch-replay
      orderedAuthorityReceiptSetDigest: exact-digest
      verifiedScopeCheckpointDigest: exact-scope-checkpoint
      primaryOwnerAfterRef: exact-batch-after-ref
      freshHandoffRef: exact-batch-handoff-ref
      freshSourceMode: terminalize-nonfinal-custody
      freshApplyPlanDigest: exact-handoff-plan
      newCustodyRef: exact-created-revision-one-custody
      generationLossAfterRef: exact-recovery-in-progress-transfer-ref | null
      appliedMutationPlanDigest: exact-batch-plan
      localWriteSetDigest: exact-complete-write-set
      applyDigest: exact-digest
    - schemaVersion: 1
      applyKind: fresh
      applyId: exact-fresh-origin-batch-planned-apply-id
      receiptBatchId: exact-fresh-origin-batch
      batchCompletionDigest: exact-batch-completion
      transitionReplayDigest: exact-batch-replay
      orderedAuthorityReceiptSetDigest: exact-digest
      verifiedScopeCheckpointDigest: exact-scope-checkpoint
      primaryOwnerAfterRef: exact-batch-new-custody-ref
      freshHandoffRef: exact-batch-handoff-ref
      freshSourceMode: reuse-final-custody | open-generation-loss
      freshApplyPlanDigest: exact-handoff-plan
      newCustodyRef: exact-created-revision-one-custody
      generationLossAfterRef: exact-recovery-in-progress-ref | null
      appliedMutationPlanDigest: exact-handoff-plan
      localWriteSetDigest: exact-complete-write-set
      applyDigest: exact-digest
~~~

`retirementPlanDigest=LD("recovery-retirement-plan",body)` over every displayed plan member except the digest. Its plan, proof and evidence are byte-identical to the retirement subject/replay/batch; a changed archival write cannot reuse the same plan or authority receipt. The subject, plan, effect and result equality-copy the exact `finalizedTerminalEvidenceDigest`, `admissionDigest`, `transitionProofDigest`, `mutationPlanDigest` and `retirementEvidenceDigest` tuple; the finalized terminal evidence also equality-binds the exact finalized custody revision.

The transition replay is exactly `lifecycleTransitionReplayV1`:

~~~yaml
lifecycleTransitionReplayV1:
  oneOf:
    - schemaVersion: 1
      transactionId: exact-stable-transaction
      projectSessionId: exact-project-session
      runId: exact-run
      agentId: exact-agent
      transitionKind: custody-terminal | generation-loss-terminal | custody-recovery-retirement
      primaryOwnerBeforeRef: exact-ref
      primaryOwnerAfterRef: exact-same-identity/revision-plus-one-or-same-retirement-plan-ref
      primaryOwnerBeforeJournalDigest: exact-current-journal-digest
      primaryOwnerAfterSemanticDigest: exact-planned-semantic-digest
      effectsSetDigest: exact-batch-effect-set
      admissionDigest: exact-digest
      providerActionRef: exact-provider-action-ref | null
      recoverySource: exact-lifecycle-recovery-source-ref-v1
      terminalDisposition: adopted | no-effect | superseded | quarantined | abandoned | recovery-retired
      terminalEvidenceDigest: exact-digest
      transitionProof: exact-proof-arm
      transitionProofDigest: exact-digest
      mutationPlan: exact-lifecycle-mutation-plan-v1
      mutationPlanDigest: exact-digest
      reviewReservationDigest: exact-digest | null
      freshHandoffDigest: exact-digest | null
    - schemaVersion: 1
      transactionId: exact-handoff-planned-apply-id
      projectSessionId: exact-project-session
      runId: exact-run
      agentId: exact-agent
      transitionKind: fresh-origin
      primaryOwnerBeforeRef: exact-fresh-handoff-ref
      primaryOwnerAfterRef: exact-new-revision-one-custody-ref
      primaryOwnerBeforeJournalDigest: exact-source-journal-digest
      primaryOwnerAfterSemanticDigest: exact-new-custody-semantic-digest
      effectsSetDigest: exact-fresh-origin-effect-set-digest
      admissionDigest: exact-fresh-recovery-admission-digest
      recoverySource: exact-lifecycle-recovery-source-ref-v1
      sourceMode: reuse-final-custody | open-generation-loss
      freshHandoffDigest: exact-handoff-digest
      freshApplyPlanDigest: exact-handoff-plan-digest
      affectedGenerationLossBeforeRef: exact-before-ref | null
      affectedGenerationLossBeforeJournalDigest: exact-before-journal | null
      affectedGenerationLossAfterRef: exact-after-ref | null
      affectedGenerationLossAfterSemanticDigest: exact-after-semantic | null
~~~

`lifecycleMutationPlanV1` (the `mutationPlan` member above and every `freshApplyPlan`) contains exactly `schemaVersion`, a complete strictly sorted `writes` array and `writeSetDigest`. Each write contains `{relation,keyDigest, operation,expectedSemanticDigest,afterSemanticJcs,afterSemanticDigest}`. `operation` is `insert|update|delete`; expected digest is null only for insert; after semantic body and digest are null only for delete. `afterSemanticJcs` is the exact closed canonical state/effect body, not SQL or caller-selected relation data. It excludes every batch, intent, authority receipt, authorization, apply and journal-wrapper member; those downstream links occur only in immutable effect/apply rows after authority. The closed relation enum is: `agent-state`, `provider-session`, `provider-lineage`, `principal-capability`, `agent-bridge`, `chair-bridge`, `turn-lease`, `write-lease`, `delivery`, `task-owner`, `result-obligation`, `membership`, `barrier`, `freeze-owner`, `provider-action`, `custody-revision`, `custody-head`, `generation-loss-revision`, `generation-loss- head`, `review-cut`, `review-binding`, `review-binding-pointer`, `recovery- issue`, `fresh-preparation`, `fresh-handoff`, `fresh-commit`, `recovery- retirement`, and `audit`. `writeSetDigest=LD("mutation-plan", {schemaVersion:1,writes})`; `mutationPlanDigest` equality-copies it, and `freshApplyPlanDigest` equality-copies it when this codec is the handoff plan. The `provider-action` member is update-only. Its `keyDigest` binds the exact daemon-global `ProviderActionRefV1` pair `{adapterId,actionId}`: a normal `mutationPlan` equality-copies the replay's non-null `providerActionRef`, while a `freshApplyPlan` equality-copies its enclosing `freshRecoveryHandoffV1.replacementActionRef`. The applicable pair must exist; insert, delete or any different pair is invalid. Every row changed by adoption, no-effect, supersession, quarantine or abandonment, including archival delivery/task/barrier effects and a linked generation loss, must occur exactly once. No unplanned lifecycle-affecting write may share the apply transaction.

An owner-transition receipt effect is exactly `{schemaVersion:1,effectKind,role,ownerBeforeRef,beforeJournalDigest, ownerAfterRef,afterSemanticDigest}`. A fresh-origin effect is exactly `{schemaVersion:1,effectKind:"fresh-origin",role,sourceMode,recoverySource, sourceJournalDigest,freshHandoffDigest,admissionDigest,freshApplyPlanDigest, newCustodyRef,newCustodySemanticDigest,newCustodySourceRefDigest, affectedGenerationLossBeforeRef,affectedGenerationLossBeforeJournalDigest, affectedGenerationLossAfterRef,affectedGenerationLossAfterSemanticDigest}`. Each has `effectDigest=LD("lifecycle-effect",body)`.

A recovery-retirement effect uses this distinct closed arm:

~~~yaml
lifecycleRecoveryRetirementEffectV1:
  schemaVersion: 1
  effectKind: recovery-retirement
  role: primary
  ownerBeforeRef: exact-immutable-retirement-plan-ref-v1
  beforeJournalDigest: exact-finalized-custody-journal-digest
  ownerAfterRef: exact-same-immutable-retirement-plan-ref-v1
  afterSemanticDigest: exact-retirement-plan-digest
  finalizedTerminalEvidenceDigest: exact-digest
  admissionDigest: exact-digest
  transitionProofDigest: exact-digest
  mutationPlanDigest: exact-digest
  retirementEvidenceDigest: exact-digest
  effectDigest: exact-digest
~~~

Its mutation plan contains all archival effects. `effectDigest=LD("lifecycle-effect",body)` over every displayed effect member except `effectDigest`; crossing any of the five evidence digests is invalid. The effect set contains the primary effect first and at most one linked effect second; `effectsSetDigest=LD("effect-set",members)`. A custody batch has exactly one primary custody effect; a standalone loss batch exactly one primary loss effect; a recovery-retirement batch exactly one primary retirement effect; a pure fresh-origin batch exactly one primary fresh-origin effect; and a terminal-fresh custody batch has its primary custody effect followed by one secondary fresh-origin effect. A custody batch may also carry its one declared linked loss; no other batch may. The effect set equality-copies the corresponding mutation-plan semantic writes.

`transitionProofDigest=LD("transition-proof",transitionProof)` and `transitionReplayDigest=LD("transition-replay",transitionReplay)`. Ordered subject members are exactly `{ordinalDec,kind,ownerRefDigest,ownerRevisionDec, subjectDigest}`, and `orderedSubjectSetDigest=LD("receipt-subject-set",members)`. Ordinal one is the primary custody terminal, standalone generation-loss terminal, recovery retirement or pure fresh origin. Ordinal two exists exactly for either adopted true-chair custody and is its review decision, or terminal-fresh custody and is its fresh-origin subject. These arms are mutually exclusive. A linked generation-loss effect is in ordinal one's mutation plan, not another subject. `batchId=LD("receipt-batch-id",body)` where body contains exactly schema version, scope/agent, planned apply ID, transition kind, both primary refs, before-journal/after-semantic/effect-set digests, transition replay digest, ordered subject-set digest, count, secondary intent kind, review reservation ref and fresh handoff ref. Each `intentDigest=LD("receipt-intent",{schemaVersion:1,batchId,ordinalDec,kind, subjectDigest,transitionReplayDigest})`.

`orderedAuthorityReceiptSetDigest=LD("authority-receipt-set",members)`, where members are in batch ordinal order and each is exactly `{ordinalDec, intentDigest,authorityId,authoritySequenceDec,receiptDigest,subjectDigest}`. `completionDigest=LD("batch-completion",body)` over every displayed completion member except itself. Completion exists only after every declared intent and its exact verified authority receipt exist. Count one requires null ordinal two; count two requires both ordinals. Primary-effect kind must match transition kind. Custody terminal may name one linked loss and, in its terminal-fresh arm, one secondary fresh-origin effect; no other batch may. Authorization and every apply equality-copy the same completion and ordered receipt-set digests, so none can exist for a childless, partial or crossed batch. `authorizationDigest=LD("batch-authorization",body)` over every displayed authorization member except itself. `applyDigest=LD("transition-apply",body)` where body is every displayed selected `lifecycleTransitionApplyV1` member except `applyDigest`. The terminal arm has no fresh values, while terminal-fresh and fresh equality-copy their authenticated batch handoff and fresh-origin effect. `localWriteSetDigest` covers every row written by the one transaction, including journal wrappers and the apply marker, but is a digest of sorted relation/key/operation identities rather than row contents; it therefore cannot introduce a digest cycle. Only after its terminal, terminal-fresh or fresh batch is externally authorized does the transaction construct journal wrappers and materialize the planned fresh custody.

`LD("custody-journal",wrapper)` or `LD("generation-loss-journal",wrapper)` hashes exactly `{schemaVersion,ownerRef,priorJournalDigest,semanticDigest, sourceRefDigest,authorityBatchId,authorityApplyId,authorityApplyDigest, originFreshApplyId,originFreshApplyDigest,recordedAt}`. The five downstream members form one exact arm: an ordinary creation/intermediate revision has all five null; any revision materialized by an externally authenticated batch has the three authority members nonnull and both origin members null; and a fresh-created custody or fresh-advanced generation-loss revision has the three authority members null and both origin members nonnull. The two nonnull arms are mutually exclusive. Thus a linked loss `recovery-in-progress -> open` still binds the owning custody batch/apply, while `open -> recovery-in-progress` binds the fresh apply that created its custody. `recordedAt`, apply ID and write identities were fixed in the prepared transition plan; the apply digest is computed after authority but before the transaction. No journal value enters its apply, semantic/source, subject, replay, batch or intent preimage.

Batch and intent rows never update or delete. Returned receipts are separate append-only rows keyed by batch/ordinal; apply is a separate append-only marker. Derived state is `prepared` for batch+effects+all intents without the exact completion/authorization/apply set, `authority-complete` for one exact completion covering all verified authority rows/effects plus one verified scope checkpoint without apply, and `applied` only with the exact apply marker. There are no independently mutable copies of those states. Direct SQL cannot expose a half receipt, half review or half apply.

The terminal-proof union exhaustively matches the lifecycle edge table: `zero-dispatch-no-effect` embeds the exact zero-dispatch journal; `predispatch-superseded` embeds the awaiting-boundary/prepared source/checkpoint drift proof; `postterminal-adoption-cas-superseded` embeds the exact provider- terminal-or-committing source state, authenticated terminal observation, replacement candidate, checkpoint, precondition and local write-set values that were equality-checked by the failed adoption CAS; `fresh-handoff-superseded` embeds the zero-dispatch nonfinal-source handoff, issue, preparation and complete fresh apply plan; `provider-terminal` embeds the authenticated closed terminal observation and replacement candidate; `provider-no-effect` embeds the activated adapter's authenticated no-effect proof; `integrity-quarantine` embeds malformed/crossed/ conflict evidence; and `confirmed-abandon` embeds the exact operator authority, gate, direct-human attestation and archival write set. Provider observation is absent in every nonprovider arm. Kind, source phase, disposition, evidence and nullability are a closed truth table. In particular, predispatch supersession is legal only from awaiting-boundary/prepared, postterminal adoption-CAS supersession only from provider-terminal/committing, and neither proof arm may justify the other. Fixtures cover every legal from-state/proof/disposition edge and reject using one arm to justify another.

Each proof arm is a closed object with `schemaVersion:1`, its literal `kind`, and exactly these remaining members:

~~~yaml
zero-dispatch-no-effect:
  sourceState: awaiting-boundary | prepared
  providerActionRef: exact-ref
  zeroDispatchJournalDigest: exact-digest
  dispatchCountDec: "0"
  expectedSourceJournalDigest: exact-digest
  expectedCheckpointDigest: exact-digest

predispatch-superseded:
  sourceState: awaiting-boundary | prepared
  driftKind: source | checkpoint
  expectedSourceJournalDigest: exact-digest
  observedSourceJournalDigest: exact-digest
  expectedCheckpointDigest: exact-digest
  observedCheckpointDigest: exact-digest

postterminal-adoption-cas-superseded:
  sourceState: provider-terminal | committing
  terminalObservationDigest: exact-digest
  replacementCandidateDigest: exact-digest
  expectedSourceJournalDigest: exact-digest
  observedSourceJournalDigest: exact-digest
  expectedCheckpointDigest: exact-digest
  observedCheckpointDigest: exact-digest
  expectedMutationPreconditionDigest: exact-digest
  failedCasEvidenceDigest: exact-digest

fresh-handoff-superseded:
  sourceState: awaiting-boundary | prepared
  providerActionRef: exact-old-ref
  zeroDispatchJournalDigest: exact-digest
  dispatchCountDec: "0"
  issueId: exact-issue
  preparationDigest: exact-digest
  freshHandoffDigest: exact-digest
  freshApplyPlanDigest: exact-digest
  replacementActionRef: exact-distinct-new-ref

provider-terminal:
  sourceState: provider-terminal | committing
  terminalObservationDigest: exact-digest
  replacementCandidateDigest: exact-digest
  launchAttestationDigest: exact-digest
  adoptionPreconditionDigest: exact-digest

provider-no-effect:
  sourceState: provider-terminal
  providerActionRef: exact-ref
  adapterContractDigest: exact-digest
  authenticatedNoEffectProofDigest: exact-digest
  reason: provider-declared-no-effect

integrity-quarantine:
  sourceState: awaiting-boundary | prepared | dispatched | accepted | ambiguous | provider-terminal | committing
  failureKind: malformed | crossed | conflicting | unverifiable
  integrityEvidenceDigest: exact-digest
  conflictingRecordDigest: exact-digest | null

confirmed-abandon:
  oneOf:
    - sourceKind: custody | generation-loss
      sourceState: exact-nonterminal-source-state
      operatorId: exact-operator
      gateId: exact-gate
      gateRevision: positive
      directHumanAttestationDigest: exact-digest
      archivalMutationPlanDigest: exact-equal-transition-plan-digest
    - sourceKind: recovery-retirement
      retirementRef: exact-revision-one-retirement-plan-ref
      finalizedCustodyRef: exact-finalized-nonadopted-custody-ref
      finalizedCustodySourceRefDigest: exact-digest
      finalizedCustodyJournalDigest: exact-digest
      finalizedDisposition: no-effect | superseded | quarantined
      operatorId: exact-operator
      gateId: exact-gate
      gateRevision: positive
      directHumanAttestationDigest: exact-digest
      archivalMutationPlanDigest: exact-equal-mutation-plan-digest
~~~

`zero-dispatch-no-effect` maps only to no-effect; `predispatch-superseded`, `postterminal-adoption-cas-superseded` and `fresh- handoff-superseded` only to superseded; `provider-terminal` only to adopted (and may carry a linked loss recovered-adopted write); `provider-no-effect` only to no-effect; `integrity-quarantine` only to quarantined; and `confirmed-abandon` only to abandoned for a nonfinal custody/loss or `recovery-retired` for an already-final nonadopted custody plan. For standalone generation loss, only confirmed-abandon is terminal; linked recovered-adopted is proved by the adopted custody's provider-terminal arm plus the exact linked-loss before/after writes. Digests cannot substitute for wrong-arm members because the closed proof codec is validated before hashing. The fresh handoff arm is legal only for an awaiting-boundary/prepared nonfinal source with proved zero dispatch. A dispatched/accepted/ambiguous source must first reach an authenticated ordinary terminal/no-effect/abandon path; fresh recovery cannot repurpose this arm.

For an adopted chair, preparation occurs at the lifecycle/review serialization point. Before the batch it persists one immutable `lifecycleReviewAdoptionReservationV1` containing exactly schema version, reservation ID, project/session/run/agent, finalized custody ref, lifecycle adoption evidence digest, terminal-sequence high-water, active target/pointer and predecessor binding snapshots (or exact null target arm), recovery-source ref and decision, the final decision below, certification cut or null, and local mutation plan digest. `reviewReservationDigest=LD("review-adoption-reservation",body)` over all those members except the digest. It contains no batch, intent, receipt, apply, successor-row or reservation digest.

The final decision is one exact arm:

~~~yaml
reviewAdoptionDecisionV1:
  oneOf:
    - schemaVersion: 1
      outcome: rebound
      targetGeneration: exact-target
      predecessorBindingGeneration: exact-active-binding
      predecessorBindingDigest: exact-active-binding-digest
      terminalSequenceHighWater: nonnegative
      lifecycleCustodyRef: exact-finalized-custody-ref
      lifecycleAdoptionEvidenceDigest: exact-digest
      certificationCut: exact-reviewCertificationCutV1
      successorBindingGeneration: predecessor-plus-one
      successorBindingDigest: exact-planned-binding-digest
      targetSubjectDigest: exact-unchanged-subject
      reviewStateSetDigest: exact-current-head/open/repair-set
    - schemaVersion: 1
      outcome: left-stale
      reason: no-current-target | target-subject-changed | binding-changed | target-head-changed | recovery-source-conflict
      targetGeneration: exact-target | null
      predecessorBindingGeneration: positive | null
      predecessorBindingDigest: exact-digest | null
      terminalSequenceHighWater: nonnegative
      lifecycleCustodyRef: exact-finalized-custody-ref
      lifecycleAdoptionEvidenceDigest: exact-digest
      certificationCut: exact-reviewCertificationCutV1 | null
      observedReviewStateDigest: exact-digest
~~~

The rebound arm requires a nonnull cut and all nonnull binding fields. The stale arm has null target/binding/cut exactly for `no-current-target`; every other reason requires all three nonnull and preserves the observed target without mutation. `recoverySourceDecisionV1` is exactly either `{schemaVersion:1,kind:"custody",sourceRef,terminalDisposition, terminalEvidenceDigest,transitionProofDigest}` or `{schemaVersion:1,kind:"generation-loss",sourceRef,linkedLossAfterRef, linkedLossAfterSemanticDigest,state,abandonKind,terminalEvidenceDigest, recoveryActionRef}`. `sourceRef` remains the immutable original recovery source; the after ref/semantic equality-copy the same batch's linked loss effect and the state/abandon/action tuple describes that after revision. It obeys the exact generation-loss truth table and contains no receipt-derived value. The reservation is the adoption linearization point: it fences target preparation/rebind/another adoption but does not block later provider terminals, which are post-cut. External subject and apply equality-copy this reservation; no recovery rereads or recomputes a later high-water, decision or binding.

After the prepared transaction commits, the worker processes local intent order without assuming adjacent global authority sequences. It point-reads before append; exact verified presence succeeds, absence permits one idempotent append, and mismatch conflicts. If append returns, throws or times out, the worker point-reads again; exact verified presence succeeds, while absence/unavailability leaves the intent pending and returns a retryable error. It never infers failure from a lost response. Each verified result inserts one immutable authority- receipt child. No terminal/review/provider/history/audit mutation occurs yet.

When all declared children and exact effect rows exist, the worker inserts one immutable batch-completion row, pins and verifies the full scope checkpoint and proves every child sequence is at or below its count and is a member of its ordered set. It then inserts one immutable batch-authorization row and scope checkpoint/head history. A following transaction equality-checks the exact primary current journal, transition replay and complete mutation plan; applies all planned custody/review/source/binding/archive/fresh-handoff writes; and inserts one apply marker. Exact post-state replay returns that apply; any state other than the exact pre-state or exact applied post-state is integrity failure. Later provider terminal/high-water/evidence rows allowed by the review reservation are post-cut and do not stale apply; it consumes the reserved cut without comparing it to the later high-water. Unrelated target/binding mutation remains fenced until apply. A transient authority failure leaves the batch prepared and the source unchanged; it never converts no-effect to quarantine. Crash fixtures stop after pre-read, append success, append-success-then-throw, receipt insert, checkpoint authorization, every planned write boundary and apply insert.

Hydration is read-only: it first pages the authenticated project namespace, then every listed scope at one pinned checkpoint, including scopes absent from the local snapshot. It never appends. It exact-reconciles each external record to one local immutable intent plus either pending or applied state. A pending intent may be externally absent; an applied intent may not. Any external row without an exact local intent proves whole-custody/run rollback; any local authority receipt absent externally proves ledger loss. Only after successful hydration may the runtime recovery worker resume a pending append/apply. Missing, extra, crossed, deleted, downgraded, chain-invalid, digest-invalid, unverifiable or coordinated- resealed evidence is `SNAPSHOT_INVALID`; an admitted lifecycle scope without its authority fails closed.

Fresh recovery uses three immutable closed records:

~~~yaml
freshRotationPreparationV1:
  schemaVersion: 1
  preparationId: exact-preparation
  attemptId: exact-attempt
  issueId: exact-issue
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  recoverySource: exact-source-ref
  sourceJournalDigest: exact-current-journal
  replacementActionRef: exact-new-pair
  checkpointRef: exact-checkpoint
  checkpointDigest: exact-digest
  checkpointValidationReceiptDigest: exact-digest | null
  adapterContractDigest: exact-digest
  operation: fresh-rotate
  reservedProviderGeneration: positive
  reservedPrincipalGeneration: positive
  reservedBridgeGeneration: positive
  preparationDigest: exact-digest

freshRecoveryHandoffV1:
  schemaVersion: 1
  handoffId: exact-handoff
  preparationId: exact-preparation
  preparationDigest: exact-digest
  attemptId: exact-attempt
  issueId: exact-issue
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  sourceMode: terminalize-nonfinal-custody | reuse-final-custody | open-generation-loss
  recoverySource: exact-source-ref
  sourceJournalDigest: exact-current-journal
  newCustodyId: exact-reserved-custody
  plannedApplyId: exact-transition-apply
  newCustodySemanticDigest: exact-planned-revision-one-semantic
  newCustodySourceRefDigest: exact-planned-revision-one-source-ref
  generationLossAfterRef: exact-planned-recovery-in-progress-ref | null
  replacementActionRef: exact-new-pair
  reservedProviderGeneration: positive
  reservedPrincipalGeneration: positive
  reservedBridgeGeneration: positive
  admissionDigest: exact-fresh-recovery-admission
  freshApplyPlan: exact-lifecycleMutationPlanV1
  freshApplyPlanDigest: exact-digest
  handoffDigest: exact-digest

freshCommitRecordV1:
  schemaVersion: 1
  commitId: exact-commit
  handoffId: exact-handoff
  handoffDigest: exact-digest
  preparationId: exact-preparation
  preparationDigest: exact-digest
  attemptId: exact-attempt
  issueId: exact-issue
  projectSessionId: exact-project-session
  runId: exact-run
  agentId: exact-agent
  recoverySource: exact-source-ref
  sourceJournalDigest: exact-handoff-source-journal
  newCustodyRef: exact-created-revision-one-custody
  newCustodySemanticDigest: exact-created-semantic
  newCustodySourceRefDigest: exact-created-source-ref
  newCustodyJournalDigest: exact-created-journal
  generationLossAfterRef: exact-created-recovery-in-progress-ref | null
  replacementActionRef: exact-new-pair
  admissionDigest: exact-fresh-recovery-admission
  freshApplyPlanDigest: exact-handoff-plan
  freshApplyId: exact-transition-apply
  freshApplyDigest: exact-transition-apply-digest
  sourceTerminalReceiptApplyDigest: exact-digest | null
  commitDigest: exact-digest
~~~

Preparation digest omits itself and uses `LD("fresh-preparation",body)`; handoff digest similarly uses `LD("fresh-handoff",body)`; commit digest uses `LD("fresh-commit",body)`. No handoff contains a batch/intent/receipt/apply/ commit back-pointer. For `terminalize-nonfinal-custody`, the immutable handoff is referenced by a custody-terminal batch using `fresh-handoff-superseded`; source custody and issue remain unchanged while authority is pending. One authorized apply finalizes the old custody as superseded, appends its journal, creates the new custody at revision one, inserts the commit and derives issue consumption. Append failure creates none of those effects. `freshApplyId` and `freshApplyDigest` identify the same `terminal-fresh` apply for `terminalize-nonfinal-custody` and the same `fresh` apply for both other modes. All three apply arms are receipt-backed: terminalize uses ordinal-two `fresh-origin` in its custody-terminal batch; reuse-final and open-loss use a one-intent `fresh-origin` batch. `sourceTerminalReceiptApplyDigest` equals `freshApplyDigest` exactly in the terminal-fresh arm and is null otherwise. `reuse-final-custody` requires the exact terminal journal and creates the new custody/commit directly from the same handoff. `open-generation-loss` requires the exact open loss journal and one transaction creates the new custody/commit and moves the loss to recovery-in- progress; it is nonterminal and has no custody-terminal batch, but its fresh-origin batch authenticates the new custody and loss advance before apply. For `open-generation-loss`, `generationLossAfterRef` is nonnull, is the source loss revision plus one and equality-copies across handoff, fresh apply, new loss journal and commit; that journal selects the origin-fresh apply arm. A `terminalize-nonfinal-custody` whose old custody actively owns a loss instead transfers that same loss `recovery-in-progress(A) -> recovery-in-progress(B)` as the authenticated linked batch effect, so the field is its receipt-backed next revision; it is null when no loss is linked. `reuse-final-custody` requires no nonterminal loss and therefore null. If a finalized custody's prior terminal effect reopened a loss, the operator source is canonically that open loss rather than the finalized custody. Recovery retirement likewise rejects while any nonterminal loss or custody owns the agent.

Issue status is a derived projection, not a mutable duplicate: valid unexpired issue without handoff/apply is `active`; handoff without commit is `commit- pending`; commit is `consumed`; revocation/expiry may win only before the handoff linearization. A pending handoff freezes later expiry/revocation until exact apply or explicit integrity recovery. The relation is bidirectional: every fresh recovery custody has exactly one preparation, handoff, commit, issue, attempt and source; every commit/consumed issue has that exact set; none is shared. A commit-pending issue without one handoff, a committed issue projected active/revoked/expired, an orphan consumed issue or an active issue with a commit/custody is `SNAPSHOT_INVALID`. Exact Preview replay precedes source-state validation; exact Commit replay precedes live checks and never reopens a loss.

The digest dependency order is binding and acyclic: leaf request/state objects; preparation, review reservation and handoff core; transition proof and mutation plan; retirement plan; owner semantic/source ref; effect/replay; terminal or retirement subject; review subject; ordered subject set; batch ID; intents; authority receipts; scope and namespace checkpoints; transition-apply body/digest; post-authority journal wrapper; atomic apply marker and final commit. Final journal/apply/receipt/batch/intent digests never feed an earlier node. Checked-in cross-Rust/TypeScript goldens freeze every JCS preimage and digest. Mutants cover every omitted/extra/null/wrong-type member, array reorder/duplicate, domain swap, cycle-producing downstream field, integer/decimal boundary, proof-arm swap, crossed owner/revision/source, linked-loss drift and altered fresh/review plan.

Runtime append failures are `LIFECYCLE_RECEIPT_AUTHORITY_UNAVAILABLE`, `LIFECYCLE_RECEIPT_APPEND_FAILED` or `LIFECYCLE_RECEIPT_INVALID`. Hydration normalises every absent/untrusted/crossed receipt failure to `SNAPSHOT_INVALID`.

## Persistence and retention

SQLite/WAL owns concurrent coordination records: agents, tasks, mailbox events,
cursors, leases, budgets, provider resume references and schema migrations.
Stage 1 isolates synchronous SQLite writes and checkpoints from adapter event
processing so a migration or WAL checkpoint cannot stall provider supervision.

Each project run directory owns:

- assignment and authority envelopes;
- checkpoints and handoffs;
- reports, patches and verification evidence;
- model-routing and adapter receipts;
- final synthesis and human-gate state.

Mailbox bodies are operational state and default to ephemeral retention.
Artifacts referenced by messages retain their project-defined classification.
The fabric never deletes provider-native session files. Unknown or user-owned
files are never pruned.

## Failure handling

| Failure | Required behaviour |
|---|---|
| Daemon restart | Replay committed events and restore cursors. Ordinary provider sessions follow their adapter recovery contract. A launched chair whose volatile bridge is lost is journalled and fenced as chair loss; never re-expose Fabric tools from its resume reference alone. Recovery uses the explicit generation-bound chair-bridge recovery custody below. |
| Duplicate message | Deduplicate by message and action key |
| Unknown provider turn | Reconcile adapter state before retrying any side-effecting action |
| Worker loss | Expire its turn lease, preserve partial artifacts and notify its parent |
| Leader loss | Freeze new grants in its subtree; chair adopts or reassigns with a new generation |
| Chair loss | Require explicit lease takeover and persisted handoff; never silently promote a peer |
| Provider outage | Bound retries; degrade optional families; block required coverage |
| Herdr control or telemetry socket loss | Continue only healthy provider processes; mark `visibility-degraded`; infer no task state from absent telemetry |
| Observed renderer or pane loss | Keep the adapter-owned session; recreate only the renderer and resume its display cursor |
| Interactive TUI or pane-process loss | Freeze delivery and the turn lease; reconcile the provider session; explicitly reattach or rotate with a higher generation |
| Interactive operator edit | Record it where integrations permit; regardless of detection, compare-and-set rejects stale task mutations and forces reconciliation; declare provenance limits honestly |
| Message storm | Apply quotas, hop limits, bounded conversations and no global broadcast |
| Overlapping writes | Reject intersecting leases and verify base revision after writes |
| Store corruption | Stop mutations, preserve the database and require recovery from exports or backup |

### Asynchronous lifecycle rotation custody

This closes FR-013 and AC-009. It does not add an automatic context-pressure controller, successor selector or research-only routing mode.

requestLifecycle with rotate or compact executes inside the caller's existing lifecycle/tool turn and carries no caller-turn ID. After committing delivery claim-expiry/reclaim and membership/delivery-watermark housekeeping, the daemon derives exactly one active provider_session_turn_lease from the authenticated capability, current live bridge generation and provider-session generation. Zero, multiple, foreign or quarantined candidates reject; any other active/quarantined predecessor lease also rejects. released and revoked are the only terminal turn states; quarantined is not. The delivery-claim path performs the same housekeeping and rechecks lifecycle freeze immediately before its claim CAS. A stranded agent cannot authenticate a synthetic self-rotation.

One request transaction first quarantines every active agent-owned write lease, then validates and snapshots the exact daemon-validated checkpoint, task, children, `openWorkSetDigest` and ordered predecessor-turn revisions. The open- work set includes every nonterminal request-result obligation, especially provider-accepted/unconsumed delivery, in canonical obligation-ID/revision order. It fences delivery claims, records one immutable delivery-cut watermark and captures only claimed predecessor delivery IDs/generations in the adoption vector; ready/unclaimed rows are successor-pending and excluded. It rechecks the post-housekeeping freeze/lease set, fences new delivery/provider turns, suspends the agent, reserves replacement generations, inserts custody in awaiting-boundary and commits an immutable accepted LifecycleResult whose lifecycle is suspended. No adapter I/O occurs. Accepted acknowledges durable custody only; exact replay always returns that receipt. getAgentLifecycle is the separate current-state read.

Durable delivery enqueue may continue while suspended, but claim and acknowledgement are denied. Existing ready/unclaimed rows and every enqueue after the cut remain ordered successor-pending; they do not enter or stale the captured checkpoint/precondition/adoption digest. Adoption makes those same rows claimable by the successor without replay or re-enqueue. A peer can add pending work but cannot force repeated checkpoint supersession. The captured caller turn is the sole in-band exception: its old capability may finish only this lifecycle call and bounded lifecycle reads. It cannot start another turn, mutate task/mailbox/authority, acquire a write lease or close a barrier. The staged capability may invoke only the existing grant-scoped launch.attest descriptor for action-bound activation, challenge response and exact checkpoint-vector acknowledgement. Every other mutation, turn, write or barrier operation fails while lifecycle custody owns the agent.

Each custody reservation increments durable per-run/agent global provider- and principal-generation high-water marks plus a per-run/agent/bridge-owner bridge- generation high-water mark. Each stored target is exactly its corresponding predecessor high-water plus one, even when an earlier staged attempt was superseded or quarantined; a generation is never reused. Only the bridge sequence is distinct for chair versus child. Custody also snapshots the exact source provider-session reference, capability hash, custody action, adapter and contract, bridge row/revision and, for a chair, project-session/run/chair-lease generations. Final adoption must CAS and revoke those exact source rows while installing the reserved targets. Skipped, reused, crossed-owner or source-plus- one substitutions reject.

The complete state/disposition edges are:

| From | Required proof/event | To | Terminal disposition |
| --- | --- | --- | --- |
| awaiting-boundary | captured caller and every predecessor turn terminal at the recorded generation | prepared | none |
| awaiting-boundary or prepared | durable journal proves zero dispatch | finalized | no-effect |
| awaiting-boundary or prepared | checkpoint/source drift before dispatch | finalized | superseded |
| prepared | action and one-time volatile handoff durably marked before I/O | dispatched | none |
| dispatched | authenticated provider acceptance, but not terminal outcome | accepted | none |
| dispatched or accepted | bounded observation cannot prove outcome/effect | ambiguous | none |
| dispatched, accepted or ambiguous | authenticated terminal activation/checkpoint or adapter-advertised closed no-effect proof | provider-terminal | none |
| provider-terminal | exact adoption preconditions remain current | committing | none |
| committing | atomic generation/bridge/capability CAS succeeds | finalized | adopted |
| provider-terminal or committing | checkpoint/source CAS drift | finalized | superseded |
| provider-terminal | authenticated closed post-dispatch no-effect proof | finalized | no-effect |
| any nonfinal state | malformed, crossed or conflicting evidence | finalized | quarantined |
| any nonfinal state | confirmed agent-lifecycle-recovery abandon | finalized | abandoned |

No other edge or disposition is legal. In particular, absence/timing alone cannot produce post-dispatch no-effect; the activated adapter contract must advertise and return its authenticated closed no-effect proof. `revoked` is a terminal capability/turn-lease status, not a custody disposition.

Finalized `no-effect` and `superseded` revoke only the staged replacement, release only delivery/turn/write freeze contributions owned by that custody, retain the still-valid predecessor bridge/capability and return the agent to `ready`. They do not require an operator recovery gate. Finalized `quarantined` keeps the agent `suspended` and marks it `recovery-required`; owned freezes remain until the narrow recovery/abandon path resolves them. Finalized `abandoned` uses the explicit archival transaction below. These lifecycle exits are part of the same terminal custody transaction and cannot be performed by generic Resume.

awaiting-boundary becomes prepared only after the in-band caller turn is released and every captured predecessor is terminal at its recorded generation. An operator-created fresh rotation has no caller-turn exception and cannot enter awaiting-boundary until every predecessor turn is terminal. It binds an exact replacement adapter, activated contract, new action ID, current validated checkpoint row and reserved high-water targets.

After dispatch the replacement session must use launch.attest to return the grant challenge and exact checkpoint/task/mailbox/child/open-work vector. The daemon verifies and retains that successor volatile bridge before database adoption. The final transaction rechecks custody and source/high-water/CAS bindings, persists the session, swaps child custody through agent_bridge_state or chair custody through launched_chair_bridge_state, activates the staged capability, revokes the predecessor, transfers the exact open-work obligations and returns the agent to ready. Only after commit does the daemon best-effort retire the exact old volatile bridge; its revoked capability makes a crash-safe leftover powerless. Existing write leases remain quarantined and require explicit recovery or reacquisition.

For a true chair, that same serialization point captures the certifying-review cut and performs deterministic binding rebind-or-stale. Review actions, ambiguity and capacity state cannot reject or roll back lifecycle adoption; their existing recovery/preparation fences remain owned by the review subsystem.

Checkpoint identity never floats. A provider acknowledgement of A cannot adopt B. A becomes finalized/superseded, its staged capability/bridge is revoked and its reserved generations remain spent. B needs a new custody, action, capability, challenge, high-water reservation and acknowledgement.

LifecycleRotationRecoveryService runs before all generic provider/bridge scans, and every lifecycle-linked action/bridge is excluded from those generic owners. awaiting-boundary/prepared uses only durable zero-dispatch proof; restart loss of the predecessor volatile bridge never restores ready. dispatched/accepted/ambiguous performs at most pair-keyed lookup. It adopts only exact activation/checkpoint evidence, accepts no-effect only under the closed proof above, supersedes drift and quarantines absent/malformed/crossed/conflict. It never dispatches, redispatches, reconstructs a secret or treats a resume reference as continuity.

Each adapter normalises provider context telemetry to `providerContextObservationV1 {sourceEventId: stable adapter event ID, providerGeneration: positive integer, contextRevision: nonnegative integer, evidenceDigest: sha256 digest}` before lifecycle logic. Revision is monotonic only within its provider generation; a jump is legal and no `+1` assumption exists. The daemon stores one high-water pair per run/agent/provider generation and classifies each authenticated `sourceEventId` exactly once:

1. lower provider generation, or the same generation with lower context
   revision: append `reordered-observation` audit evidence and make no lifecycle,    bridge, high-water or receipt mutation;
2. equal generation and equal context revision: exact replay, no mutation;
3. equal generation and greater context revision: one `context-advance` loss
   whose `newContextRevision > oldContextRevision`; and
4. greater provider generation: one `generation-advance` loss regardless of its
   context revision, and install that revision as the new generation baseline.

The final database compare-and-set repeats this ordering, so simultaneous events, restart and a delayed callback cannot regress state. When both provider generation and context revision advance, only generation-advance exists. Unannounced provider compaction/generation advance with no active lifecycle custody has an explicit predecessor, never inferred null custody:

~~~yaml
lifecycleRecoverySourceV1:
  oneOf:
    - kind: custody
      custodyRef: exact-lifecycle-custody/revision
    - kind: generation-loss
      oldCustodyRef: null
      generationLossRef: exact-generation-loss/revision
      lossKind: generation-advance-or-context-advance
      oldProviderSessionRef: exact-session
      newProviderSessionRef: exact-observed-session
      oldProviderGeneration: positive-generation
      newProviderGeneration: positive-generation
      oldContextRevision: null-or-nonnegative-revision
      newContextRevision: nonnegative-observed-revision
      sourceBridgeRef: exact-bridge/revision
      sourceCapabilityHash: exact-hash
      checkpointState: absent-or-invalid-or-last-validated
      checkpointRef: null-or-exact-checkpoint
      checkpointDigest: null-or-sha256-prefixed-digest
      lossEvidenceDigest: sha256-prefixed-digest
~~~

generation-advance is canonical whenever the new provider generation is greater than old, including when context revision also changes. context-advance requires equal provider generations and a strictly greater proved new context revision. The arms are therefore disjoint. checkpoint ref/digest are both non-null only for last- validated and both null for absent/invalid.

Detection equality-checks that no custody owns the transition, inserts one immutable generation-loss row, revokes/fences the observed bridge/capability, CAS-ratchets only provider/context high-water from this telemetry, quarantines writes, turns and delivery claims, sets context-unreconciled and assigns LifecycleRotationRecoveryService before generic scans. Repeated source event is idempotent and returns its existing classification/audit row. Principal and bridge high-water may advance only from authenticated daemon custody reservation/adoption inputs that name those exact generations; they are never inferred from provider generation or context revision. The loss arm permits no agent-requested rotation, Resume or pair lookup that could bless the unannounced generation; only the exact operator fresh-rotate/abandon paths below can close it.

Generation-loss edges are `open -> recovery-in-progress -> recovered-adopted`, `recovery-in-progress -> abandoned`, `recovery-in-progress -> open` and direct `open -> abandoned`. fresh-rotate binds its new custody and canonical provider action pair to the loss and moves open to recovery-in-progress. Only adopted custody atomically records recovered-adopted and clears loss freezes. A no-effect/quarantined/ superseded custody returns the loss to open (the `recovery-in-progress -> open` edge) with immutable attempt history. Direct-open abandon records `abandonKind: direct-open` and `recoveryActionRef: null`; abandon after a recovery attempt records `abandonKind: recovery-attempt` and that custody's exact `{adapterId, actionId}` pair. Crossed null/discriminator/pair combinations are invalid. Both terminal arms perform the same owner-row cleanup below; no action is fabricated for direct abandon.

Lifecycle custody is the sole owner even when the rotating agent is the true chair. ChairBridgeLossRecoveryService excludes any chair with nonfinal custody, an open generation-loss row or a finalized nonadopted lifecycle-recovery marker; no chair_bridge_loss row is created for that bridge. Ownership ends only at lifecycle adoption or confirmed abandon. Child custody cannot promote a chair, and generic Resume cannot own either case.

A stranded suspended/context-unreconciled agent has one reachable operator surface: the closed agent-lifecycle-recovery intent on fabric.v1.operator-action.preview/commit/status/reconcile. Before Preview, the private local control plane may issue an agent-lifecycle-recovery-takeover capability only to the same authenticated local operator holding an exact current session capability containing agent-lifecycle-recovery-issue and one independently attested consequential gate bound to this recovery. Its immutable issuance row binds operator/project/ session/run/agent, session/run revisions and generations, one exact lifecycleRecoverySourceV1 arm, current validated checkpoint digest, exact source session/capability/ action/adapter/contract/bridge-row identity and revisions, provider/principal/ bridge generations, current chair-lease generation when applicable, bridge- owner kind, fresh-rotate only, gate, issue/expiry and capability hash. Status is active, commit-pending, consumed, revoked or expired; a handoff without commit is commit-pending and freezes later expiry/revocation until exact apply or explicit integrity recovery (see the lifecycle-receipt contract). Neither a generic session grant nor broad takeover reaches fresh-rotate Commit directly.

The intent additionally binds one closed path:

- fresh-rotate requires that narrow active capability and binds the replacement
  adapter, activated contract digest, distinct new canonical provider action   pair and the exact   current daemon-validated checkpoint row/vector. For an absent/invalid loss   checkpoint it additionally requires an exact existing checkpoint artifact   accepted by the read-only   fabric.v1.agent-lifecycle-recovery-checkpoint.validate operation under the   recovery gate; without one, fresh-rotate rejects and only abandon is   reachable. Commit consumes the issue,   reserves new high-water targets and creates one distinct awaiting-boundary   custody/capability/challenge with an empty caller-turn exception and immutable   recovery-from custody-or-generation-loss link. If a referenced old custody is   nonfinal, Commit may take its legal   superseded edge; if it is already finalized no-effect/quarantined/superseded,   its row/disposition remains unchanged. It calls neither old nor new provider;   the lifecycle owner dispatches later after the boundary.
- abandon requires exact session cancel authority plus consequential-gate and
  independently attested destructive direct-human confirmation. In one   transaction it moves a nonfinal custody through its legal abandoned edge, or   preserves an already-final custody and appends a distinct immutable lifecycle-   recovery-retirement row; an open generation-loss source takes direct-open   abandon with a null recovery action, while a recovery-in-progress source   takes recovery-attempt abandon with its exact action pair. It archives the   agent; revokes old and staged   capabilities, principal and bridge; terminally revokes turn leases;   changes quarantined write leases to revoked-abandoned without a write;   terminally abandons every owned or sole-recipient ready/claimed delivery,   task owner lease, required result obligation and agent/task/run membership   with reason; advances their message/delivery membership watermarks; and   terminalises dependent owned barriers as abandoned-failure, never success.   It appends revocations for active grants without mutating immutable authority   envelopes and clears only freeze contributions whose exact owned rows are now   terminal. No delivery or barrier is orphaned. Child abandon leaves unrelated   run work intact and makes any affected parent explicitly failed/recovery-   required; chair abandon enters the existing explicit run/session cancel-   failure terminal path in the same transaction.

Preview performs no lifecycle/provider mutation. Status returns the current intent, issuance and custody state. Reconcile uses pair lookup only for a new action that may have dispatched. Wrong/stale checkpoint, adapter/contract, action, source/high-water, capability issue or confirmation changes nothing.

- **FR-061:** rotate/compact shall return immutable accepted-suspended after
  durable boundary fencing; asynchronous custody alone may swap   provider/principal/bridge generations.
- **NFR-031:** nonterminal custody shall restrict predecessor/staged
  capabilities as above, quarantine writes before checkpoint binding, wait for   every captured turn and recover as the sole owner before generic scans   without replay.
- **FR-068:** An open generation loss shall support direct confirmed abandon
  with a null recovery action reference; attempted-recovery abandon shall carry   the exact provider action pair and a distinct provenance discriminator.
- **NFR-032:** Adapter-normalised context revision shall be nonnegative and
  monotonic within one provider generation; reordered/lower observations shall   be audited without lifecycle mutation.
- **FR-075:** Lifecycle custody and launch.attest shall bind the exact open-work
  set so accepted/unconsumed result obligations survive fresh-context adoption.
- **FR-076:** Final no-effect/superseded custody shall restore ready while
  releasing only owned freezes; quarantined custody shall remain suspended and   recovery-required.
- **NFR-033:** Provider observation replay shall be naturally idempotent by
  stable source event, and provider telemetry shall never infer principal or   bridge generations.
- **AC-051:** crash tests cover awaiting-boundary, every provider/custody state,
  every legal edge/disposition, unique caller inference, terminal predecessor   ordering, private handoff, launch.attest attribution, pre-CAS retained bridge,   child/true-chair owner swap and post-commit old-bridge retirement. They prove   accepted-versus-current-read separation, durable high-water-plus-one targets   across A-to-B supersession, global identity versus owner-scoped bridge   sequences, exact source-row/reserved-generation CAS, delivery-cut successor-   pending enqueue without checkpoint starvation, open-work handoff across   compaction, pre/post-dispatch no-effect distinction, ready restoration for   no-effect/superseded, suspended recovery for quarantine, retained write   quarantine and generic-recovery exclusion.   Unannounced compaction fixtures prove the fully bound generation-loss union   arm, classify simultaneous provider/context advance only as generation-   advance, and reject absent/null inference and generic Resume. Delivery   fixtures prove successor-pending remains stored/counted ready for custody,   open loss and exact linked loss/custody owners, becomes claimable on adoption,   becomes abandoned on retirement and rejects crossed/multiple owners. Operator fixtures prove reachable   parent-grant/gate and narrow-capability fresh-rotate with distinct custody/   action/adapter/contract, empty caller boundary and finalized-predecessor   immutability; confirmed abandon proves exact delivery/watermark/barrier and   other owner-row terminal transitions without orphaning required work;   self-rotate, generic resume and chair-loss recovery cannot bypass the sole   lifecycle owner.
- **AC-054:** context fixtures cover restart, exact duplicate, lower provider
  generation, lower same-generation revision, arbitrary forward jump and   simultaneous provider/context advance. Only same-generation strict increase   creates context-advance and its receipt always has   `newContextRevision > oldContextRevision`; greater provider generation wins.   Direct-open abandon persists null action/direct-open provenance, while   recovery-attempt abandon persists the exact adapter/action pair, and every   crossed discriminator/nullability combination fails atomically.
- **AC-055:** policy-required rotation starts a distinct fresh provider context
  and injects only the bounded canonical checkpoint/handoff after adoption;   same-history attach/resume passes crash-recovery fixtures but fails rotation.   Duplicate/lower/reordered context callbacks with the same source event create   one bounded audit row, and provider values cannot ratchet principal/bridge   high-water.
