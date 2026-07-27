import { canonicalString, digestCanonical, type Sha256Digest } from "../canonical/index.js";
import { REVIEW_BUNDLE_LIMITS, REVIEW_RISK_RULES_DIGEST } from "../bundle/index.js";
import {
  REVIEW_PROFILE_CATALOGUE_DIGEST,
  REVIEW_PROFILE_DOCUMENT_DIGEST,
} from "./catalogue-digest.generated.js";

export type ReviewSlot = "native" | "other-primary" | "cursor-grok" | "agy-gemini";
export type ModelFamily = "openai" | "anthropic" | "xai" | "google";
export type ResolvedEffort = { kind: "applied"; value: string } | { kind: "inapplicable" };

export interface ReviewProfileCatalogue {
  schemaVersion: 1;
  profileId: "certifying-review-four-slot-v1";
  /**
   * Dated evidence that each pinned identity was observed to be what the slots
   * declare. Deliberately a sibling of `chairProfiles`, never a slot field:
   * `resolveReviewProfile` spreads slots into `ResolvedReviewProfileSlot`, whose
   * schema is closed and which certifying evidence byte-matches, so a slot-level
   * observation field would change what a certifying receipt asserts. It still
   * lives inside the digest-bound catalogue document, so a refresh is a
   * deliberate, reviewable commit.
   */
  pinObservations?: readonly {
    providerFamily: ModelFamily;
    model: string;
    routeAlias: string;
    observedOn: string;
    observedVia: "claude-subscription-canary" | "codex-model-catalogue" | "manual-attestation";
  }[];
  chairProfiles: readonly {
    targetChairFamily: "openai" | "anthropic";
    slots: readonly {
      slot: ReviewSlot;
      adapterClass: "primary-native" | "equal-primary" | "cursor" | "agy";
      adapterId: string;
      providerFamily: ModelFamily;
      model: string;
      requiredActualEndpointProvider: string;
      requestedEffort: string | null;
      resolvedEffort: ResolvedEffort;
      sourceMode: "direct-portal" | "portal-helper";
      providerTurnCeiling: number;
      internalStepCeiling: number;
      mandatoryReadOps: number;
      mandatoryReadBytes: number;
      explorationReadOps: number;
      explorationReadBytes: number;
      routeAliases: readonly string[];
      riskReadMapDigest: Sha256Digest;
    }[];
  }[];
}

export interface SlotAvailabilityIdentity {
  projectSessionId: string;
  profileId: "certifying-review-four-slot-v1";
  profileSchemaDigest: Sha256Digest;
  targetChairFamily: "openai" | "anthropic";
  slot: ReviewSlot;
  adapterId: string;
  adapterContractDigest: Sha256Digest;
  providerFamily: ModelFamily;
  model: string;
  sourceMode: "direct-portal" | "portal-helper";
  runtimeIdentityDigest: Sha256Digest;
  platformIdentityDigest: Sha256Digest;
  availabilityRevision: number;
  state: "available" | "unavailable";
  reason: string | null;
}

export interface ResolvedReviewProfileSlot {
  schemaVersion: 1;
  slot: ReviewSlot;
  adapterClass: "primary-native" | "equal-primary" | "cursor" | "agy";
  adapterId: string;
  adapterContractDigest: Sha256Digest;
  providerFamily: ModelFamily;
  model: string;
  requiredActualEndpointProvider: string;
  requiredActualProviderFamily: ModelFamily;
  requiredActualModel: string;
  requestedEffort: string | null;
  resolvedEffort: ResolvedEffort;
  sourceMode: "direct-portal" | "portal-helper";
  runtimeIdentityDigest: Sha256Digest;
  platformIdentityDigest: Sha256Digest;
  providerTurnCeiling: number;
  internalStepCeiling: number;
  mandatoryReadOps: number;
  mandatoryReadBytes: number;
  explorationReadOps: number;
  explorationReadBytes: number;
  routeAliases: readonly string[];
  riskReadMapDigest: Sha256Digest;
  reviewerFamilyRelation: "same-family-exempt" | "distinct-family-proved";
}

export interface ResolvedReviewProfile {
  schemaVersion: 1;
  profileId: "certifying-review-four-slot-v1";
  profileSchemaDigest: Sha256Digest;
  targetChairFamily: "openai" | "anthropic";
  slots: readonly ResolvedReviewProfileSlot[];
  resolvedProfileDigest: Sha256Digest;
}

