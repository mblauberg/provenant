import {
  arrayOf,
  boolean,
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
import {
  PROVIDER_IDENTITY_ASSURANCE_V1_CODEC,
  supportsCertifyingAnswerBearingLeg,
  type ProviderIdentityAssurance,
} from "./provider-assurance.js";
import { LOCAL_PROVIDER_ROUTE_V1_CODEC } from "./route-lineage.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const nullableId = nullable(id256);
const nullableDigest = nullable(sha256);
const cursor = nullable(boundedString({ maxBytes: 256, example: "cursor_01" }));

export const REVIEW_SLOTS = ["native", "other-primary", "cursor-grok", "agy-gemini"] as const;
const reviewSlot = enumeration(REVIEW_SLOTS);
const providerFailureCode = enumeration([
  "max-turns-exhausted",
  "provider-rejected",
  "terminal-no-answer",
  "adapter-terminal-failure",
]);

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
const reviewCurrencyBlocker = enumeration([
  ...TOP_REVIEW_BLOCKERS,
  ...SLOT_REVIEW_BLOCKERS,
  "superseded",
]);

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

const evidenceCurrencyRefCodec = objectCodec({ evidenceRef: id256, evidenceRevision: positive, contentDigest: sha256 });
const repairCurrencyBaseCodec = unionOf([
  objectCodec({
    kind: literal("repository-source"),
    originRepositorySourceStateDigest: sha256,
    evidenceRefs: arrayOf(evidenceCurrencyRefCodec, { maximum: 0 }),
  }),
  objectCodec({
    kind: literal("registered-evidence"),
    originRepositorySourceStateDigest: literal(null),
    evidenceRefs: arrayOf(evidenceCurrencyRefCodec, { minimum: 1, maximum: 1024, unique: true }),
  }),
  objectCodec({
    kind: literal("mixed"),
    originRepositorySourceStateDigest: sha256,
    evidenceRefs: arrayOf(evidenceCurrencyRefCodec, { minimum: 1, maximum: 1024, unique: true }),
  }),
]);
export const REPAIR_CURRENCY_V1_CODEC = parserBacked(
  defineCodec(
    { ...repairCurrencyBaseCodec.schema, "x-repairCurrencyOrdered": true },
    repairCurrencyBaseCodec.example,
    (input, path) => repairCurrencyBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const refs = record.evidenceRefs as readonly Readonly<Record<string, unknown>>[];
    for (let index = 1; index < refs.length; index += 1) {
      const previous = refs[index - 1];
      const current = refs[index];
      if (previous === undefined || current === undefined) throw new TypeError(`${path}.evidenceRefs has an invalid gap`);
      const idOrder = Buffer.compare(Buffer.from(String(previous.evidenceRef), "utf8"), Buffer.from(String(current.evidenceRef), "utf8"));
      if (idOrder >= 0) {
        throw new TypeError(`${path}.evidenceRefs must use unique evidenceRef UTF-8 order`);
      }
    }
    return record;
  },
  repairCurrencyBaseCodec.example,
);
export const SAFE_FINDING_V1_CODEC = objectCodec({
  findingDigest: sha256,
  findingId: boundedString({ maxBytes: 64, example: "finding_01" }),
  severity: enumeration(["P0", "P1", "P2"]),
  summary: boundedString({ maxBytes: 256, example: "Finding summary" }),
  evidence: boundedString({ maxBytes: 768, example: "Finding evidence" }),
  originTargetGeneration: positive,
  originActionRef: PROVIDER_ACTION_REF_V1_CODEC,
  originResultDigest: sha256,
  originDeliveryManifestRef: id256,
  originDeliveryReviewBasisDigest: sha256,
  originBundleDigest: sha256,
  repairCurrency: REPAIR_CURRENCY_V1_CODEC,
});

const coverageGroupCodec = parserBacked(
  objectCodec({
    groupId: enumeration([
      "security-auth",
      "protocol-schema",
      "persistence-migration",
      "provider-adapter",
      "console-ui",
      "tests-evaluations",
      "documentation",
      "generated-other",
    ]),
    totalCount: nonnegative,
    readCount: nonnegative,
    unreadCount: nonnegative,
    unreadObjectSetDigest: sha256,
  }),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    if (Number(record.readCount) + Number(record.unreadCount) !== record.totalCount) {
      throw new TypeError(`${path}.totalCount must equal readCount plus unreadCount`);
    }
    return record;
  },
  {
    groupId: "security-auth",
    totalCount: 1,
    readCount: 1,
    unreadCount: 0,
    unreadObjectSetDigest: sha256.example,
  },
);
const coverageSummaryBaseCodec = objectCodec({
  mode: literal("manifest-complete-risk-directed"),
  mandatoryComplete: boolean,
  groups: arrayOf(coverageGroupCodec, { maximum: 8, unique: true }),
  byteComplete: boolean,
});
export const COVERAGE_SUMMARY_V1_CODEC = parserBacked(
  defineCodec(
    { ...coverageSummaryBaseCodec.schema, "x-coverageGroupsOrdered": true },
    coverageSummaryBaseCodec.example,
    (input, path) => coverageSummaryBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const groups = (value as Readonly<Record<string, unknown>>).groups as readonly Readonly<Record<string, unknown>>[];
    for (let index = 1; index < groups.length; index += 1) {
      if (String(groups[index - 1]?.groupId) >= String(groups[index]?.groupId)) {
        throw new TypeError(`${path}.groups must be strictly ascending and unique by groupId`);
      }
    }
    return value;
  },
  coverageSummaryBaseCodec.example,
);
const findingSetRefBaseCodec = objectCodec({
  findingSetDigest: sha256,
  findingCount: nonnegative,
  pageDigests: arrayOf(sha256, { maximum: 16_384, unique: true }),
});
export const FINDING_SET_REF_V1_CODEC = parserBacked(
  findingSetRefBaseCodec,
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const pages = record.pageDigests as readonly unknown[];
    if ((record.findingCount === 0) !== (pages.length === 0)) throw new TypeError(`${path} zero finding count must match empty page list`);
    return record;
  },
  { ...findingSetRefBaseCodec.example, findingCount: 0, pageDigests: [] },
);
const findingWindowCodec = unionOf([
  objectCodec({
    mode: literal("normal"),
    maximumNewFindings: literal(32),
    maximumNewFindingBytes: positive,
    capacityReservationDigest: sha256,
  }),
  objectCodec({
    mode: literal("resolution-only"),
    maximumNewFindings: literal(0),
    maximumNewFindingBytes: literal(0),
    capacityReservationDigest: sha256,
  }),
]);
const reviewCertificationBasisBaseCodec = unionOf([
  objectCodec({
    kind: literal("active-binding"),
    actionBindingGeneration: positive,
    activeBindingGeneration: positive,
    terminalSequence: positive,
    bindingChainDigest: sha256,
  }),
  objectCodec({
    kind: literal("predecessor-cut"),
    actionBindingGeneration: positive,
    firstSuccessorBindingGeneration: positive,
    activeBindingGeneration: positive,
    terminalSequence: positive,
    certificationCutSequence: nonnegative,
    certificationCutCustodyRef: LIFECYCLE_CUSTODY_REF_V1_CODEC,
    certificationCutDigest: sha256,
    bindingChainDigest: sha256,
  }),
  objectCodec({
    kind: literal("post-cut"),
    actionBindingGeneration: positive,
    firstSuccessorBindingGeneration: positive,
    activeBindingGeneration: positive,
    terminalSequence: positive,
    certificationCutSequence: nonnegative,
    certificationCutCustodyRef: LIFECYCLE_CUSTODY_REF_V1_CODEC,
    certificationCutDigest: sha256,
    bindingChainDigest: sha256,
  }),
]);
export const REVIEW_CERTIFICATION_BASIS_V1_CODEC = parserBacked(
  defineCodec(
    { ...reviewCertificationBasisBaseCodec.schema, "x-reviewCertificationBasisCorrelated": true },
    reviewCertificationBasisBaseCodec.example,
    (input, path) => reviewCertificationBasisBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const basis = value as Readonly<Record<string, unknown>>;
    if (basis.kind === "active-binding" && basis.actionBindingGeneration !== basis.activeBindingGeneration) {
      throw new TypeError(`${path}.actionBindingGeneration must equal activeBindingGeneration`);
    }
    if (basis.kind === "predecessor-cut" && Number(basis.terminalSequence) > Number(basis.certificationCutSequence)) {
      throw new TypeError(`${path}.terminalSequence must be at or before certificationCutSequence`);
    }
    if (basis.kind === "post-cut" && Number(basis.terminalSequence) <= Number(basis.certificationCutSequence)) {
      throw new TypeError(`${path}.terminalSequence must be after certificationCutSequence`);
    }
    return value;
  },
  reviewCertificationBasisBaseCodec.example,
);

