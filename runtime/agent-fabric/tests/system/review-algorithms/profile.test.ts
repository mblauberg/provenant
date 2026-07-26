import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { sha256Digest } from "../../../src/review/canonical/index.ts";
import { REVIEW_BUNDLE_LIMITS, REVIEW_RISK_RULES_DIGEST } from "../../../src/review/bundle/index.ts";
import {
  evaluateActualReviewRoute,
  resolveReviewProfile,
  type ReviewProfileCatalogue,
  type SlotAvailabilityIdentity,
} from "../../../src/review/profile/index.ts";

const digest = (value: string) => sha256Digest(value);
const catalogue = JSON.parse(readFileSync(
  new URL("../../../../../config/review-profiles/certifying-review-four-slot-v1.json", import.meta.url),
  "utf8",
)) as ReviewProfileCatalogue;

function availability(): SlotAvailabilityIdentity[] {
  const selected = catalogue.chairProfiles.find((value) => value.targetChairFamily === "openai")!;
  return selected.slots.map((slot, index) => ({
    projectSessionId: "session-1",
    profileId: "certifying-review-four-slot-v1",
    profileSchemaDigest: digest("profile-schema"),
    targetChairFamily: "openai",
    slot: slot.slot,
    adapterId: slot.adapterId,
    adapterContractDigest: digest(`contract-${slot.slot}`),
    providerFamily: slot.providerFamily,
    model: slot.model,
    sourceMode: slot.sourceMode,
    runtimeIdentityDigest: digest(`runtime-${slot.slot}`),
    platformIdentityDigest: digest(`platform-${slot.slot}`),
    availabilityRevision: index + 1,
    state: "available",
    reason: null,
  }));
}

