import { Ajv2020 } from "ajv/dist/2020.js";
import { describe, expect, it } from "vitest";

import {
  ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC,
  DEPLOYED_ROUTE_OBSERVATION_V1_CODEC,
  PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC,
  PROVIDER_ROUTE_PROJECTION_V1_CODEC,
  REVIEW_COMPLETION_V1_CODEC,
  REVIEW_EVIDENCE_CURRENCY_V1_CODEC,
  REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC,
  REVIEW_EVIDENCE_RECORD_V1_CODEC,
  REVIEW_EVIDENCE_READ_V1_CODEC,
  REVIEW_SLOT_V1_CODEC,
  SAFE_FINDING_V1_CODEC,
  addProtocolSchemaKeywords,
} from "../src/index.js";

const digestA = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const digestB = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const actionRef = { adapterId: "adapter_01", actionId: "action_01" };

function observed<T>(value: T) {
  return { state: "observed", value, source: "provider-result", confidence: "exact" } as const;
}

function unavailable() {
  return { state: "unavailable", value: null, source: "unavailable", confidence: "unknown" } as const;
}

function activeBasis() {
  return {
    kind: "active-binding",
    actionBindingGeneration: 1,
    activeBindingGeneration: 1,
    terminalSequence: 1,
    bindingChainDigest: digestA,
  } as const;
}

function terminalProjection() {
  const route = {
    ...PROVIDER_ROUTE_PROJECTION_V1_CODEC.example,
    adapterId: actionRef.adapterId,
    targetGeneration: 1,
    slot: "native",
    slotHeadGeneration: 0,
    attemptGeneration: 1,
    providerAssurance: "full-vendor-identity",
  } as const;
  const terminalReview = {
    kind: "safe-answer",
    terminalSequence: 1,
    terminalResultDigest: digestA,
    currentCertificationBasis: activeBasis(),
    providerAssurance: "full-vendor-identity",
    certifying: true,
    providerAnswerDigest: digestA,
    reviewResultDigest: digestA,
    verdict: "CLEAN",
    failureCode: null,
    noEffectEvidenceDigest: null,
    integrityEvidenceDigest: null,
    retirementEvidenceDigest: null,
    readCoverageDigest: digestA,
    coverageSummaryDigest: digestB,
  } as const;
  const evidenceMutationReceipt = {
    ...REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.example,
    actionRef,
    targetGeneration: 1,
    slot: "native",
    attemptGeneration: 1,
    terminalSequence: 1,
    terminalResultDigest: digestA,
    readCoverageDigest: digestA,
    coverageSummaryDigest: digestB,
  } as const;
  return {
    ...PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.example,
    actionRef,
    status: "terminal",
    routeState: "present",
    route,
    routeRecoveryEvidenceDigest: null,
    terminalReview,
    evidenceMutationReceipt,
  } as const;
}

function cleanSlot(slot: "native" | "other-primary" | "cursor-grok" | "agy-gemini") {
  return {
    ...REVIEW_SLOT_V1_CODEC.example,
    slot,
    headGeneration: 1,
    attemptGeneration: 1,
    actionRef,
    adapterId: actionRef.adapterId,
    evidenceId: `evidence_${slot}`,
    terminalKind: "safe-answer",
    verdict: "CLEAN",
    resultDigest: digestA,
    routeReceiptDigest: digestA,
    routeObservationDigest: digestA,
    actualRouteIdentityDigest: digestA,
    readCoverageDigest: digestA,
    reviewerFamilyRelation: slot === "native" ? "same-family-exempt" : "distinct-family-proved",
    currentCertificationBasis: activeBasis(),
    providerAssurance: "full-vendor-identity",
    certifying: true,
    openFindingSet: { findingSetDigest: digestA, findingCount: 0, pageDigests: [] },
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
      adapterId: "adapter_01",
      adapterContractDigest: digestA,
      modelFamily: "openai",
      model: "model_01",
      routeReceiptDigest: digestA,
    },
    reviewedArtifactRef: "artifact_01",
    publicationLineageDigest: digestA,
    bundleDigest: digestA,
    manifestRootDigest: digestA,
    coverageDigest: digestA,
    riskReadMapDigest: digestA,
    mandatoryReadSetDigest: digestA,
    profileDigest: digestA,
    unavailableSlots: [],
    slots: [cleanSlot("native"), cleanSlot("other-primary"), cleanSlot("cursor-grok"), cleanSlot("agy-gemini")],
    finalReviewComplete: true,
  } as const;
}

