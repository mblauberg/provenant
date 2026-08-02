import {
  arrayOf,
  boundedString,
  defineCodec,
  enumeration,
  integer,
  literal,
  nullable,
  objectCodec,
  parserBacked,
  sha256,
  unionOf,
  type CodecOutput,
} from "./codec.js";
import { PROVIDER_ACTION_REF_V1_CODEC } from "./launch.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const providerFailureCode = enumeration([
  "max-turns-exhausted",
  "provider-rejected",
  "terminal-no-answer",
  "adapter-terminal-failure",
]);

export const REVIEW_SLOTS = ["native", "other-primary", "cursor-grok", "agy-gemini"] as const;

export const TOP_REVIEW_BLOCKERS = [
  "certifying-review-capability-unavailable",
  "finding-capacity-exhausted",
  "missing-target",
  "stale-target",
  "profile-unavailable",
  "integrity-failure",
] as const;
export const SLOT_REVIEW_BLOCKERS = [
  "missing-evidence",
  "nonterminal-action",
  "ambiguous-action",
  "provider-terminal-failure",
  "terminal-no-effect",
  "retired-unknown",
  "route-integrity",
  "insufficient-read-coverage",
  "noncertifying",
  "actual-route-mismatch",
  "actual-route-unproved",
  "unusable",
  "wrong-artifact",
  "wrong-bundle",
  "wrong-route",
  "wrong-provider",
  "wrong-model",
  "wrong-chair-generation",
  "reviewer-family-distinctness",
  "open-findings",
] as const;
export const TERMINAL_RESULT_IDENTITY_V1_CODEC = unionOf([
  objectCodec({
    schemaVersion: literal(1),
    actionRef: PROVIDER_ACTION_REF_V1_CODEC,
    terminalSequence: positive,
    terminalKind: literal("safe-answer"),
    providerAnswerDigest: sha256,
    reviewResultDigest: sha256,
    answerSafety: literal("safe"),
    readCoverageDigest: sha256,
    coverageSummaryDigest: sha256,
  }),
  objectCodec({
    schemaVersion: literal(1),
    actionRef: PROVIDER_ACTION_REF_V1_CODEC,
    terminalSequence: positive,
    terminalKind: literal("unusable-answer"),
    providerAnswerDigest: sha256,
    reviewResultDigest: literal(null),
    answerSafety: literal("unusable"),
    readCoverageDigest: sha256,
    coverageSummaryDigest: sha256,
  }),
  objectCodec({
    schemaVersion: literal(1),
    actionRef: PROVIDER_ACTION_REF_V1_CODEC,
    terminalSequence: positive,
    terminalKind: literal("provider-terminal-failure"),
    providerFailureCode,
    providerFailureDigest: sha256,
  }),
  objectCodec({
    schemaVersion: literal(1),
    actionRef: PROVIDER_ACTION_REF_V1_CODEC,
    terminalSequence: positive,
    terminalKind: literal("terminal-no-effect"),
    noEffectEvidenceDigest: sha256,
  }),
  objectCodec({
    schemaVersion: literal(1),
    actionRef: PROVIDER_ACTION_REF_V1_CODEC,
    terminalSequence: positive,
    terminalKind: literal("integrity-terminal"),
    integrityEvidenceDigest: sha256,
  }),
  objectCodec({
    schemaVersion: literal(1),
    actionRef: PROVIDER_ACTION_REF_V1_CODEC,
    terminalSequence: positive,
    terminalKind: literal("retired-unknown"),
    retirementEvidenceDigest: sha256,
  }),
]);