describe("certifying-review-four-slot-v1 profile", () => {
  it("resolves exactly four ordered slots with target-family reviewer relations and full availability identity", () => {
    const rows = availability();
    const resolved = resolveReviewProfile({
      catalogue,
      projectSessionId: "session-1",
      targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"),
      availability: [...rows].reverse(),
    });
    expect(resolved.slots.map((slot) => [slot.slot, slot.reviewerFamilyRelation])).toStrictEqual([
      ["native", "same-family-exempt"],
      ["other-primary", "distinct-family-proved"],
      ["cursor-grok", "distinct-family-proved"],
      ["agy-gemini", "distinct-family-proved"],
    ]);
    expect(resolveReviewProfile({
      catalogue,
      projectSessionId: "session-1",
      targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"),
      availability: rows,
    })).toStrictEqual(resolved);
    // Pin observation metadata is a sibling of chairProfiles, never a slot
    // field, so a refresh cannot change what a certifying receipt asserts.
    expect(catalogue.pinObservations).toBeDefined();
    for (const slot of resolved.slots) {
      expect(Object.keys(slot).sort()).toStrictEqual([
        "adapterClass", "adapterContractDigest", "adapterId", "explorationReadBytes", "explorationReadOps",
        "internalStepCeiling", "mandatoryReadBytes", "mandatoryReadOps", "model", "platformIdentityDigest",
        "providerFamily", "providerTurnCeiling", "requestedEffort", "requiredActualEndpointProvider",
        "requiredActualModel", "requiredActualProviderFamily", "resolvedEffort", "reviewerFamilyRelation",
        "riskReadMapDigest", "routeAliases", "runtimeIdentityDigest", "schemaVersion", "slot", "sourceMode",
      ]);
      expect(slot.riskReadMapDigest).toBe(REVIEW_RISK_RULES_DIGEST);
      if (slot.sourceMode === "direct-portal") {
        expect(slot.providerTurnCeiling).toBe(128);
        expect(slot.mandatoryReadOps + slot.explorationReadOps).toBe(REVIEW_BUNDLE_LIMITS.maximumCombinedPortalCallsDirect);
        expect(slot.providerTurnCeiling - REVIEW_BUNDLE_LIMITS.maximumCombinedPortalCallsDirect).toBe(16);
      } else {
        expect(slot.providerTurnCeiling).toBe(1);
        expect(slot.mandatoryReadOps + slot.explorationReadOps).toBe(REVIEW_BUNDLE_LIMITS.maximumCombinedPortalCallsHelper);
      }
    }
    expect(() => resolveReviewProfile({
      catalogue,
      projectSessionId: "session-1",
      targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"),
      availability: [...rows, { ...rows.find((row) => row.slot === "cursor-grok")!, adapterContractDigest: digest("crossed") }],
    })).toThrow(/availability/u);
    const crossedBudget = structuredClone(catalogue);
    crossedBudget.chairProfiles[0]!.slots[0]!.providerTurnCeiling = 16;
    expect(() => resolveReviewProfile({ catalogue: crossedBudget, projectSessionId: "session-1", targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"), availability: rows })).toThrow(/budget/u);
    const crossedRisk = structuredClone(catalogue);
    crossedRisk.chairProfiles[0]!.slots[0]!.riskReadMapDigest = digest("stale-risk-rules");
    expect(() => resolveReviewProfile({ catalogue: crossedRisk, projectSessionId: "session-1", targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"), availability: rows })).toThrow(/risk-map digest/u);
  });

  it("rejects crossed effort applicability and classifies required actual route identity", () => {
    const broken = structuredClone(catalogue);
    broken.chairProfiles[0]!.slots[2]!.requestedEffort = "xhigh";
    expect(() => resolveReviewProfile({
      catalogue: broken,
      projectSessionId: "session-1",
      targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"),
      availability: availability(),
    })).toThrow(/inapplicable/u);

    const slot = resolveReviewProfile({
      catalogue,
      projectSessionId: "session-1",
      targetChairFamily: "openai",
      profileSchemaDigest: digest("profile-schema"),
      availability: availability(),
    }).slots[2]!;
    const admission = {
      hostId: "cursor",
      adapterId: "cursor-agent",
      endpointProvider: "xai",
      family: "xai" as const,
      model: "cursor-grok-4.5-high",
      resolvedEffort: { kind: "inapplicable" as const },
      normalizedReasoningEffort: null,
      rawNativeMode: null,
      orchestrationMode: "single" as const,
    };
    const observed = <T,>(value: T) => ({ state: "observed" as const, value, source: "provider-result" as const, confidence: "exact" as const });
    const unavailable = { state: "unavailable" as const, value: null, source: "unavailable" as const, confidence: "unknown" as const };
    const observation = {
      hostId: unavailable,
      adapterId: observed("cursor-agent"),
      endpointProvider: observed("xai"),
      family: observed("xai" as const),
      model: observed("cursor-grok-4.5-high"),
      resolvedEffort: observed({ kind: "inapplicable" as const }),
      normalizedReasoningEffort: observed(null),
      rawNativeMode: observed(null),
      orchestrationMode: observed("single" as const),
    };
    const base = { slot, admissionDigest: digest("admission"), observationDigest: digest("observation"), admission };
    expect(evaluateActualReviewRoute({ ...base, observation }).status).toBe("proved-equal");
    const completeObservation = { ...observation, hostId: observed("cursor") };
    expect(evaluateActualReviewRoute({ ...base, observation: completeObservation }).status).toBe("proved-equal");
    for (const field of ["endpointProvider", "family", "model"] as const) {
      expect(evaluateActualReviewRoute({ ...base, observation: { ...completeObservation, [field]: unavailable } }).status,
        `${field} unavailable`).toBe("actual-route-unproved");
    }
    for (const field of ["hostId", "adapterId", "resolvedEffort", "normalizedReasoningEffort", "rawNativeMode", "orchestrationMode"] as const) {
      expect(evaluateActualReviewRoute({ ...base, observation: { ...completeObservation, [field]: unavailable } }).status,
        `${field} honestly unavailable`).toBe("proved-equal");
    }
    expect(evaluateActualReviewRoute({ ...base, observation: { ...observation, family: unavailable } }).status)
      .toBe("actual-route-unproved");
    expect(evaluateActualReviewRoute({ ...base, observation: { ...completeObservation, model: observed("wrong"), hostId: unavailable } }).status)
      .toBe("actual-route-mismatch");
    expect(evaluateActualReviewRoute({ ...base, observation: { ...completeObservation, hostId: observed("wrong") } }).status)
      .toBe("actual-route-mismatch");
    expect(evaluateActualReviewRoute({ ...base, observation: { ...completeObservation,
      endpointProvider: unavailable, hostId: observed("wrong") } }).status).toBe("actual-route-mismatch");
  });
});