export function resolveReviewProfile(_input: Readonly<{
  catalogue: ReviewProfileCatalogue;
  projectSessionId: string;
  targetChairFamily: "openai" | "anthropic";
  profileSchemaDigest: Sha256Digest;
  availability: readonly SlotAvailabilityIdentity[];
}>): ResolvedReviewProfile {
  const input = _input;
  // Startup verifies the on-disk schema/profile catalogue against this
  // checked-in digest. The wire field remains the caller's claim and the
  // availability foreign key, but it is authoritative only when it equals
  // that startup-verified value.
  if (input.profileSchemaDigest !== REVIEW_PROFILE_CATALOGUE_DIGEST) {
    throw new TypeError("profileSchemaDigest differs from the authoritative checked-in catalogue digest");
  }
  if (input.catalogue.schemaVersion !== 1 || input.catalogue.profileId !== "certifying-review-four-slot-v1") {
    throw new TypeError("review profile catalogue identity is invalid");
  }
  const profiles = input.catalogue.chairProfiles.filter((value) => value.targetChairFamily === input.targetChairFamily);
  if (profiles.length !== 1) throw new TypeError("target chair profile is missing or ambiguous");
  const profile = profiles[0]!;
  const expectedOrder: readonly ReviewSlot[] = ["native", "other-primary", "cursor-grok", "agy-gemini"];
  if (profile.slots.length !== 4 || profile.slots.some((slot, index) => slot.slot !== expectedOrder[index])) {
    throw new TypeError("review profile must contain exactly four ordered slots");
  }
  const resolvedSlots = profile.slots.map((slot): ResolvedReviewProfileSlot => {
    const direct = slot.sourceMode === "direct-portal";
    const budgetIsExact = slot.providerTurnCeiling === (direct ? 128 : 1)
      && slot.internalStepCeiling === (direct ? 0 : 128)
      && slot.mandatoryReadOps === REVIEW_BUNDLE_LIMITS.maximumMandatoryReads
      && slot.mandatoryReadBytes === REVIEW_BUNDLE_LIMITS.maximumMandatoryWireBytes
      && slot.explorationReadOps === (direct ? 32 : 48)
      && slot.explorationReadBytes === 4 * 1_024 * 1_024
      && slot.mandatoryReadOps + slot.explorationReadOps === (direct
        ? REVIEW_BUNDLE_LIMITS.maximumCombinedPortalCallsDirect : REVIEW_BUNDLE_LIMITS.maximumCombinedPortalCallsHelper)
      && slot.mandatoryReadBytes + slot.explorationReadBytes === REVIEW_BUNDLE_LIMITS.maximumCombinedPortalBytes;
    if (!budgetIsExact) throw new TypeError(`review slot budget is not exact: ${slot.slot}`);
    if (slot.riskReadMapDigest !== REVIEW_RISK_RULES_DIGEST) {
      throw new TypeError(`review slot risk-map digest differs from current rules: ${slot.slot}`);
    }
    if ((slot.resolvedEffort.kind === "inapplicable" && slot.requestedEffort !== null)
      || (slot.resolvedEffort.kind === "applied" && slot.resolvedEffort.value.length === 0)) {
      throw new TypeError("inapplicable/applied effort is crossed with its request");
    }
    if (slot.routeAliases.length === 0 || new Set(slot.routeAliases).size !== slot.routeAliases.length
      || slot.routeAliases.some((value, index) => index > 0 && value <= slot.routeAliases[index - 1]!)) {
      throw new TypeError("review route aliases must be ordered and unique");
    }
    const rows = input.availability.filter((row) => row.projectSessionId === input.projectSessionId
      && row.profileId === input.catalogue.profileId
      && row.profileSchemaDigest === input.profileSchemaDigest
      && row.targetChairFamily === input.targetChairFamily
      && row.slot === slot.slot
      && row.adapterId === slot.adapterId
      && row.providerFamily === slot.providerFamily
      && row.model === slot.model
      && row.sourceMode === slot.sourceMode);
    if (rows.length !== 1 || rows[0]!.state !== "available" || rows[0]!.reason !== null) {
      throw new TypeError(`slot availability identity is missing, unavailable or ambiguous: ${slot.slot}`);
    }
    const row = rows[0]!;
    if (!Number.isSafeInteger(row.availabilityRevision) || row.availabilityRevision < 1) {
      throw new TypeError("slot availability revision is invalid");
    }
    const reviewerFamilyRelation = slot.slot === "native"
      ? "same-family-exempt"
      : "distinct-family-proved";
    if ((slot.slot === "native" && slot.providerFamily !== input.targetChairFamily)
      || (slot.slot !== "native" && slot.providerFamily === input.targetChairFamily)) {
      throw new TypeError("reviewer family relation is incompatible with the target chair");
    }
    return {
      schemaVersion: 1,
      ...slot,
      adapterContractDigest: row.adapterContractDigest,
      requiredActualProviderFamily: slot.providerFamily,
      requiredActualModel: slot.model,
      runtimeIdentityDigest: row.runtimeIdentityDigest,
      platformIdentityDigest: row.platformIdentityDigest,
      reviewerFamilyRelation,
    };
  });
  // The combined digest above is the authoritative wire/availability foreign
  // key. After the detailed validation errors, this member pin additionally
  // binds the caller-supplied object used for resolution to the profile
  // document that startup verified on disk.
  if (digestCanonical(input.catalogue) !== REVIEW_PROFILE_DOCUMENT_DIGEST) {
    throw new TypeError("caller-supplied catalogue differs from the pinned profile member");
  }
  const withoutDigest = {
    schemaVersion: 1 as const,
    profileId: "certifying-review-four-slot-v1" as const,
    profileSchemaDigest: input.profileSchemaDigest,
    targetChairFamily: input.targetChairFamily,
    slots: resolvedSlots,
  };
  return { ...withoutDigest, resolvedProfileDigest: digestCanonical(withoutDigest) };
}