export const PROVIDER_ROUTE_PROJECTION_V1_CODEC = LOCAL_PROVIDER_ROUTE_V1_CODEC;

const reviewEvidenceMutationReceiptBaseCodec = objectCodec({
  schemaVersion: literal(1),
  evidenceId: id256,
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  terminalSequence: positive,
  targetGeneration: positive,
  slot: reviewSlot,
  attemptGeneration: positive,
  priorHeadGeneration: nonnegative,
  newHeadGeneration: positive,
  priorEvidenceId: nullableId,
  terminalResultDigest: sha256,
  terminalInputDigest: sha256,
  reportedResolvedSetDigest: sha256,
  acceptedResolvedSetDigest: sha256,
  findingSetDigest: sha256,
  newOpenSetDigest: sha256,
  repairRequiredSetDigest: sha256,
  readCoverageDigest: sha256,
  coverageSummaryDigest: sha256,
  findingWindowDigest: sha256,
  certificationBasisAtTerminalDigest: sha256,
  mutationReceiptDigest: sha256,
});
function assertLinearEvidenceHeadTuple(record: Readonly<Record<string, unknown>>, path: string): void {
  if (record.newHeadGeneration !== Number(record.priorHeadGeneration) + 1) {
    throw new TypeError(`${path}.newHeadGeneration must equal priorHeadGeneration + 1`);
  }
  if ((record.priorHeadGeneration === 0) !== (record.priorEvidenceId === null)) {
    throw new TypeError(`${path}.priorEvidenceId must be null exactly for generation-zero prior head`);
  }
}
export const REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC = parserBacked(
  defineCodec(
    { ...reviewEvidenceMutationReceiptBaseCodec.schema, "x-reviewEvidenceMutationReceiptCorrelated": true },
    reviewEvidenceMutationReceiptBaseCodec.example,
    (input, path) => reviewEvidenceMutationReceiptBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    assertLinearEvidenceHeadTuple(record, path);
    return record;
  },
  {
    ...reviewEvidenceMutationReceiptBaseCodec.example,
    priorHeadGeneration: 0,
    newHeadGeneration: 1,
    priorEvidenceId: null,
  },
);

