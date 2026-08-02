import {
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
import { REVIEW_SLOTS } from "./provider-review-core.js";

const positive = integer({ minimum: 1 });
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const nullableDigest = nullable(sha256);
const reviewSlot = enumeration(REVIEW_SLOTS);

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
  state: enumeration([
    "detected", "inspecting", "terminal-proved-no-effect", "terminal-proved-usage", "awaiting-human-retire", "terminal-retired-unknown",
  ]),
  reason: enumeration([
    "intact-effect-ambiguity", "route-row-missing", "route-row-conflict", "route-receipt-mismatch", "target-binding-invalid", "bundle-binding-invalid", "prompt-binding-invalid", "profile-binding-invalid", "lineage-binding-invalid",
  ]),
  reservationDigest: sha256,
  routeState: enumeration(["present", "missing", "integrity-failed"]),
  routeReceiptDigest: nullableDigest,
  lookupState: enumeration(["not-attempted", "in-flight", "completed"]),
  lookupEvidenceDigest: nullableDigest,
  disposition: nullable(enumeration([
    "proved-no-effect-release", "exact-usage-settled", "conservative-full-ceiling-settled", "full-ceiling-retired",
  ])),
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

export type ProviderRouteIntegrityRecoveryReadRequestV1 = CodecOutput<typeof PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_REQUEST_V1_CODEC>;
export type ProviderRouteIntegrityRecoveryProjectionV1 = CodecOutput<typeof PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC>;
export type ProviderRouteIntegrityRecoveryReadErrorV1 = CodecOutput<typeof PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_ERROR_V1_CODEC>;