export type ObservedValue<T> =
  | { state: "observed"; value: T; source: "provider-result" | "adapter-attestation"; confidence: "exact" | "attested" }
  | { state: "unavailable"; value: null; source: "unavailable"; confidence: "unknown" };

export function evaluateActualReviewRoute(_input: Readonly<{
  slot: ResolvedReviewProfileSlot;
  admissionDigest: Sha256Digest;
  observationDigest: Sha256Digest;
  admission: {
    hostId: string;
    adapterId: string;
    endpointProvider: string;
    family: ModelFamily;
    model: string;
    resolvedEffort: ResolvedEffort;
    normalizedReasoningEffort: string | null;
    rawNativeMode: string | null;
    orchestrationMode: "single" | "native-subagents" | "dynamic-workflow" | "provider-multi-agent";
  };
  observation: {
    hostId: ObservedValue<string>;
    adapterId: ObservedValue<string>;
    endpointProvider: ObservedValue<string>;
    family: ObservedValue<ModelFamily>;
    model: ObservedValue<string>;
    resolvedEffort: ObservedValue<ResolvedEffort>;
    normalizedReasoningEffort: ObservedValue<string | null>;
    rawNativeMode: ObservedValue<string | null>;
    orchestrationMode: ObservedValue<"single" | "native-subagents" | "dynamic-workflow" | "provider-multi-agent">;
  };
}>): Readonly<
  | { status: "proved-equal"; actualRouteIdentityDigest: Sha256Digest }
  | { status: "actual-route-unproved"; actualRouteIdentityDigest: null }
  | { status: "actual-route-mismatch"; actualRouteIdentityDigest: Sha256Digest | null }
> {
  const input = _input;
  const observedValue = <T>(value: ObservedValue<T>): T | undefined =>
    value.state === "observed" ? value.value : undefined;
  const requiredObserved = input.observation.endpointProvider.state === "observed"
    && input.observation.family.state === "observed"
    && input.observation.model.state === "observed";
  const comparisons: Array<[ObservedValue<unknown>, unknown]> = [
    [input.observation.hostId, input.admission.hostId],
    [input.observation.adapterId, input.admission.adapterId],
    [input.observation.endpointProvider, input.admission.endpointProvider],
    [input.observation.family, input.admission.family],
    [input.observation.model, input.admission.model],
    [input.observation.resolvedEffort, input.admission.resolvedEffort],
    [input.observation.normalizedReasoningEffort, input.admission.normalizedReasoningEffort],
    [input.observation.rawNativeMode, input.admission.rawNativeMode],
    [input.observation.orchestrationMode, input.admission.orchestrationMode],
  ];
  let mismatch = comparisons.some(([actual, admitted]) => actual.state === "observed"
    && canonicalString(actual.value) !== canonicalString(admitted));
  if (input.observation.endpointProvider.state === "observed"
    && input.observation.endpointProvider.value !== input.slot.requiredActualEndpointProvider) mismatch = true;
  if (input.observation.family.state === "observed"
    && input.observation.family.value !== input.slot.requiredActualProviderFamily) mismatch = true;
  if (input.observation.model.state === "observed"
    && input.observation.model.value !== input.slot.requiredActualModel) mismatch = true;
  if (input.observation.rawNativeMode.state === "observed"
    && input.observation.rawNativeMode.value === null
    && input.observation.orchestrationMode.state === "observed"
    && observedValue(input.observation.orchestrationMode) !== "single") mismatch = true;

  const identity = {
    schemaVersion: 1,
    admissionDigest: input.admissionDigest,
    observationDigest: input.observationDigest,
    hostId: input.observation.hostId,
    adapterId: input.observation.adapterId,
    endpointProvider: input.observation.endpointProvider,
    family: input.observation.family,
    model: input.observation.model,
    resolvedEffort: input.observation.resolvedEffort,
    normalizedReasoningEffort: input.observation.normalizedReasoningEffort,
    rawNativeMode: input.observation.rawNativeMode,
    orchestrationMode: input.observation.orchestrationMode,
  };
  const actualRouteIdentityDigest = requiredObserved ? digestCanonical(identity) : null;
  if (mismatch) return { status: "actual-route-mismatch", actualRouteIdentityDigest };
  if (!requiredObserved) return { status: "actual-route-unproved", actualRouteIdentityDigest: null };
  return { status: "proved-equal", actualRouteIdentityDigest: actualRouteIdentityDigest! };
}