const terminalReviewCommon = {
  terminalSequence: positive,
  terminalResultDigest: sha256,
  currentCertificationBasis: nullable(REVIEW_CERTIFICATION_BASIS_V1_CODEC),
  certifying: boolean,
};
const terminalReviewCodec = (required: Parameters<typeof objectCodec>[0]) => objectCodec(required, {
  providerAssurance: PROVIDER_IDENTITY_ASSURANCE_V1_CODEC,
});
const terminalReviewProjectionBaseCodec = unionOf([
  terminalReviewCodec({
    kind: literal("safe-answer"),
    ...terminalReviewCommon,
    providerAnswerDigest: sha256,
    reviewResultDigest: sha256,
    verdict: enumeration(["CLEAN", "FINDINGS"]),
    failureCode: literal(null),
    noEffectEvidenceDigest: literal(null),
    integrityEvidenceDigest: literal(null),
    retirementEvidenceDigest: literal(null),
    readCoverageDigest: sha256,
    coverageSummaryDigest: sha256,
  }),
  terminalReviewCodec({
    kind: literal("unusable-answer"),
    ...terminalReviewCommon,
    providerAnswerDigest: sha256,
    reviewResultDigest: literal(null),
    verdict: literal("UNUSABLE"),
    failureCode: literal(null),
    noEffectEvidenceDigest: literal(null),
    integrityEvidenceDigest: literal(null),
    retirementEvidenceDigest: literal(null),
    readCoverageDigest: sha256,
    coverageSummaryDigest: sha256,
  }),
  terminalReviewCodec({
    kind: literal("provider-terminal-failure"),
    ...terminalReviewCommon,
    providerAnswerDigest: literal(null),
    reviewResultDigest: literal(null),
    verdict: literal(null),
    failureCode: providerFailureCode,
    noEffectEvidenceDigest: literal(null),
    integrityEvidenceDigest: literal(null),
    retirementEvidenceDigest: literal(null),
    readCoverageDigest: literal(null),
    coverageSummaryDigest: literal(null),
  }),
  terminalReviewCodec({
    kind: literal("terminal-no-effect"),
    ...terminalReviewCommon,
    providerAnswerDigest: literal(null),
    reviewResultDigest: literal(null),
    verdict: literal(null),
    failureCode: literal(null),
    noEffectEvidenceDigest: sha256,
    integrityEvidenceDigest: literal(null),
    retirementEvidenceDigest: literal(null),
    readCoverageDigest: literal(null),
    coverageSummaryDigest: literal(null),
  }),
  terminalReviewCodec({
    kind: literal("integrity-terminal"),
    ...terminalReviewCommon,
    providerAnswerDigest: literal(null),
    reviewResultDigest: literal(null),
    verdict: literal(null),
    failureCode: literal(null),
    noEffectEvidenceDigest: literal(null),
    integrityEvidenceDigest: sha256,
    retirementEvidenceDigest: literal(null),
    readCoverageDigest: literal(null),
    coverageSummaryDigest: literal(null),
  }),
  terminalReviewCodec({
    kind: literal("retired-unknown"),
    ...terminalReviewCommon,
    providerAnswerDigest: literal(null),
    reviewResultDigest: literal(null),
    verdict: literal(null),
    failureCode: literal(null),
    noEffectEvidenceDigest: literal(null),
    integrityEvidenceDigest: literal(null),
    retirementEvidenceDigest: sha256,
    readCoverageDigest: literal(null),
    coverageSummaryDigest: literal(null),
  }),
]);
const terminalReviewProjectionCodec = parserBacked(
  terminalReviewProjectionBaseCodec,
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    if (record.certifying === true) {
      if (!supportsCertifyingAnswerBearingLeg(record.providerAssurance as ProviderIdentityAssurance)) {
        throw new TypeError(`${path}.certifying requires a certifying provider assurance`);
      }
      const basis = record.currentCertificationBasis as Readonly<Record<string, unknown>> | null;
      if (record.kind !== "safe-answer" || basis === null || basis.kind === "post-cut") {
        throw new TypeError(`${path}.certifying requires a safe answer with a current certifying basis`);
      }
    } else if (record.kind !== "safe-answer" && record.certifying !== false) {
      throw new TypeError(`${path}.certifying must be false for a non-safe terminal arm`);
    }
    return record;
  },
  {
    ...terminalReviewProjectionBaseCodec.example,
    currentCertificationBasis: REVIEW_CERTIFICATION_BASIS_V1_CODEC.example,
    certifying: true,
  },
);
const providerActionTerminalProjectionBaseCodec = objectCodec({
  schemaVersion: literal(1),
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  status: enumeration(["prepared", "dispatched", "accepted", "ambiguous", "terminal"]),
  originalDispatchReceiptDigest: sha256,
  routeState: enumeration(["present", "missing", "integrity-failed"]),
  route: nullable(PROVIDER_ROUTE_PROJECTION_V1_CODEC),
  routeRecoveryEvidenceDigest: nullableDigest,
  terminalReview: nullable(terminalReviewProjectionCodec),
  evidenceMutationReceipt: nullable(REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC),
});
export const PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC = parserBacked(
  defineCodec(
    { ...providerActionTerminalProjectionBaseCodec.schema, "x-providerActionTerminalCorrelated": true },
    providerActionTerminalProjectionBaseCodec.example,
    (input, path) => providerActionTerminalProjectionBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const present = record.routeState === "present";
    if (present !== (record.route !== null) || present === (record.routeRecoveryEvidenceDigest !== null)) {
      throw new TypeError(`${path}.route and routeRecoveryEvidenceDigest must match routeState`);
    }
    const terminal = record.terminalReview as Readonly<Record<string, unknown>> | null;
    if ((record.status === "terminal") !== (terminal !== null)) {
      throw new TypeError(`${path}.terminalReview must be nonnull exactly for terminal status`);
    }
    const mutation = record.evidenceMutationReceipt;
    const evidenceKind = terminal?.kind === "safe-answer" || terminal?.kind === "unusable-answer";
    if (evidenceKind !== (mutation !== null)) {
      throw new TypeError(`${path}.evidenceMutationReceipt must exist only for safe/unusable terminal answers`);
    }
    if (present) {
      const action = record.actionRef as Readonly<Record<string, unknown>>;
      const route = record.route as Readonly<Record<string, unknown>>;
      if (action.adapterId !== route.adapterId) throw new TypeError(`${path}.actionRef.adapterId must equal route.adapterId`);
      if (terminal !== null && terminal.providerAssurance !== route.providerAssurance) {
        throw new TypeError(`${path}.terminalReview.providerAssurance must equal route.providerAssurance`);
      }
    }
    if (evidenceKind && mutation !== null) {
      if (!present) throw new TypeError(`${path}.evidenceMutationReceipt requires a present route`);
      const action = record.actionRef as Readonly<Record<string, unknown>>;
      const route = record.route as Readonly<Record<string, unknown>>;
      const receipt = mutation as Readonly<Record<string, unknown>>;
      const receiptAction = receipt.actionRef as Readonly<Record<string, unknown>>;
      if (receiptAction.adapterId !== action.adapterId || receiptAction.actionId !== action.actionId) {
        throw new TypeError(`${path}.evidenceMutationReceipt.actionRef must equal actionRef`);
      }
      for (const field of ["targetGeneration", "slot", "attemptGeneration"] as const) {
        if (receipt[field] !== route[field]) throw new TypeError(`${path}.evidenceMutationReceipt.${field} must equal route.${field}`);
      }
      if (receipt.priorHeadGeneration !== route.slotHeadGeneration) {
        throw new TypeError(`${path}.evidenceMutationReceipt.priorHeadGeneration must equal route.slotHeadGeneration`);
      }
      for (const [receiptField, terminalField] of [
        ["terminalSequence", "terminalSequence"],
        ["terminalResultDigest", "terminalResultDigest"],
        ["readCoverageDigest", "readCoverageDigest"],
        ["coverageSummaryDigest", "coverageSummaryDigest"],
      ] as const) {
        if (receipt[receiptField] !== terminal?.[terminalField]) {
          throw new TypeError(`${path}.evidenceMutationReceipt.${receiptField} must equal terminalReview.${terminalField}`);
        }
      }
    }
    return record;
  },
  {
    ...providerActionTerminalProjectionBaseCodec.example,
    status: "prepared",
    routeState: "missing",
    route: null,
    routeRecoveryEvidenceDigest: sha256.example,
    terminalReview: null,
    evidenceMutationReceipt: null,
  },
);

