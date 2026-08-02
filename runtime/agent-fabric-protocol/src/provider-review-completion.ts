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
  type CodecOutput,
} from "./codec.js";
import { PROVIDER_ACTION_REF_V1_CODEC } from "./launch.js";
import {
  PROVIDER_IDENTITY_ASSURANCE_V1_CODEC,
  supportsCertifyingAnswerBearingLeg,
  type ProviderIdentityAssurance,
} from "./provider-assurance.js";
import { REVIEW_SLOTS, SLOT_REVIEW_BLOCKERS, TOP_REVIEW_BLOCKERS } from "./provider-review-core.js";
import { FINDING_SET_REF_V1_CODEC, REVIEW_CERTIFICATION_BASIS_V1_CODEC } from "./provider-review-evidence.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const nullableId = nullable(id256);
const nullableDigest = nullable(sha256);
const reviewSlot = enumeration(REVIEW_SLOTS);
const providerFailureCode = enumeration([
  "max-turns-exhausted",
  "provider-rejected",
  "terminal-no-answer",
  "adapter-terminal-failure",
]);
const reviewerFamilyRelation = enumeration([
  "same-family-exempt",
  "distinct-family-proved",
  "same-family-forbidden",
  "family-unproved",
]);
function requireEnumOrder(values: readonly unknown[], order: readonly string[], path: string): void {
  let previous = -1;
  for (const value of values) {
    const rank = order.indexOf(String(value));
    if (rank <= previous) throw new TypeError(`${path} must use canonical deterministic order`);
    previous = rank;
  }
}

export const REVIEW_COMPLETION_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
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

export type ReviewCompletionReadRequestV1 = CodecOutput<typeof REVIEW_COMPLETION_READ_REQUEST_V1_CODEC>;
export type ReviewCompletionV1 = CodecOutput<typeof REVIEW_COMPLETION_V1_CODEC>;