function ajv() {
  const instance = new Ajv2020({ strict: false, allErrors: true });
  addProtocolSchemaKeywords(instance);
  return instance;
}

describe("Agent Fabric re-review protocol repair", () => {
  it("equality-binds a terminal evidence receipt to action, route, terminal and coverage", () => {
    const valid = terminalProjection();
    expect(PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse(valid, "terminal")).toStrictEqual(valid);
    const invalid = {
      ...valid,
      evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, targetGeneration: 2 },
    };
    expect(() => PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse(invalid, "terminal")).toThrow(/targetGeneration|route|receipt/);
    expect(ajv().compile(PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.schema)(invalid)).toBe(false);
    const crossed = [
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, actionRef: { ...actionRef, actionId: "action_02" } } },
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, slot: "other-primary" } },
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, attemptGeneration: 2 } },
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, terminalSequence: 2 } },
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, terminalResultDigest: digestB } },
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, readCoverageDigest: digestB } },
      { ...valid, evidenceMutationReceipt: { ...valid.evidenceMutationReceipt, coverageSummaryDigest: digestA } },
    ];
    crossed.forEach((candidate) => {
      expect(() => PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse(candidate, "terminal")).toThrow(/evidenceMutationReceipt/);
    });
  });

  it("binds mutation receipt head generations to the prior evidence tuple", () => {
    const receipt = REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.example;
    expect(REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.parse(receipt, "receipt")).toStrictEqual(receipt);
    const skipped = { ...receipt, newHeadGeneration: 2 };
    expect(() => REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.parse(skipped, "receipt")).toThrow(/newHeadGeneration|priorHeadGeneration/);
    expect(ajv().compile(REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.schema)(skipped)).toBe(false);
    const crossedPrior = { ...receipt, priorHeadGeneration: 1, newHeadGeneration: 2, priorEvidenceId: null };
    expect(() => REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.parse(crossedPrior, "receipt")).toThrow(/priorEvidenceId|prior head/);
    expect(ajv().compile(REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.schema)(crossedPrior)).toBe(false);
  });

  it("binds terminal and evidence-record head tuples to their exact predecessor", () => {
    const terminal = terminalProjection();
    const crossedTerminal = {
      ...terminal,
      evidenceMutationReceipt: {
        ...terminal.evidenceMutationReceipt,
        priorHeadGeneration: 1,
        newHeadGeneration: 2,
        priorEvidenceId: "evidence_prior",
      },
    };
    expect(() => PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse(crossedTerminal, "terminal"))
      .toThrow(/priorHeadGeneration|slotHeadGeneration/);
    expect(ajv().compile(PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.schema)(crossedTerminal)).toBe(false);

    const validRecord = {
      ...REVIEW_EVIDENCE_RECORD_V1_CODEC.example,
      priorHeadGeneration: 0,
      newHeadGeneration: 1,
      priorEvidenceId: null,
    };
    expect(REVIEW_EVIDENCE_RECORD_V1_CODEC.parse(validRecord, "record")).toStrictEqual(validRecord);
    const skippedRecord = { ...validRecord, newHeadGeneration: 2 };
    expect(() => REVIEW_EVIDENCE_RECORD_V1_CODEC.parse(skippedRecord, "record"))
      .toThrow(/newHeadGeneration|priorHeadGeneration/);
    expect(ajv().compile(REVIEW_EVIDENCE_RECORD_V1_CODEC.schema)(skippedRecord)).toBe(false);
    const crossedPriorRecord = {
      ...validRecord,
      priorHeadGeneration: 1,
      newHeadGeneration: 2,
      priorEvidenceId: null,
    };
    expect(() => REVIEW_EVIDENCE_RECORD_V1_CODEC.parse(crossedPriorRecord, "record"))
      .toThrow(/priorEvidenceId|prior head/);
    expect(ajv().compile(REVIEW_EVIDENCE_RECORD_V1_CODEC.schema)(crossedPriorRecord)).toBe(false);
  });

  it("requires positive clean-slot head and attempt generations in both completion validators", () => {
    const invalid = {
      ...completeReview(),
      slots: completeReview().slots.map((slot, index) => index === 0 ? { ...slot, headGeneration: 0 } : slot),
    };
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse(invalid, "completion")).toThrow(/headGeneration|positive head|finalReviewComplete/);
    expect(ajv().compile(REVIEW_COMPLETION_V1_CODEC.schema)(invalid)).toBe(false);
  });

  it("makes certifying review slots intrinsically route-proved and family-valid", () => {
    const clean = cleanSlot("native");
    const invalidSlots = [
      { ...clean, evidenceId: null },
      { ...clean, routeObservationDigest: null },
      { ...clean, actualRouteIdentityDigest: null },
      { ...clean, readCoverageDigest: null },
      { ...clean, reviewerFamilyRelation: "distinct-family-proved" },
      { ...clean, blockers: ["noncertifying"] },
    ];
    const validate = ajv().compile(REVIEW_SLOT_V1_CODEC.schema);
    invalidSlots.forEach((candidate) => {
      expect(() => REVIEW_SLOT_V1_CODEC.parse(candidate, "slot"))
        .toThrow(/certifying|evidence|route|coverage|family|blocker/);
      expect(validate(candidate)).toBe(false);
    });

    const certifyingFindings = {
      ...cleanSlot("other-primary"),
      verdict: "FINDINGS",
      openFindingSet: { findingSetDigest: digestA, findingCount: 1, pageDigests: [digestA] },
      blockers: ["open-findings"],
    } as const;
    expect(REVIEW_SLOT_V1_CODEC.parse(certifyingFindings, "slot")).toStrictEqual(certifyingFindings);
    expect(validate(certifyingFindings)).toBe(true);
  });

  it("enforces missing-target branches and deterministic blocker/unavailable ordering", () => {
    const invalidMissing = { ...REVIEW_COMPLETION_V1_CODEC.example, targetGeneration: 1 };
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse(invalidMissing, "completion")).toThrow(/missing-target|branch/);

    const unavailableSlot = (slot: "native" | "other-primary") => ({
      projectSessionId: "session_01",
      profileId: "certifying-review-four-slot-v1",
      profileSchemaDigest: digestA,
      targetChairFamily: "openai",
      slot,
      adapterId: "adapter_01",
      adapterContractDigest: digestA,
      providerFamily: "openai",
      model: "model_01",
      sourceMode: "direct-portal",
      runtimeIdentityDigest: digestA,
      platformIdentityDigest: digestA,
      availabilityRevision: 1,
      reason: "adapter-inactive",
    } as const);
    const invalidOrder = {
      ...REVIEW_COMPLETION_V1_CODEC.example,
      blockers: ["certifying-review-capability-unavailable"],
      unavailableSlots: [unavailableSlot("other-primary"), unavailableSlot("native")],
    };
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse(invalidOrder, "completion")).toThrow(/unavailableSlots|order/);
    expect(ajv().compile(REVIEW_COMPLETION_V1_CODEC.schema)(invalidOrder)).toBe(false);

    const impossibleStale = {
      ...REVIEW_COMPLETION_V1_CODEC.example,
      blockers: ["stale-target"],
      targetGeneration: 1,
    };
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse(impossibleStale, "completion")).toThrow(/stale-target|branch/);
    expect(ajv().compile(REVIEW_COMPLETION_V1_CODEC.schema)(impossibleStale)).toBe(false);
  });

  it("preserves immutable target identity in target-present empty-slot branches", () => {
    const immutableTarget = {
      targetGeneration: 1,
      reviewedArtifactRef: "artifact_01",
      publicationLineageDigest: digestA,
      bundleDigest: digestA,
      manifestRootDigest: digestA,
      coverageDigest: digestA,
      riskReadMapDigest: digestA,
      mandatoryReadSetDigest: digestA,
    } as const;
    const targetChair = {
      agentId: "chair_01",
      bindingGeneration: 1,
      principalGeneration: 1,
      chairLeaseGeneration: 1,
      providerSessionGeneration: 1,
      bridgeGeneration: 1,
      adapterId: "adapter_01",
      adapterContractDigest: digestA,
      modelFamily: "openai",
      model: "model_01",
      routeReceiptDigest: digestA,
    } as const;
    const invalidIntegrity = {
      ...REVIEW_COMPLETION_V1_CODEC.example,
      blockers: ["integrity-failure"],
      targetGeneration: 1,
    };
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse(invalidIntegrity, "completion"))
      .toThrow(/integrity-failure|immutable target/);
    expect(ajv().compile(REVIEW_COMPLETION_V1_CODEC.schema)(invalidIntegrity)).toBe(false);

    const validIntegrity = {
      ...REVIEW_COMPLETION_V1_CODEC.example,
      ...immutableTarget,
      blockers: ["integrity-failure"],
    };
    expect(REVIEW_COMPLETION_V1_CODEC.parse(validIntegrity, "completion")).toStrictEqual(validIntegrity);

    const invalidProfile = {
      ...REVIEW_COMPLETION_V1_CODEC.example,
      ...immutableTarget,
      blockers: ["profile-unavailable"],
    };
    expect(() => REVIEW_COMPLETION_V1_CODEC.parse(invalidProfile, "completion"))
      .toThrow(/profile-unavailable|targetChair/);
    expect(ajv().compile(REVIEW_COMPLETION_V1_CODEC.schema)(invalidProfile)).toBe(false);

    const validProfile = {
      ...invalidProfile,
      targetChair,
    };
    expect(REVIEW_COMPLETION_V1_CODEC.parse(validProfile, "completion")).toStrictEqual(validProfile);
    expect(ajv().compile(REVIEW_COMPLETION_V1_CODEC.schema)(validProfile)).toBe(true);
  });

  it("keeps safe-answer verdict and nonanswer certification runtime/AJV parity", () => {
    const invalidSlot = { ...cleanSlot("native"), verdict: "UNUSABLE" };
    expect(() => REVIEW_SLOT_V1_CODEC.parse(invalidSlot, "slot")).toThrow(/verdict/);
    expect(ajv().compile(REVIEW_SLOT_V1_CODEC.schema)(invalidSlot)).toBe(false);

    const valid = terminalProjection();
    const invalidTerminal = {
      ...valid,
      terminalReview: {
        ...valid.terminalReview,
        kind: "provider-terminal-failure",
        providerAnswerDigest: null,
        reviewResultDigest: null,
        verdict: null,
        failureCode: "provider-rejected",
        readCoverageDigest: null,
        coverageSummaryDigest: null,
        certifying: true,
      },
      evidenceMutationReceipt: null,
    };
    expect(() => PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse(invalidTerminal, "terminal")).toThrow(/certifying/);
    expect(ajv().compile(PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.schema)(invalidTerminal)).toBe(false);
  });

  it("requires certifying provider assurance across terminal, slot and currency projections", () => {
    const advisorySlot = { ...cleanSlot("native"), providerAssurance: "partial-signed-helpers" };
    expect(() => REVIEW_SLOT_V1_CODEC.parse(advisorySlot, "slot")).toThrow(/provider assurance/);
    expect(ajv().compile(REVIEW_SLOT_V1_CODEC.schema)(advisorySlot)).toBe(false);

    const advisoryTerminal = {
      ...terminalProjection(),
      route: { ...terminalProjection().route, providerAssurance: "partial-signed-helpers" },
      terminalReview: { ...terminalProjection().terminalReview, providerAssurance: "partial-signed-helpers" },
    };
    expect(() => PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.parse(advisoryTerminal, "terminal")).toThrow(/provider assurance/);
    expect(ajv().compile(PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.schema)(advisoryTerminal)).toBe(false);

    const advisoryCurrency = {
      ...REVIEW_EVIDENCE_CURRENCY_V1_CODEC.example,
      target: "current",
      source: "current",
      chair: "current",
      profile: "current",
      currentCertificationBasis: activeBasis(),
      providerAssurance: "owner-controlled-install-root",
      certifying: true,
      blockerCodes: [],
    };
    expect(() => REVIEW_EVIDENCE_CURRENCY_V1_CODEC.parse(advisoryCurrency, "currency")).toThrow(/provider assurance/);
    expect(ajv().compile(REVIEW_EVIDENCE_CURRENCY_V1_CODEC.schema)(advisoryCurrency)).toBe(false);
  });

  it("allows observed null normalized effort only with observed inapplicable effort", () => {
    const deployed = {
      ...DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.example,
      resolvedEffort: unavailable(),
      normalizedReasoningEffort: observed(null),
    };
    expect(() => DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.parse(deployed, "deployed")).toThrow(/resolvedEffort|normalizedReasoningEffort/);
    expect(ajv().compile(DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.schema)(deployed)).toBe(false);

    const actual = {
      ...ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC.example,
      endpointProvider: observed("openai"),
      family: observed("openai"),
      model: observed("model_01"),
      resolvedEffort: unavailable(),
      normalizedReasoningEffort: observed(null),
    };
    expect(() => ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC.parse(actual, "actual")).toThrow(/resolvedEffort|normalizedReasoningEffort/);
    expect(ajv().compile(ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC.schema)(actual)).toBe(false);

    const inapplicableDeployed = {
      ...deployed,
      resolvedEffort: observed({ kind: "inapplicable" }),
    };
    expect(DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.parse(inapplicableDeployed, "deployed")).toStrictEqual(inapplicableDeployed);
    expect(ajv().compile(DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.schema)(inapplicableDeployed)).toBe(true);
    const unknownEffort = {
      ...deployed,
      resolvedEffort: unavailable(),
      normalizedReasoningEffort: unavailable(),
    };
    expect(DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.parse(unknownEffort, "deployed")).toStrictEqual(unknownEffort);
  });

  it("requires certifying currency to be wholly current and wrap certifiable safe evidence", () => {
    const currency = {
      ...REVIEW_EVIDENCE_CURRENCY_V1_CODEC.example,
      target: "stale",
      currentCertificationBasis: activeBasis(),
      providerAssurance: "full-vendor-identity",
      certifying: true,
      blockerCodes: [],
    };
    expect(() => REVIEW_EVIDENCE_CURRENCY_V1_CODEC.parse(currency, "currency")).toThrow(/certifying|current/);
    expect(ajv().compile(REVIEW_EVIDENCE_CURRENCY_V1_CODEC.schema)(currency)).toBe(false);

    const read = REVIEW_EVIDENCE_READ_V1_CODEC.example as Readonly<Record<string, unknown>>;
    const readRecord = read.record as Readonly<Record<string, unknown>>;
    const readCurrency = read.currency as Readonly<Record<string, unknown>>;
    const invalidRead = {
      ...read,
      record: {
        ...readRecord,
        terminalKind: "unusable-answer",
        verdict: "UNUSABLE",
        answerSafety: "unusable",
        reviewResultDigest: null,
      },
      currency: {
        ...readCurrency,
        currentCertificationBasis: activeBasis(),
        providerAssurance: "full-vendor-identity",
        certifying: true,
        blockerCodes: [],
      },
    };
    expect(() => REVIEW_EVIDENCE_READ_V1_CODEC.parse(invalidRead, "read")).toThrow(/certifying|safe/);
    expect(ajv().compile(REVIEW_EVIDENCE_READ_V1_CODEC.schema)(invalidRead)).toBe(false);

    const validRead = {
      ...read,
      record: {
        ...readRecord,
        routeObservationDigest: digestA,
        actualRouteIdentityDigest: digestA,
        reviewerFamilyRelation: "same-family-exempt",
        providerAssurance: "full-vendor-identity",
      },
      currency: {
        ...readCurrency,
        target: "current",
        source: "current",
        chair: "current",
        profile: "current",
        currentCertificationBasis: activeBasis(),
        providerAssurance: "full-vendor-identity",
        certifying: true,
        blockerCodes: [],
      },
    };
    expect(REVIEW_EVIDENCE_READ_V1_CODEC.parse(validRead, "read")).toStrictEqual(validRead);
    expect(ajv().compile(REVIEW_EVIDENCE_READ_V1_CODEC.schema)(validRead)).toBe(true);
  });

  it("rejects every intrinsically noncertifying evidence-read arm", () => {
    const read = REVIEW_EVIDENCE_READ_V1_CODEC.example as Readonly<Record<string, unknown>>;
    const readRecord = read.record as Readonly<Record<string, unknown>>;
    const readCurrency = read.currency as Readonly<Record<string, unknown>>;
    const coverageSummary = readRecord.coverageSummary as Readonly<Record<string, unknown>>;
    const validRead = {
      ...read,
      record: {
        ...readRecord,
        slot: "native",
        routeObservationDigest: digestA,
        actualRouteIdentityDigest: digestA,
        reviewerFamilyRelation: "same-family-exempt",
        certificationBasisAtTerminal: activeBasis(),
        providerAssurance: "full-vendor-identity",
      },
      currency: {
        ...readCurrency,
        target: "current",
        source: "current",
        chair: "current",
        profile: "current",
        currentCertificationBasis: activeBasis(),
        providerAssurance: "full-vendor-identity",
        certifying: true,
        blockerCodes: [],
      },
    };
    expect(REVIEW_EVIDENCE_READ_V1_CODEC.parse(validRead, "read")).toStrictEqual(validRead);

    const invalidReads = [
      {
        ...validRead,
        record: {
          ...validRead.record,
          findingWindow: {
            mode: "resolution-only",
            maximumNewFindings: 0,
            maximumNewFindingBytes: 0,
            capacityReservationDigest: digestA,
          },
        },
      },
      {
        ...validRead,
        record: {
          ...validRead.record,
          coverageSummary: { ...coverageSummary, mandatoryComplete: false },
        },
      },
      {
        ...validRead,
        record: { ...validRead.record, slot: "other-primary", reviewerFamilyRelation: "same-family-exempt" },
      },
      {
        ...validRead,
        currency: {
          ...validRead.currency,
          currentCertificationBasis: { ...activeBasis(), terminalSequence: 2 },
        },
      },
      {
        ...validRead,
        record: {
          ...validRead.record,
          certificationBasisAtTerminal: { ...activeBasis(), terminalSequence: 2 },
        },
      },
      {
        ...validRead,
        record: {
          ...validRead.record,
          certificationBasisAtTerminal: {
            kind: "post-cut",
            actionBindingGeneration: 1,
            firstSuccessorBindingGeneration: 2,
            activeBindingGeneration: 2,
            terminalSequence: 1,
            certificationCutSequence: 0,
            certificationCutCustodyRef: {
              schemaVersion: 1,
              runId: "run_01",
              agentId: "agent_01",
              custodyId: "custody_01",
              custodyRevision: 1,
            },
            certificationCutDigest: digestA,
            bindingChainDigest: digestA,
          },
        },
      },
    ];
    const validate = ajv().compile(REVIEW_EVIDENCE_READ_V1_CODEC.schema);
    invalidReads.forEach((candidate) => {
      expect(() => REVIEW_EVIDENCE_READ_V1_CODEC.parse(candidate, "read"))
        .toThrow(/certifying|findingWindow|mandatoryComplete|reviewerFamilyRelation|terminalSequence/);
      expect(validate(candidate)).toBe(false);
    });
  });

  it("closes and deterministically orders repair-currency evidence references", () => {
    const evidenceRef = (id: string) => ({ evidenceRef: id, evidenceRevision: 1, contentDigest: digestA });
    const repositorySource = {
      ...SAFE_FINDING_V1_CODEC.example,
      repairCurrency: {
        kind: "repository-source",
        originRepositorySourceStateDigest: digestA,
        evidenceRefs: [evidenceRef("evidence_01")],
      },
    };
    expect(() => SAFE_FINDING_V1_CODEC.parse(repositorySource, "finding")).toThrow(/repository-source|evidenceRefs/);

    const unsorted = {
      ...SAFE_FINDING_V1_CODEC.example,
      repairCurrency: {
        kind: "registered-evidence",
        originRepositorySourceStateDigest: null,
        evidenceRefs: [evidenceRef("evidence_b"), evidenceRef("evidence_a")],
      },
    };
    expect(() => SAFE_FINDING_V1_CODEC.parse(unsorted, "finding")).toThrow(/evidenceRefs|order/);
    expect(ajv().compile(SAFE_FINDING_V1_CODEC.schema)(unsorted)).toBe(false);
    const duplicateRef = {
      ...unsorted,
      repairCurrency: {
        ...unsorted.repairCurrency,
        evidenceRefs: [
          evidenceRef("evidence_a"),
          { ...evidenceRef("evidence_a"), evidenceRevision: 2, contentDigest: digestB },
        ],
      },
    };
    expect(() => SAFE_FINDING_V1_CODEC.parse(duplicateRef, "finding")).toThrow(/unique|evidenceRef/);
    expect(ajv().compile(SAFE_FINDING_V1_CODEC.schema)(duplicateRef)).toBe(false);
    const sorted = {
      ...unsorted,
      repairCurrency: {
        ...unsorted.repairCurrency,
        evidenceRefs: [evidenceRef("evidence_a"), evidenceRef("evidence_b")],
      },
    };
    expect(SAFE_FINDING_V1_CODEC.parse(sorted, "finding")).toStrictEqual(sorted);
    expect(ajv().compile(SAFE_FINDING_V1_CODEC.schema)(sorted)).toBe(true);
  });
});