const reviewerFamilyRelation = enumeration([
  "same-family-exempt",
  "distinct-family-proved",
  "same-family-forbidden",
  "family-unproved",
]);
const reviewEvidenceRecordBaseCodec = objectCodec({
  evidenceId: id256,
  targetGeneration: positive,
  slot: reviewSlot,
  taskId: id256,
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  terminalSequence: positive,
  terminalKind: enumeration(["safe-answer", "unusable-answer"]),
  verdict: enumeration(["CLEAN", "FINDINGS", "UNUSABLE"]),
  answerSafety: enumeration(["safe", "unusable"]),
  providerAnswerDigest: sha256,
  terminalResultDigest: sha256,
  reviewResultDigest: nullableDigest,
  providerFailureCode: literal(null),
  providerFailureDigest: literal(null),
  routeReceiptDigest: sha256,
  routeObservationDigest: nullableDigest,
  actualRouteIdentityDigest: nullableDigest,
  finalPromptDigest: sha256,
  adapterId: id256,
  endpointProvider: id256,
  providerFamily: id256,
  model: id256,
  bundleDigest: sha256,
  coverageDigest: sha256,
  profileDigest: sha256,
  priorHeadGeneration: nonnegative,
  newHeadGeneration: positive,
  attemptGeneration: positive,
  priorEvidenceId: nullableId,
  priorOpenFindingSet: FINDING_SET_REF_V1_CODEC,
  reportedResolvedFindingDigests: arrayOf(sha256, { maximum: 16_384, unique: true }),
  acceptedResolvedFindingDigests: arrayOf(sha256, { maximum: 16_384, unique: true }),
  findingSet: FINDING_SET_REF_V1_CODEC,
  newOpenFindingSet: FINDING_SET_REF_V1_CODEC,
  repairRequiredFindingSet: FINDING_SET_REF_V1_CODEC,
  findingWindow: findingWindowCodec,
  readCoverageDigest: sha256,
  coverageSummary: COVERAGE_SUMMARY_V1_CODEC,
  reviewerFamilyRelation,
  certificationBasisAtTerminal: REVIEW_CERTIFICATION_BASIS_V1_CODEC,
  mutationReceiptDigest: sha256,
}, {
  providerAssurance: PROVIDER_IDENTITY_ASSURANCE_V1_CODEC,
});
export const REVIEW_EVIDENCE_RECORD_V1_CODEC = parserBacked(
  defineCodec(
    { ...reviewEvidenceRecordBaseCodec.schema, "x-reviewEvidenceCorrelated": true },
    reviewEvidenceRecordBaseCodec.example,
    (input, path) => reviewEvidenceRecordBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const action = record.actionRef as Readonly<Record<string, unknown>>;
    if (record.adapterId !== action.adapterId) {
      throw new TypeError(`${path}.adapterId must equal actionRef.adapterId`);
    }
    if (record.actualRouteIdentityDigest !== null && record.routeObservationDigest === null) {
      throw new TypeError(`${path}.actualRouteIdentityDigest requires routeObservationDigest and proved actual route identity arms`);
    }
    assertLinearEvidenceHeadTuple(record, path);
    const terminalBasis = record.certificationBasisAtTerminal as Readonly<Record<string, unknown>>;
    if (terminalBasis.terminalSequence !== record.terminalSequence) {
      throw new TypeError(`${path}.certificationBasisAtTerminal.terminalSequence must equal terminalSequence`);
    }
    if (record.terminalKind === "safe-answer") {
      if (record.answerSafety !== "safe" || !["CLEAN", "FINDINGS"].includes(String(record.verdict)) || record.reviewResultDigest === null) {
        throw new TypeError(`${path}.safe-answer requires safe CLEAN/FINDINGS with reviewResultDigest`);
      }
    } else if (record.answerSafety !== "unusable" || record.verdict !== "UNUSABLE" || record.reviewResultDigest !== null) {
      throw new TypeError(`${path}.unusable-answer requires unusable UNUSABLE with null reviewResultDigest`);
    }
    return record;
  },
  {
    ...reviewEvidenceRecordBaseCodec.example,
    adapterId: (reviewEvidenceRecordBaseCodec.example.actionRef as Readonly<Record<string, unknown>>).adapterId,
    reviewResultDigest: sha256.example,
    priorHeadGeneration: 0,
    newHeadGeneration: 1,
    priorEvidenceId: null,
  },
);

const reviewEvidenceCurrencyBaseCodec = objectCodec({
  target: enumeration(["current", "stale", "superseded"]),
  source: enumeration(["current", "stale"]),
  chair: enumeration(["current", "stale"]),
  profile: enumeration(["current", "stale"]),
  currentCertificationBasis: nullable(REVIEW_CERTIFICATION_BASIS_V1_CODEC),
  certifying: boolean,
  blockerCodes: arrayOf(reviewCurrencyBlocker, { maximum: 32, unique: true }),
}, {
  providerAssurance: PROVIDER_IDENTITY_ASSURANCE_V1_CODEC,
});
const reviewCurrencyBlockerOrder = [
  ...TOP_REVIEW_BLOCKERS,
  ...SLOT_REVIEW_BLOCKERS,
  "superseded",
] as const;
function requireEnumOrder(values: readonly unknown[], order: readonly string[], path: string): void {
  let previous = -1;
  for (const value of values) {
    const rank = order.indexOf(String(value));
    if (rank <= previous) throw new TypeError(`${path} must use canonical deterministic order`);
    previous = rank;
  }
}
export const REVIEW_EVIDENCE_CURRENCY_V1_CODEC = parserBacked(
  defineCodec(
    { ...reviewEvidenceCurrencyBaseCodec.schema, "x-reviewEvidenceCurrencyCorrelated": true },
    reviewEvidenceCurrencyBaseCodec.example,
    (input, path) => reviewEvidenceCurrencyBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const blockers = record.blockerCodes as readonly unknown[];
    requireEnumOrder(blockers, reviewCurrencyBlockerOrder, `${path}.blockerCodes`);
    if (record.certifying === true) {
      if (!supportsCertifyingAnswerBearingLeg(record.providerAssurance as ProviderIdentityAssurance)) {
        throw new TypeError(`${path}.certifying requires a certifying provider assurance`);
      }
      const basis = record.currentCertificationBasis as Readonly<Record<string, unknown>> | null;
      const current = ["target", "source", "chair", "profile"].every((field) => record[field] === "current");
      if (!current || basis === null || basis.kind === "post-cut" || blockers.length !== 0) {
        throw new TypeError(`${path}.certifying requires wholly current currency, current basis and no blockers`);
      }
    }
    return record;
  },
  {
    ...reviewEvidenceCurrencyBaseCodec.example,
    currentCertificationBasis: null,
    certifying: false,
    blockerCodes: [],
  },
);
export const REVIEW_EVIDENCE_ANNOTATION_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  evidenceId: id256,
  annotationRevision: positive,
  priorAnnotationRevision: nullable(positive),
  commandId: id256,
  chairBindingGeneration: positive,
  disposition: enumeration(["substantiated", "unsubstantiated", "duplicate", "needs-more-evidence"]),
  note: boundedString({ minBytes: 0, maxBytes: 512, example: "reviewed" }),
  noteDigest: sha256,
  annotationDigest: sha256,
});
const reviewEvidenceReadBaseCodec = objectCodec({
  schemaVersion: literal(1),
  record: REVIEW_EVIDENCE_RECORD_V1_CODEC,
  currency: REVIEW_EVIDENCE_CURRENCY_V1_CODEC,
  annotation: nullable(REVIEW_EVIDENCE_ANNOTATION_V1_CODEC),
});
export const REVIEW_EVIDENCE_READ_V1_CODEC = parserBacked(
  defineCodec(
    { ...reviewEvidenceReadBaseCodec.schema, "x-reviewEvidenceReadCorrelated": true },
    reviewEvidenceReadBaseCodec.example,
    (input, path) => reviewEvidenceReadBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const evidence = record.record as Readonly<Record<string, unknown>>;
    const currency = record.currency as Readonly<Record<string, unknown>>;
    if (currency.certifying === true) {
      const findingWindow = evidence.findingWindow as Readonly<Record<string, unknown>>;
      const coverage = evidence.coverageSummary as Readonly<Record<string, unknown>>;
      const terminalBasis = evidence.certificationBasisAtTerminal as Readonly<Record<string, unknown>>;
      const currentBasis = currency.currentCertificationBasis as Readonly<Record<string, unknown>>;
      const expectedRelation = evidence.slot === "native" ? "same-family-exempt" : "distinct-family-proved";
      if (
        currency.providerAssurance !== evidence.providerAssurance ||
        !supportsCertifyingAnswerBearingLeg(currency.providerAssurance as ProviderIdentityAssurance) ||
        evidence.terminalKind !== "safe-answer" || evidence.answerSafety !== "safe" ||
        !["CLEAN", "FINDINGS"].includes(String(evidence.verdict)) || evidence.reviewResultDigest === null ||
        evidence.routeObservationDigest === null || evidence.actualRouteIdentityDigest === null ||
        evidence.reviewerFamilyRelation !== expectedRelation || findingWindow.mode !== "normal" ||
        coverage.mandatoryComplete !== true || terminalBasis.kind === "post-cut" ||
        currentBasis.terminalSequence !== evidence.terminalSequence
      ) {
        throw new TypeError(`${path}.currency.certifying requires a normal, sufficient, route-proved, family-valid safe evidence record at the same terminal sequence`);
      }
    }
    return record;
  },
  reviewEvidenceReadBaseCodec.example,
);

