import { createRequire } from "node:module";

import { Ajv2020 } from "ajv/dist/2020.js";
import type { FormatsPlugin } from "ajv-formats";
import { describe, expect, it } from "vitest";

import {
  ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC,
  PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC,
  REVIEW_COMPLETION_V1_CODEC,
  REVIEW_EVIDENCE_RECORD_V1_CODEC,
  REVIEW_SLOT_V1_CODEC,
  addProtocolSchemaKeywords,
} from "../src/index.js";

const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const actionRef = { adapterId: "adapter_01", actionId: "action_01" };

function certificationBasis() {
  return {
    kind: "active-binding",
    actionBindingGeneration: 1,
    activeBindingGeneration: 1,
    terminalSequence: 1,
    bindingChainDigest: digest,
  } as const;
}

function cleanSlot(slot: "native" | "other-primary" | "cursor-grok" | "agy-gemini") {
  return {
    ...REVIEW_SLOT_V1_CODEC.example,
    slot,
    headGeneration: 1,
    attemptGeneration: 1,
    actionRef,
    evidenceId: `evidence_${slot}`,
    terminalKind: "safe-answer",
    verdict: "CLEAN",
    resultDigest: digest,
    providerFailureCode: null,
    providerFailureDigest: null,
    routeReceiptDigest: digest,
    adapterId: actionRef.adapterId,
    endpointProvider: slot === "cursor-grok" ? "xai" : slot === "agy-gemini" ? "google" : "provider",
    providerFamily: slot === "cursor-grok" ? "xai" : slot === "agy-gemini" ? "google" : "openai",
    model: "model_01",
    routeObservationDigest: digest,
    actualRouteIdentityDigest: digest,
    readCoverageDigest: digest,
    reviewerFamilyRelation: slot === "native" ? "same-family-exempt" : "distinct-family-proved",
    currentCertificationBasis: certificationBasis(),
    certifying: true,
    openFindingSet: { findingSetDigest: digest, findingCount: 0, pageDigests: [] },
    blockers: [],
  } as const;
}

function completeReview() {
  return {
    ...REVIEW_COMPLETION_V1_CODEC.example,
    blockers: [],
    targetGeneration: 1,
    targetChair: {
      agentId: "chair_01",
      bindingGeneration: 1,
      principalGeneration: 1,
      chairLeaseGeneration: 1,
      providerSessionGeneration: 1,
      bridgeGeneration: 1,
      adapterId: "codex-app-server",
      adapterContractDigest: digest,
      modelFamily: "openai",
      model: "model_01",
      routeReceiptDigest: digest,
    },
    reviewedArtifactRef: "artifact_01",
    publicationLineageDigest: digest,
    bundleDigest: digest,
    manifestRootDigest: digest,
    coverageDigest: digest,
    riskReadMapDigest: digest,
    mandatoryReadSetDigest: digest,
    profileDigest: digest,
    unavailableSlots: [],
    slots: [cleanSlot("native"), cleanSlot("other-primary"), cleanSlot("cursor-grok"), cleanSlot("agy-gemini")],
    finalReviewComplete: true,
  } as const;
}

describe("Agent Fabric review correlation repair", () => {
  it("binds effective-configuration subjectKind to its exact subjectRef and activation parent arm", () => {
    const activation = ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC.example;
    expect(() => ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC.parse({
      ...activation,
      subjectKind: "provider-action",
    }, "configuration")).toThrow(/subjectKind|subjectRef/);
    expect(() => ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC.parse({
      ...activation,
      subjectKind: "provider-smoke",
      subjectRef: { smokeId: "smoke_01", actionRef },
      activationConfigurationRef: null,
    }, "configuration")).toThrow(/activationConfigurationRef/);
  });

  it("allows certifying only for a safe answer with a current certifying basis", () => {
    const projection = PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.example;
    expect(() => PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse({
      ...projection,
      status: "terminal",
      terminalReview: {
        kind: "provider-terminal-failure",
        terminalSequence: 1,
        terminalResultDigest: digest,
        currentCertificationBasis: null,
        providerAssurance: "full-vendor-identity",
        certifying: true,
        providerAnswerDigest: null,
        reviewResultDigest: null,
        verdict: null,
        failureCode: "provider-rejected",
        noEffectEvidenceDigest: null,
        integrityEvidenceDigest: null,
        retirementEvidenceDigest: null,
        readCoverageDigest: null,
        coverageSummaryDigest: null,
      },
      evidenceMutationReceipt: null,
    }, "projection")).toThrow(/certifying/);
  });

  it("correlates evidence and slot action adapters and terminal arms", () => {
    const evidence = {
      ...REVIEW_EVIDENCE_RECORD_V1_CODEC.example,
      actionRef,
      adapterId: "wrong-adapter",
      terminalKind: "safe-answer",
      verdict: "CLEAN",
      answerSafety: "safe",
      reviewResultDigest: digest,
    };
    expect(() => REVIEW_EVIDENCE_RECORD_V1_CODEC.parse(evidence, "evidence")).toThrow(/adapterId/);

    expect(() => REVIEW_SLOT_V1_CODEC.parse({
      ...cleanSlot("native"),
      terminalKind: "safe-answer",
      verdict: "UNUSABLE",
    }, "slot")).toThrow(/terminalKind|verdict/);
    expect(() => REVIEW_SLOT_V1_CODEC.parse({
      ...REVIEW_SLOT_V1_CODEC.example,
      terminalKind: null,
      verdict: "CLEAN",
    }, "slot")).toThrow(/terminalKind|verdict/);
  });

  it("makes finalReviewComplete exactly imply and follow the full clean-slot predicate", () => {
    const complete = completeReview();
    expect(REVIEW_COMPLETION_V1_CODEC.parse(complete, "completion")).toStrictEqual(complete);
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse({
      ...complete,
      slots: complete.slots.map((slot, index) => index === 0 ? {
        ...slot,
        openFindingSet: { findingSetDigest: digest, findingCount: 1, pageDigests: [digest] },
      } : slot),
    }, "completion")).toThrow(/openFindingSet|open findings|finalReviewComplete/);
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse({ ...complete, finalReviewComplete: false }, "completion"))
      .toThrow(/finalReviewComplete/);
  });

  it("keeps the generated completion schema in parity with the completion predicate", () => {
    const addFormats = createRequire(import.meta.url)("ajv-formats") as FormatsPlugin;
    const ajv = new Ajv2020({ strict: false, allErrors: true });
    addFormats(ajv);
    addProtocolSchemaKeywords(ajv);
    const validate = ajv.compile(REVIEW_COMPLETION_V1_CODEC.schema);
    const invalid = completeReview();
    const crossed = {
      ...invalid,
      slots: invalid.slots.map((slot, index) => index === 1 ? {
        ...slot,
        reviewerFamilyRelation: "same-family-exempt",
      } : slot),
    };
    expect(validate(crossed)).toBe(false);
  });
});
