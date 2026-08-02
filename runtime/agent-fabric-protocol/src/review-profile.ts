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
} from "./codec.js";
import { RESOLVED_EFFORT_V1_CODEC } from "./route-lineage.js";
import { REVIEW_SLOTS } from "./provider-review-core.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
export const RESOLVED_REVIEW_PROFILE_SLOT_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  slot: enumeration(REVIEW_SLOTS),
  adapterClass: enumeration(["primary-native", "equal-primary", "cursor", "agy"]),
  adapterId: id256,
  adapterContractDigest: sha256,
  providerFamily: id256,
  model: id256,
  requiredActualEndpointProvider: id256,
  requiredActualProviderFamily: id256,
  requiredActualModel: id256,
  requestedEffort: nullable(id256),
  resolvedEffort: RESOLVED_EFFORT_V1_CODEC,
  sourceMode: enumeration(["direct-portal", "portal-helper"]),
  runtimeIdentityDigest: sha256,
  platformIdentityDigest: sha256,
  providerTurnCeiling: positive,
  internalStepCeiling: nonnegative,
  mandatoryReadOps: nonnegative,
  mandatoryReadBytes: nonnegative,
  explorationReadOps: nonnegative,
  explorationReadBytes: nonnegative,
  routeAliases: arrayOf(id256, { minimum: 1, maximum: 64, unique: true }),
  riskReadMapDigest: sha256,
  reviewerFamilyRelation: enumeration(["same-family-exempt", "distinct-family-proved"]),
});

const resolvedReviewProfileBaseCodec = objectCodec({
  schemaVersion: literal(1),
  profileId: literal("certifying-review-four-slot-v1"),
  profileSchemaDigest: sha256,
  targetChairFamily: enumeration(["openai", "anthropic"]),
  slots: arrayOf(RESOLVED_REVIEW_PROFILE_SLOT_V1_CODEC, { minimum: 4, maximum: 4 }),
  resolvedProfileDigest: sha256,
});
const slotExamples = REVIEW_SLOTS.map((slot, index) => ({
  ...RESOLVED_REVIEW_PROFILE_SLOT_V1_CODEC.example,
  slot,
  adapterClass: (["primary-native", "equal-primary", "cursor", "agy"] as const)[index],
  reviewerFamilyRelation: index === 0 ? "same-family-exempt" : "distinct-family-proved",
}));
const profileExample = {
  ...resolvedReviewProfileBaseCodec.example,
  slots: slotExamples,
};
export const RESOLVED_REVIEW_PROFILE_V1_CODEC = parserBacked(
  defineCodec(
    { ...resolvedReviewProfileBaseCodec.schema, "x-fourSlotProfileMatrix": true },
    resolvedReviewProfileBaseCodec.example,
    (input, path) => resolvedReviewProfileBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const slots = record.slots as readonly Readonly<Record<string, unknown>>[];
    if (slots.length !== 4) throw new TypeError(`${path}.slots must contain exactly four entries`);
    slots.forEach((slot, index) => {
      if (slot.slot !== REVIEW_SLOTS[index]) throw new TypeError(`${path}.slots must use canonical four-slot order`);
      if (slot.requiredActualProviderFamily !== slot.providerFamily || slot.requiredActualModel !== slot.model) {
        throw new TypeError(`${path}.slots[${String(index)}] actual family/model must equal admitted family/model`);
      }
      const effort = slot.resolvedEffort as Readonly<Record<string, unknown>>;
      if (effort.kind === "inapplicable" && slot.requestedEffort !== null) {
        throw new TypeError(`${path}.slots[${String(index)}].requestedEffort must be null for inapplicable effort`);
      }
      if (
        (slot.slot === "cursor-grok" || slot.slot === "agy-gemini") &&
        (slot.requestedEffort !== null || effort.kind !== "inapplicable")
      ) {
        throw new TypeError(`${path}.slots[${String(index)}] helper effort must be inapplicable with a null request`);
      }
    });
    const target = record.targetChairFamily as "openai" | "anthropic";
    const expected = [
      target === "openai"
        ? { slot: "native", adapterClass: "primary-native", adapterId: "codex-app-server", providerFamily: "openai", sourceMode: "direct-portal", reviewerFamilyRelation: "same-family-exempt" }
        : { slot: "native", adapterClass: "primary-native", adapterId: "claude-agent-sdk", providerFamily: "anthropic", sourceMode: "direct-portal", reviewerFamilyRelation: "same-family-exempt" },
      target === "openai"
        ? { slot: "other-primary", adapterClass: "equal-primary", adapterId: "claude-agent-sdk", providerFamily: "anthropic", sourceMode: "direct-portal", reviewerFamilyRelation: "distinct-family-proved" }
        : { slot: "other-primary", adapterClass: "equal-primary", adapterId: "codex-app-server", providerFamily: "openai", sourceMode: "direct-portal", reviewerFamilyRelation: "distinct-family-proved" },
      { slot: "cursor-grok", adapterClass: "cursor", adapterId: "cursor-agent", providerFamily: "xai", model: "cursor-grok-4.5-high", sourceMode: "portal-helper", reviewerFamilyRelation: "distinct-family-proved" },
      { slot: "agy-gemini", adapterClass: "agy", adapterId: "agy", providerFamily: "google", model: "Gemini 3.1 Pro (High)", sourceMode: "portal-helper", reviewerFamilyRelation: "distinct-family-proved" },
    ] as const;
    slots.forEach((slot, index) => {
      const rules = expected[index];
      if (rules === undefined || Object.entries(rules).some(([field, expectedValue]) => slot[field] !== expectedValue)) {
        throw new TypeError(`${path}.slots[${String(index)}] must match the exact ${String(slot.slot)} matrix`);
      }
    });
    return record;
  },
  profileExample,
);