export const REVIEW_EVIDENCE_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  evidenceId: id256,
});
export const REVIEW_EVIDENCE_LIST_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  targetGeneration: nullable(positive),
  slot: nullable(reviewSlot),
  pageSize: integer({ minimum: 1, maximum: 100 }),
  cursor,
});
export const REVIEW_EVIDENCE_LIST_RESULT_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  entries: arrayOf(REVIEW_EVIDENCE_READ_V1_CODEC, { maximum: 100 }),
  nextCursor: cursor,
});
export const REVIEW_COMPLETION_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
});
export const REVIEW_EVIDENCE_ANNOTATION_APPEND_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  commandId: id256,
  projectSessionId: id256,
  coordinationRunId: id256,
  evidenceId: id256,
  expectedResultDigest: sha256,
  expectedHeadGeneration: nonnegative,
  expectedAnnotationRevision: nonnegative,
  disposition: enumeration(["substantiated", "unsubstantiated", "duplicate", "needs-more-evidence"]),
  note: boundedString({ minBytes: 0, maxBytes: 512, example: "reviewed" }),
});
export const REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_REQUEST_V1_CODEC = REVIEW_EVIDENCE_READ_REQUEST_V1_CODEC;
export const REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_RESULT_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  evidenceId: id256,
  annotation: nullable(REVIEW_EVIDENCE_ANNOTATION_V1_CODEC),
});
export const REVIEW_FINDING_PAGE_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  findingSetDigest: sha256,
  pageDigest: sha256,
});
export const REVIEW_FINDING_PAGE_READ_RESULT_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  findingSetDigest: sha256,
  pageDigest: sha256,
  members: arrayOf(SAFE_FINDING_V1_CODEC, { minimum: 1, maximum: 4096 }),
  nextPageDigest: nullableDigest,
});
export const REVIEW_READ_ERROR_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  code: enumeration(["NOT_FOUND", "AUTHORITY_DENIED", "SCOPE_MISMATCH", "STALE_CURSOR", "STALE_REVISION", "INTEGRITY_FAILURE"]),
  currentRevision: nullable(nonnegative),
  evidenceDigest: nullableDigest,
});

const targetChairCodec = objectCodec({
  agentId: id256,
  bindingGeneration: positive,
  principalGeneration: positive,
  chairLeaseGeneration: positive,
  providerSessionGeneration: positive,
  bridgeGeneration: positive,
  adapterId: id256,
  adapterContractDigest: sha256,
  modelFamily: id256,
  model: id256,
  routeReceiptDigest: nullableDigest,
});
const certifyingSlotUnavailableCodec = objectCodec({
  projectSessionId: id256,
  profileId: id256,
  profileSchemaDigest: sha256,
  targetChairFamily: id256,
  slot: reviewSlot,
  adapterId: id256,
  adapterContractDigest: sha256,
  providerFamily: id256,
  model: id256,
  sourceMode: id256,
  runtimeIdentityDigest: sha256,
  platformIdentityDigest: sha256,
  availabilityRevision: positive,
  reason: enumeration(["adapter-inactive", "contract-mismatch", "confinement-unproved", "portal-unavailable", "provider-runtime-unavailable"]),
});
const reviewSlotBaseCodec = objectCodec({
  slot: reviewSlot,
  headGeneration: nonnegative,
  attemptGeneration: nonnegative,
  actionRef: nullable(PROVIDER_ACTION_REF_V1_CODEC),
  evidenceId: nullableId,
  terminalKind: nullable(enumeration(["safe-answer", "unusable-answer", "provider-terminal-failure", "terminal-no-effect", "integrity-terminal", "retired-unknown"])),
  verdict: nullable(enumeration(["CLEAN", "FINDINGS", "UNUSABLE"])),
  resultDigest: nullableDigest,
  providerFailureCode: nullable(providerFailureCode),
  providerFailureDigest: nullableDigest,
  routeReceiptDigest: nullableDigest,
  adapterId: id256,
  endpointProvider: id256,
  providerFamily: id256,
  model: id256,
  routeObservationDigest: nullableDigest,
  actualRouteIdentityDigest: nullableDigest,
  readCoverageDigest: nullableDigest,
  reviewerFamilyRelation,
  currentCertificationBasis: nullable(REVIEW_CERTIFICATION_BASIS_V1_CODEC),
  certifying: boolean,
  openFindingSet: FINDING_SET_REF_V1_CODEC,
  blockers: arrayOf(enumeration(SLOT_REVIEW_BLOCKERS), { maximum: SLOT_REVIEW_BLOCKERS.length, unique: true }),
}, {
  providerAssurance: PROVIDER_IDENTITY_ASSURANCE_V1_CODEC,
});
export const REVIEW_SLOT_V1_CODEC = parserBacked(
  defineCodec(
    { ...reviewSlotBaseCodec.schema, "x-reviewSlotCorrelated": true },
    reviewSlotBaseCodec.example,
    (input, path) => reviewSlotBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const action = record.actionRef as Readonly<Record<string, unknown>> | null;
    if (action !== null && record.adapterId !== action.adapterId) {
      throw new TypeError(`${path}.adapterId must equal actionRef.adapterId`);
    }
    const terminalKind = record.terminalKind;
    const verdict = record.verdict;
    if (terminalKind === "safe-answer" && !["CLEAN", "FINDINGS"].includes(String(verdict))) {
      throw new TypeError(`${path}.safe-answer terminalKind requires CLEAN or FINDINGS verdict`);
    }
    if (terminalKind === "unusable-answer" && verdict !== "UNUSABLE") {
      throw new TypeError(`${path}.unusable-answer terminalKind requires UNUSABLE verdict`);
    }
    if (terminalKind !== "safe-answer" && terminalKind !== "unusable-answer" && verdict !== null) {
      throw new TypeError(`${path}.${String(terminalKind)} terminalKind requires null verdict`);
    }
    const providerFailure = terminalKind === "provider-terminal-failure";
    if (providerFailure !== (record.providerFailureCode !== null) || providerFailure !== (record.providerFailureDigest !== null)) {
      throw new TypeError(`${path}.provider failure fields must exist exactly for provider-terminal-failure`);
    }
    if (record.certifying === true) {
      if (!supportsCertifyingAnswerBearingLeg(record.providerAssurance as ProviderIdentityAssurance)) {
        throw new TypeError(`${path}.certifying requires a certifying provider assurance`);
      }
      const basis = record.currentCertificationBasis as Readonly<Record<string, unknown>> | null;
      if (terminalKind !== "safe-answer" || basis === null || basis.kind === "post-cut") {
        throw new TypeError(`${path}.certifying requires a safe answer with a current certifying basis`);
      }
      const expectedRelation = record.slot === "native" ? "same-family-exempt" : "distinct-family-proved";
      const blockers = record.blockers as readonly unknown[];
      const open = record.openFindingSet as Readonly<Record<string, unknown>>;
      const requiredEvidenceFields = [
        "actionRef", "evidenceId", "resultDigest", "routeReceiptDigest", "routeObservationDigest",
        "actualRouteIdentityDigest", "readCoverageDigest",
      ] as const;
      if (
        Number(record.headGeneration) <= 0 || Number(record.attemptGeneration) <= 0 ||
        requiredEvidenceFields.some((field) => record[field] === null) ||
        record.reviewerFamilyRelation !== expectedRelation
      ) {
        throw new TypeError(`${path}.certifying requires positive head/attempt, complete route-proved evidence and exact reviewer family relation`);
      }
      if (record.verdict === "CLEAN") {
        if (blockers.length !== 0 || open.findingCount !== 0) {
          throw new TypeError(`${path}.certifying CLEAN requires no blockers or open findings`);
        }
      } else if (
        record.verdict !== "FINDINGS" || blockers.length !== 1 || blockers[0] !== "open-findings" ||
        Number(open.findingCount) <= 0
      ) {
        throw new TypeError(`${path}.certifying FINDINGS requires only open-findings and a nonempty open set`);
      }
    }
    return record;
  },
  {
    ...reviewSlotBaseCodec.example,
    headGeneration: 0,
    attemptGeneration: 0,
    actionRef: null,
    evidenceId: null,
    terminalKind: null,
    verdict: null,
    resultDigest: null,
    providerFailureCode: null,
    providerFailureDigest: null,
    routeReceiptDigest: null,
    routeObservationDigest: null,
    actualRouteIdentityDigest: null,
    readCoverageDigest: null,
    currentCertificationBasis: null,
    certifying: false,
  },
);

