import { describe, expect, it } from "vitest";

import {
  FABRIC_OPERATIONS,
  PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE,
  PROTOCOL_FEATURES,
  REVIEW_EVIDENCE_READ_V1_CODEC,
  assertOperationResultFeatureShape,
  createProtocolInitializeResult,
  type ProtocolFeature,
} from "../src/index.js";

const limits = {
  maximumFrameBytes: 1048576,
  maximumPendingCalls: 32,
  maximumInFlightPerConnection: 16,
  idleTimeoutMs: 300000,
  requestTimeoutMs: 30000,
};

function initializeOptions(requiredFeatures: readonly string[], offeredFeatures: readonly ProtocolFeature[]) {
  return {
    request: {
      protocolVersion: 1 as const,
      client: { name: "provider-assurance-test", version: "1" },
      authentication: {
        scheme: "capability" as const,
        credential: "operator-secret-0001",
        clientNonce: "client_01",
      },
      expectedPrincipalKind: "operator" as const,
      requiredFeatures,
      optionalFeatures: [],
    },
    verifiedCredential: {
      principal: {
        kind: "operator" as const,
        operatorId: "operator_01" as never,
        projectId: "project_01" as never,
        projectAuthorityGeneration: 1,
        principalGeneration: 1,
      },
      grantedOperations: [],
    },
    daemonVersion: "1.0.0",
    daemonInstanceGeneration: 1,
    offeredFeatures,
    limits,
    connectionNonce: "connection_01",
  };
}

describe("provider-assurance.v1 negotiated result shape", () => {
  it("accepts the absent-token legacy shape and requires the field only with the matching token", () => {
    expect(PROTOCOL_FEATURES).toContain(PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE);

    const legacyWire = REVIEW_EVIDENCE_READ_V1_CODEC.example as Record<string, unknown>;
    const legacy = REVIEW_EVIDENCE_READ_V1_CODEC.parse(legacyWire, "read");
    const negotiated = REVIEW_EVIDENCE_READ_V1_CODEC.parse({
      ...legacyWire,
      record: {
        ...(legacyWire.record as Record<string, unknown>),
        providerAssurance: "full-vendor-identity",
      },
      currency: {
        ...(legacyWire.currency as Record<string, unknown>),
        providerAssurance: "full-vendor-identity",
      },
    }, "read");
    const legacyResult = legacy;
    const negotiatedResult = negotiated;

    expect(assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.reviewEvidenceRead,
      [],
      legacyResult as never,
    )).toBe(legacyResult);
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.reviewEvidenceRead,
      [PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE],
      legacyResult as never,
    )).toThrow(expect.objectContaining({
      code: "PROTOCOL_INCOMPATIBLE",
      reason: "missing-negotiated-field",
    }));
    expect(assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.reviewEvidenceRead,
      [PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE],
      negotiatedResult as never,
    )).toBe(negotiatedResult);
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.reviewEvidenceRead,
      [],
      negotiatedResult as never,
    )).toThrow(expect.objectContaining({
      code: "PROTOCOL_INCOMPATIBLE",
      reason: "unnegotiated-field",
    }));
  });

  it("gates a client that requires the result shape on the initialize offer", () => {
    expect(() => createProtocolInitializeResult(initializeOptions(
      [PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE],
      [],
    ))).toThrow(expect.objectContaining({ code: "FEATURE_UNAVAILABLE" }));

    const result = createProtocolInitializeResult(initializeOptions(
      [PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE],
      [PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE],
    ));
    expect(result.features).toContain(PROVIDER_ASSURANCE_RESULT_SHAPE_FEATURE);
  });
});
