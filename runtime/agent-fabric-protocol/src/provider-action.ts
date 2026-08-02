import {
  arrayOf,
  boolean,
  boundedString,
  enumeration,
  integer,
  jsonValue,
  literal,
  nullable,
  objectCodec,
  sha256,
  unionOf,
  type CodecOutput,
} from "./codec.js";
import { PROVIDER_ACTION_REF_V1_CODEC } from "./launch.js";
import { PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC } from "./provider-review-evidence.js";

const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const routeRequestCodec = objectCodec({
  schemaVersion: literal(1),
  adapterAlias: id256,
  modelAlias: id256,
  explicitModel: nullable(id256),
  role: id256,
  leadFamily: id256,
  requireDistinct: boolean,
  providerEffort: nullable(id256),
});
const certifyingReviewBindingCodec = objectCodec({
  targetGeneration: integer({ minimum: 1 }),
  slot: enumeration(["native", "other-primary", "cursor-grok", "agy-gemini"]),
  expectedSlotHeadGeneration: integer(),
  expectedChairBindingGeneration: integer({ minimum: 1 }),
  expectedOpenFindingSetDigest: sha256,
  findingWindowMode: enumeration(["normal", "resolution-only"]),
  findingCapacityReservationDigest: sha256,
});
const spawnCommon = {
  adapterId: id256,
  actionId: id256,
  operation: literal("spawn"),
  taskId: id256,
  authorityId: id256,
  payload: jsonValue,
  commandId: id256,
} as const;
export const PROVIDER_ACTION_DISPATCH_INPUT_V1_CODEC = unionOf([
  objectCodec({ ...spawnCommon, certifyingReview: literal(null) }),
  objectCodec({ ...spawnCommon, routeRequest: routeRequestCodec, certifyingReview: certifyingReviewBindingCodec }),
  objectCodec({
    adapterId: id256,
    actionId: id256,
    operation: enumeration(["send_turn", "wakeup", "release", "steer"]),
    certifyingReview: literal(null),
    payload: jsonValue,
    commandId: id256,
  }, { authorityId: id256 }),
]);

const nonReviewResultCommon = {
  kind: literal("non-review"),
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  history: arrayOf(boundedString({ maxBytes: 4096, example: "prepared" }), { maximum: 1024 }),
  executionCount: integer(),
  effectCount: integer(),
} as const;
const nonReviewProviderActionResultCodecs = [
  objectCodec({
    ...nonReviewResultCommon,
    status: enumeration(["prepared", "dispatched", "accepted", "ambiguous", "quarantined"]),
  }),
  objectCodec({
    ...nonReviewResultCommon,
    status: literal("terminal"),
  }, {
    resultDigest: sha256,
  }),
  objectCodec({
    ...nonReviewResultCommon,
    status: literal("terminal"),
    resultDigest: sha256,
    providerAnswer: boundedString({ minBytes: 1, maxBytes: 262_144, example: "Answer complete." }),
  }),
] as const;
export const PROVIDER_ACTION_RESULT_V1_CODEC = unionOf([
  ...nonReviewProviderActionResultCodecs,
  objectCodec({
    kind: literal("certifying-review"),
    action: PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC,
  }),
]);

export type ProviderActionDispatchInputV1 = CodecOutput<typeof PROVIDER_ACTION_DISPATCH_INPUT_V1_CODEC>;
export type ProviderActionResultV1 = CodecOutput<typeof PROVIDER_ACTION_RESULT_V1_CODEC>;