const completionBaseCodec = objectCodec({
  schemaVersion: literal(1),
  blockers: arrayOf(enumeration(TOP_REVIEW_BLOCKERS), { maximum: TOP_REVIEW_BLOCKERS.length, unique: true }),
  targetGeneration: nullable(positive),
  targetChair: nullable(targetChairCodec),
  reviewedArtifactRef: nullableId,
  publicationLineageDigest: nullableDigest,
  bundleDigest: nullableDigest,
  manifestRootDigest: nullableDigest,
  coverageDigest: nullableDigest,
  riskReadMapDigest: nullableDigest,
  mandatoryReadSetDigest: nullableDigest,
  profileDigest: nullableDigest,
  unavailableSlots: arrayOf(certifyingSlotUnavailableCodec, { maximum: 4, unique: true }),
  slots: arrayOf(REVIEW_SLOT_V1_CODEC, { maximum: 4 }),
  finalReviewComplete: boolean,
}, {}, {
  example: {
    schemaVersion: 1,
    blockers: ["missing-target"],
    targetGeneration: null,
    targetChair: null,
    reviewedArtifactRef: null,
    publicationLineageDigest: null,
    bundleDigest: null,
    manifestRootDigest: null,
    coverageDigest: null,
    riskReadMapDigest: null,
    mandatoryReadSetDigest: null,
    profileDigest: null,
    unavailableSlots: [],
    slots: [],
    finalReviewComplete: false,
  },
});
const nonnullSchema = { not: { type: "null" } } as const;
const completeSlotCommonSchema = {
  type: "object",
  properties: {
    headGeneration: { minimum: 1 },
    attemptGeneration: { minimum: 1 },
    actionRef: nonnullSchema,
    evidenceId: nonnullSchema,
    terminalKind: { const: "safe-answer" },
    verdict: { const: "CLEAN" },
    resultDigest: nonnullSchema,
    providerFailureCode: { type: "null" },
    providerFailureDigest: { type: "null" },
    routeReceiptDigest: nonnullSchema,
    routeObservationDigest: nonnullSchema,
    actualRouteIdentityDigest: nonnullSchema,
    readCoverageDigest: nonnullSchema,
    providerAssurance: { enum: ["full-vendor-identity", "lockfile-install-attestation"] },
    currentCertificationBasis: nonnullSchema,
    certifying: { const: true },
    openFindingSet: {
      type: "object",
      properties: { findingCount: { const: 0 }, pageDigests: { maxItems: 0 } },
      required: ["findingCount", "pageDigests"],
    },
    blockers: { maxItems: 0 },
  },
} as const;
const completionPredicateSchema = {
  type: "object",
  properties: {
    blockers: { maxItems: 0 },
    targetGeneration: nonnullSchema,
    targetChair: nonnullSchema,
    reviewedArtifactRef: nonnullSchema,
    publicationLineageDigest: nonnullSchema,
    bundleDigest: nonnullSchema,
    manifestRootDigest: nonnullSchema,
    coverageDigest: nonnullSchema,
    riskReadMapDigest: nonnullSchema,
    mandatoryReadSetDigest: nonnullSchema,
    profileDigest: nonnullSchema,
    unavailableSlots: { maxItems: 0 },
    slots: {
      minItems: 4,
      maxItems: 4,
      prefixItems: REVIEW_SLOTS.map((slot, index) => ({
        allOf: [
          completeSlotCommonSchema,
          {
            type: "object",
            properties: {
              slot: { const: slot },
              reviewerFamilyRelation: { const: index === 0 ? "same-family-exempt" : "distinct-family-proved" },
            },
          },
        ],
      })),
      items: false,
    },
  },
  required: [
    "blockers", "targetGeneration", "targetChair", "reviewedArtifactRef",
    "publicationLineageDigest", "bundleDigest", "manifestRootDigest", "coverageDigest",
    "riskReadMapDigest", "mandatoryReadSetDigest", "profileDigest", "unavailableSlots", "slots",
  ],
} as const;

