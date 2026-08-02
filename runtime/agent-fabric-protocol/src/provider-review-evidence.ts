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
import {
  LIFECYCLE_CUSTODY_REF_V1_CODEC,
  SLOT_REVIEW_BLOCKERS,
  TOP_REVIEW_BLOCKERS,
  REVIEW_SLOTS,
} from "./provider-review-core.js";
import { LOCAL_PROVIDER_ROUTE_V1_CODEC } from "./route-lineage.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const nullableId = nullable(id256);
const nullableDigest = nullable(sha256);
const cursor = nullable(boundedString({ maxBytes: 256, example: "cursor_01" }));

const reviewSlot = enumeration(REVIEW_SLOTS);
const providerFailureCode = enumeration([
  "max-turns-exhausted",
  "provider-rejected",
  "terminal-no-answer",
  "adapter-terminal-failure",
]);
const reviewCurrencyBlocker = enumeration([
  ...TOP_REVIEW_BLOCKERS,
  ...SLOT_REVIEW_BLOCKERS,
  "superseded",
]);

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


export type ReviewEvidenceRecordV1 = CodecOutput<typeof REVIEW_EVIDENCE_RECORD_V1_CODEC>;
export type ReviewEvidenceReadRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_READ_REQUEST_V1_CODEC>;
export type ReviewEvidenceReadV1 = CodecOutput<typeof REVIEW_EVIDENCE_READ_V1_CODEC>;
export type ReviewEvidenceListRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_LIST_REQUEST_V1_CODEC>;
export type ReviewEvidenceListResultV1 = CodecOutput<typeof REVIEW_EVIDENCE_LIST_RESULT_V1_CODEC>;
export type ReviewEvidenceAnnotationAppendRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_APPEND_REQUEST_V1_CODEC>;
export type ReviewEvidenceAnnotationV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_V1_CODEC>;
export type ReviewEvidenceAnnotationCurrentReadRequestV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_REQUEST_V1_CODEC>;
export type ReviewEvidenceAnnotationCurrentReadResultV1 = CodecOutput<typeof REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_RESULT_V1_CODEC>;
export type ReviewFindingPageReadRequestV1 = CodecOutput<typeof REVIEW_FINDING_PAGE_READ_REQUEST_V1_CODEC>;
export type ReviewFindingPageReadResultV1 = CodecOutput<typeof REVIEW_FINDING_PAGE_READ_RESULT_V1_CODEC>;
export type ReviewReadErrorV1 = CodecOutput<typeof REVIEW_READ_ERROR_V1_CODEC>;