const reviewResultFindingCommon = {
  findingId: boundedString({ maxBytes: 64, example: "finding_01" }),
  severity: enumeration(["P0", "P1", "P2"]),
  summary: boundedString({ maxBytes: 256, example: "Finding summary" }),
  evidence: boundedString({ maxBytes: 768, example: "Finding evidence" }),
} as const;
const reviewResultFindingCodec = unionOf([
  objectCodec({ ...reviewResultFindingCommon, repairKind: literal("repository-source"), evidenceRefs: arrayOf(id256, { maximum: 0 }) }),
  objectCodec({ ...reviewResultFindingCommon, repairKind: literal("registered-evidence"), evidenceRefs: arrayOf(id256, { minimum: 1, maximum: 1024, unique: true }) }),
  objectCodec({ ...reviewResultFindingCommon, repairKind: literal("mixed"), evidenceRefs: arrayOf(id256, { minimum: 1, maximum: 1024, unique: true }) }),
]);
const reviewResultCommon = {
  schemaVersion: literal(1),
  targetGeneration: positive,
  coverageDigest: sha256,
} as const;
export const REVIEW_RESULT_V1_CODEC = unionOf([
  objectCodec({
    ...reviewResultCommon,
    findingWindowMode: literal("normal"),
    verdict: literal("CLEAN"),
    resolvedFindingDigests: arrayOf(sha256, { maximum: 16_384, unique: true }),
    findings: arrayOf(reviewResultFindingCodec, { maximum: 0 }),
  }),
  objectCodec({
    ...reviewResultCommon,
    findingWindowMode: literal("normal"),
    verdict: literal("FINDINGS"),
    resolvedFindingDigests: arrayOf(sha256, { maximum: 16_384, unique: true }),
    findings: arrayOf(reviewResultFindingCodec, { minimum: 1, maximum: 32, unique: true }),
  }),
  objectCodec({
    ...reviewResultCommon,
    findingWindowMode: literal("resolution-only"),
    verdict: literal("CLEAN"),
    resolvedFindingDigests: arrayOf(sha256, { maximum: 32, unique: true }),
    findings: arrayOf(reviewResultFindingCodec, { maximum: 0 }),
  }),
]);

export const REVIEW_TARGET_PREPARE_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  commandId: id256,
  taskId: id256,
  expectedTargetGeneration: nonnegative,
  deliveryManifestRef: id256,
});
export const REVIEW_TARGET_PREPARATION_ACCEPTED_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  preparationId: id256,
  ownerCommandId: id256,
  inputDigest: sha256,
  projectSessionId: id256,
  coordinationRunId: id256,
  taskId: id256,
  expectedTargetGeneration: nonnegative,
  reservedTargetGeneration: positive,
  reservedBundleGeneration: positive,
  deliveryManifestRef: id256,
  state: literal("prepared"),
  acceptedReceiptDigest: sha256,
});
export const REVIEW_TARGET_PREPARATION_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  preparationId: id256,
});
export const REVIEW_TARGET_PREPARATION_READ_ERROR_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  code: enumeration(["REVIEW_TARGET_PREPARATION_NOT_FOUND", "AUTHORITY_DENIED", "SCOPE_MISMATCH", "INTEGRITY_FAILURE"]),
});
const preparationProgressCodec = unionOf([
  objectCodec({ kind: literal("phase-only") }),
  objectCodec({
    kind: literal("finite"),
    unit: literal("verified-build-items"),
    completed: nonnegative,
    total: positive,
    planDigest: sha256,
  }),
]);
const preparationTerminalCodec = nullable(unionOf([
  objectCodec({ kind: literal("succeeded"), targetRef: positive }),
  objectCodec({
    kind: literal("conflicted"),
    code: enumeration([
      "target-generation-changed",
      "chair-binding-changed",
      "task-or-authority-changed",
      "delivery-basis-changed",
      "repository-source-changed",
      "profile-changed",
      "predecessor-head-changed",
      "predecessor-action-nonterminal",
    ]),
    evidenceDigest: sha256,
  }),
  objectCodec({
    kind: literal("failed"),
    code: enumeration([
      "bundle-too-large",
      "unsupported-repository-state",
      "source-read-failed",
      "content-integrity-failed",
      "certifying-capability-unavailable",
    ]),
    evidenceDigest: sha256,
  }),
]));
const preparationReadBaseCodec = objectCodec({
  schemaVersion: literal(1),
  accepted: REVIEW_TARGET_PREPARATION_ACCEPTED_V1_CODEC,
  revision: positive,
  state: enumeration(["prepared", "building", "built", "succeeded", "conflicted", "failed"]),
  phase: enumeration(["Preparing", "Building", "Committing", "Succeeded", "Conflicted", "Failed"]),
  progress: preparationProgressCodec,
  terminal: preparationTerminalCodec,
});
export const REVIEW_TARGET_PREPARATION_READ_V1_CODEC = parserBacked(
  defineCodec(
    { ...preparationReadBaseCodec.schema, "x-reviewPreparationCorrelated": true },
    preparationReadBaseCodec.example,
    (input, path) => preparationReadBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const terminal = record.terminal as Readonly<Record<string, unknown>> | null;
    const phases = {
      prepared: "Preparing",
      building: "Building",
      built: "Committing",
      succeeded: "Succeeded",
      conflicted: "Conflicted",
      failed: "Failed",
    } as const;
    if (record.phase !== phases[record.state as keyof typeof phases]) {
      throw new TypeError(`${path}.phase must exactly match state`);
    }
    const terminalState = record.state === "succeeded" || record.state === "conflicted" || record.state === "failed";
    if (terminalState !== (terminal !== null) || (terminal !== null && terminal.kind !== record.state)) {
      throw new TypeError(`${path}.terminal must exactly match terminal state`);
    }
    const progress = record.progress as Readonly<Record<string, unknown>>;
    if (progress.kind === "finite" && Number(progress.completed) > Number(progress.total)) {
      throw new TypeError(`${path}.progress.completed must not exceed total`);
    }
    if ((record.state === "built" || record.state === "succeeded") && progress.kind === "finite" && progress.completed !== progress.total) {
      throw new TypeError(`${path}.progress must be complete when built or succeeded`);
    }
    return record;
  },
  preparationReadBaseCodec.example,
);