function isCompleteReview(record: Readonly<Record<string, unknown>>): boolean {
  const blockers = record.blockers as readonly unknown[];
  const unavailable = record.unavailableSlots as readonly unknown[];
  const slots = record.slots as readonly Readonly<Record<string, unknown>>[];
  const targetFields = [
    "targetGeneration", "targetChair", "reviewedArtifactRef", "publicationLineageDigest",
    "bundleDigest", "manifestRootDigest", "coverageDigest", "riskReadMapDigest",
    "mandatoryReadSetDigest", "profileDigest",
  ];
  return blockers.length === 0 && unavailable.length === 0 && targetFields.every((field) => record[field] !== null) &&
    slots.length === REVIEW_SLOTS.length && slots.every((slot, index) => {
      const open = slot.openFindingSet as Readonly<Record<string, unknown>>;
      return slot.slot === REVIEW_SLOTS[index] && slot.terminalKind === "safe-answer" && slot.verdict === "CLEAN" &&
        Number(slot.headGeneration) > 0 && Number(slot.attemptGeneration) > 0 &&
        slot.actionRef !== null && slot.evidenceId !== null && slot.resultDigest !== null &&
        slot.routeReceiptDigest !== null && slot.routeObservationDigest !== null && slot.actualRouteIdentityDigest !== null &&
        slot.readCoverageDigest !== null && slot.currentCertificationBasis !== null && slot.certifying === true &&
        supportsCertifyingAnswerBearingLeg(slot.providerAssurance as ProviderIdentityAssurance) &&
        slot.reviewerFamilyRelation === (index === 0 ? "same-family-exempt" : "distinct-family-proved") &&
        open.findingCount === 0 && (open.pageDigests as readonly unknown[]).length === 0 &&
        (slot.blockers as readonly unknown[]).length === 0;
    });
}

export const REVIEW_COMPLETION_V1_CODEC = defineCodec(
  {
    ...completionBaseCodec.schema,
    "x-reviewCompletionCorrelated": true,
    allOf: [
      {
        if: { type: "object", properties: { finalReviewComplete: { const: true } }, required: ["finalReviewComplete"] },
        then: completionPredicateSchema,
      },
      {
        if: completionPredicateSchema,
        then: { type: "object", properties: { finalReviewComplete: { const: true } }, required: ["finalReviewComplete"] },
      },
    ],
  },
  completionBaseCodec.example,
  (input, path) => {
    const value = completionBaseCodec.parse(input, path);
    const record = value as Readonly<Record<string, unknown>>;
    const blockers = record.blockers as readonly unknown[];
    const unavailable = record.unavailableSlots as readonly Readonly<Record<string, unknown>>[];
    const slots = record.slots as readonly Readonly<Record<string, unknown>>[];
    requireEnumOrder(blockers, TOP_REVIEW_BLOCKERS, `${path}.blockers`);
    requireEnumOrder(unavailable.map((entry) => entry.slot), REVIEW_SLOTS, `${path}.unavailableSlots`);
    slots.forEach((slot, index) => {
      requireEnumOrder(slot.blockers as readonly unknown[], SLOT_REVIEW_BLOCKERS, `${path}.slots[${String(index)}].blockers`);
    });
    if (slots.length !== 0 && slots.length !== REVIEW_SLOTS.length) {
      throw new TypeError(`${path}.slots must be empty or exactly four`);
    }
    if (slots.length === REVIEW_SLOTS.length) {
      slots.forEach((slot, index) => {
        if (slot.slot !== REVIEW_SLOTS[index]) throw new TypeError(`${path}.slots must use exact profile order`);
      });
    }
    const targetFields = [
      "targetGeneration", "targetChair", "reviewedArtifactRef", "publicationLineageDigest", "bundleDigest",
      "manifestRootDigest", "coverageDigest", "riskReadMapDigest", "mandatoryReadSetDigest", "profileDigest",
    ] as const;
    const immutableTargetFields = [
      "targetGeneration", "reviewedArtifactRef", "publicationLineageDigest", "bundleDigest", "manifestRootDigest",
      "coverageDigest", "riskReadMapDigest", "mandatoryReadSetDigest",
    ] as const;
    if (blockers.length > 1) {
      throw new TypeError(`${path}.blockers must select exactly one deterministic top-level branch`);
    }
    if (blockers.includes("missing-target")) {
      if (blockers.length !== 1 || targetFields.some((field) => record[field] !== null) || unavailable.length !== 0 || slots.length !== 0) {
        throw new TypeError(`${path}.missing-target branch requires only missing-target and null target/profile/slot state`);
      }
    }
    if (unavailable.length !== 0) {
      if (blockers.length !== 1 || blockers[0] !== "certifying-review-capability-unavailable" || slots.length !== 0) {
        throw new TypeError(`${path}.unavailableSlots branch requires only certifying-review-capability-unavailable and empty slots`);
      }
    } else if (blockers.includes("certifying-review-capability-unavailable")) {
      throw new TypeError(`${path}.certifying-review-capability-unavailable requires unavailableSlots`);
    }
    if (record.targetGeneration === null) {
      const targetNullBranches = [
        "certifying-review-capability-unavailable", "finding-capacity-exhausted", "missing-target", "integrity-failure",
      ];
      if (blockers.length !== 1 || !targetNullBranches.includes(String(blockers[0])) || slots.length !== 0 ||
        targetFields.some((field) => record[field] !== null)) {
        throw new TypeError(`${path}.target-null state must use one exact target-null branch with empty slots`);
      }
    }
    if (slots.length === REVIEW_SLOTS.length && targetFields.some((field) => record[field] === null)) {
      throw new TypeError(`${path}.four-slot branch requires complete target, chair, artifact, bundle and profile fields`);
    }
    if (slots.length === 0 && blockers.length === 0) {
      throw new TypeError(`${path}.empty-slot branch requires one top-level blocker`);
    }
    if (blockers[0] === "stale-target" && (slots.length !== REVIEW_SLOTS.length || targetFields.some((field) => record[field] === null))) {
      throw new TypeError(`${path}.stale-target branch requires a complete target/profile and four slots`);
    }
    if (blockers[0] === "profile-unavailable" && (
      immutableTargetFields.some((field) => record[field] === null) || record.targetChair === null ||
      record.profileDigest !== null || slots.length !== 0
    )) {
      throw new TypeError(`${path}.profile-unavailable branch requires immutable target fields, targetChair, null profile and empty slots`);
    }
    if (blockers[0] === "integrity-failure" && (
      record.targetChair !== null || record.profileDigest !== null || slots.length !== 0 ||
      (record.targetGeneration !== null && immutableTargetFields.some((field) => record[field] === null))
    )) {
      throw new TypeError(`${path}.integrity-failure branch requires exact immutable target fields when target-present, plus null chair/profile and empty slots`);
    }
    if (blockers[0] === "finding-capacity-exhausted" && slots.length !== 0) {
      throw new TypeError(`${path}.finding-capacity-exhausted branch requires empty slots`);
    }
    if (slots.length === REVIEW_SLOTS.length && blockers.length !== 0 && blockers[0] !== "stale-target") {
      throw new TypeError(`${path}.four-slot blocked branch permits only stale-target at top level`);
    }
    if (record.finalReviewComplete !== isCompleteReview(record)) {
      throw new TypeError(`${path}.finalReviewComplete must exactly match the complete target/profile/slot predicate`);
    }
    return record;
  },
);

export const PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
});
const providerRouteIntegrityRecoveryProjectionBaseCodec = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  taskId: id256,
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  targetGeneration: positive,
  slot: reviewSlot,
  attemptGeneration: positive,
  recoveryGeneration: positive,
  state: enumeration(["detected", "inspecting", "terminal-proved-no-effect", "terminal-proved-usage", "awaiting-human-retire", "terminal-retired-unknown"]),
  reason: enumeration(["intact-effect-ambiguity", "route-row-missing", "route-row-conflict", "route-receipt-mismatch", "target-binding-invalid", "bundle-binding-invalid", "prompt-binding-invalid", "profile-binding-invalid", "lineage-binding-invalid"]),
  reservationDigest: sha256,
  routeState: enumeration(["present", "missing", "integrity-failed"]),
  routeReceiptDigest: nullableDigest,
  lookupState: enumeration(["not-attempted", "in-flight", "completed"]),
  lookupEvidenceDigest: nullableDigest,
  disposition: nullable(enumeration(["proved-no-effect-release", "exact-usage-settled", "conservative-full-ceiling-settled", "full-ceiling-retired"])),
  settlementDigest: nullableDigest,
  recoveryEvidenceDigest: sha256,
  retirementEligible: boolean,
});
export const PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC = parserBacked(
  defineCodec(
    { ...providerRouteIntegrityRecoveryProjectionBaseCodec.schema, "x-routeRecoveryCorrelated": true },
    providerRouteIntegrityRecoveryProjectionBaseCodec.example,
    (input, path) => providerRouteIntegrityRecoveryProjectionBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    if ((record.routeState === "present") !== (record.routeReceiptDigest !== null)) {
      throw new TypeError(`${path}.routeReceiptDigest must be nonnull exactly for present routeState`);
    }
    if ((record.lookupState === "completed") !== (record.lookupEvidenceDigest !== null)) {
      throw new TypeError(`${path}.lookupEvidenceDigest must be nonnull exactly for completed lookupState`);
    }
    const terminal = String(record.state).startsWith("terminal-");
    if (terminal !== (record.settlementDigest !== null) || terminal !== (record.disposition !== null)) {
      throw new TypeError(`${path}.terminal recovery requires disposition and settlementDigest`);
    }
    const allowedDispositions: Readonly<Record<string, readonly unknown[]>> = {
      "terminal-proved-no-effect": ["proved-no-effect-release"],
      "terminal-proved-usage": ["exact-usage-settled", "conservative-full-ceiling-settled"],
      "terminal-retired-unknown": ["full-ceiling-retired"],
    };
    if (terminal && !allowedDispositions[String(record.state)]?.includes(record.disposition)) {
      throw new TypeError(`${path}.disposition must exactly match terminal recovery state`);
    }
    if (record.retirementEligible !== (record.state === "awaiting-human-retire")) {
      throw new TypeError(`${path}.retirementEligible must reflect awaiting-human-retire state`);
    }
    return record;
  },
  {
    ...providerRouteIntegrityRecoveryProjectionBaseCodec.example,
    routeState: "missing",
    routeReceiptDigest: null,
    lookupState: "not-attempted",
    lookupEvidenceDigest: null,
    disposition: null,
    settlementDigest: null,
    state: "detected",
    retirementEligible: false,
  },
);
export const PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_ERROR_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  code: enumeration(["NOT_FOUND", "AUTHORITY_DENIED", "SCOPE_MISMATCH", "INTEGRITY_FAILURE"]),
  evidenceDigest: nullableDigest,
});

export type TerminalResultIdentityV1 = CodecOutput<typeof TERMINAL_RESULT_IDENTITY_V1_CODEC>;
export type ReviewEvidenceRecordV1 = CodecOutput<typeof REVIEW_EVIDENCE_RECORD_V1_CODEC>;
export type ReviewCompletionV1 = CodecOutput<typeof REVIEW_COMPLETION_V1_CODEC>;
export type ReviewTargetPrepareV1 = CodecOutput<typeof REVIEW_TARGET_PREPARE_V1_CODEC>;
export type ReviewTargetPreparationAcceptedV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_ACCEPTED_V1_CODEC>;
export type ReviewTargetPreparationReadRequestV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_READ_REQUEST_V1_CODEC>;
export type ReviewTargetPreparationReadV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_READ_V1_CODEC>;
export type ReviewTargetRebindV1 = CodecOutput<typeof REVIEW_TARGET_REBIND_V1_CODEC>;
export type ReviewTargetRebindReceiptV1 = CodecOutput<typeof REVIEW_TARGET_REBIND_RECEIPT_V1_CODEC>;
export type ReviewEvidenceReadRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_READ_REQUEST_V1_CODEC>;
export type ReviewEvidenceReadV1 = CodecOutput<typeof REVIEW_EVIDENCE_READ_V1_CODEC>;
export type ReviewEvidenceListRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_LIST_REQUEST_V1_CODEC>;
export type ReviewEvidenceListResultV1 = CodecOutput<typeof REVIEW_EVIDENCE_LIST_RESULT_V1_CODEC>;
export type ReviewCompletionReadRequestV1 = CodecOutput<typeof REVIEW_COMPLETION_READ_REQUEST_V1_CODEC>;
export type ReviewEvidenceAnnotationAppendRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_APPEND_REQUEST_V1_CODEC>;
export type ReviewEvidenceAnnotationV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_V1_CODEC>;
export type ReviewEvidenceAnnotationCurrentReadRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_REQUEST_V1_CODEC>;
export type ReviewEvidenceAnnotationCurrentReadResultV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_RESULT_V1_CODEC>;
export type ReviewFindingPageReadRequestV1 = CodecOutput<typeof REVIEW_FINDING_PAGE_READ_REQUEST_V1_CODEC>;
export type ReviewFindingPageReadResultV1 = CodecOutput<typeof REVIEW_FINDING_PAGE_READ_RESULT_V1_CODEC>;
export type ProviderRouteIntegrityRecoveryReadRequestV1 = CodecOutput<typeof PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_REQUEST_V1_CODEC>;
export type ProviderRouteIntegrityRecoveryProjectionV1 = CodecOutput<typeof PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC>;
export type ReviewTargetPreparationReadErrorV1 = CodecOutput<typeof REVIEW_TARGET_PREPARATION_READ_ERROR_V1_CODEC>;
export type ReviewReadErrorV1 = CodecOutput<typeof REVIEW_READ_ERROR_V1_CODEC>;
export type ProviderRouteIntegrityRecoveryReadErrorV1 = CodecOutput<typeof PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_ERROR_V1_CODEC>;