export const LIFECYCLE_CUSTODY_REF_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  runId: id256,
  agentId: id256,
  custodyId: id256,
  custodyRevision: positive,
});
export const REVIEW_TARGET_REBIND_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  commandId: id256,
  targetGeneration: positive,
  expectedChairBindingGeneration: positive,
  lifecycleCustodyRef: LIFECYCLE_CUSTODY_REF_V1_CODEC,
});
export const REVIEW_TARGET_REBIND_RECEIPT_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  status: literal("rebound"),
  targetGeneration: positive,
  reviewSubjectDigest: sha256,
  priorBindingGeneration: positive,
  newBindingGeneration: positive,
  priorBindingDigest: sha256,
  newBindingDigest: sha256,
  lifecycleAdoptionDigest: sha256,
  bundleDigest: sha256,
  profileDigest: sha256,
  slotHeadSetDigest: sha256,
  openAndRepairFindingSetDigest: sha256,
  rebindReceiptDigest: sha256,
});


export type TerminalResultIdentityV1 = CodecOutput<typeof TERMINAL_RESULT_IDENTITY_V1_CODEC>;
export type ReviewTargetPrepareV1 = CodecOutput<typeof REVIEW_TARGET_PREPARE_V1_CODEC>;
export type ReviewTargetPreparationAcceptedV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_ACCEPTED_V1_CODEC>;
export type ReviewTargetPreparationReadRequestV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_READ_REQUEST_V1_CODEC>;
export type ReviewTargetPreparationReadV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_READ_V1_CODEC>;
export type ReviewTargetPreparationReadErrorV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_READ_ERROR_V1_CODEC>;
export type ReviewTargetRebindV1 = CodecOutput<typeof REVIEW_TARGET_REBIND_V1_CODEC>;
export type ReviewTargetRebindReceiptV1 = CodecOutput<typeof REVIEW_TARGET_REBIND_RECEIPT_V1_CODEC>;
