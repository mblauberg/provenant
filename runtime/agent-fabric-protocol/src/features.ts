import {
  OPERATION_REGISTRY,
  type FabricOperation,
  type OperationFeature,
} from "./operations.js";

export const FABRIC_PROTOCOL_VERSION = 1 as const;
export const ACTIVITY_NARRATIVE_GROUPING_FEATURE =
  "activity-narrative-grouping.v1" as const;

export const OPERATION_FEATURES = [
  "fabric-core.v1",
  "project-sessions.v1",
  "operator-control.v1",
  "input-attestation.v1",
  "intakes.v1",
  "scoped-gates.v1",
  "scoped-gate-read.v1",
  "resource-reservations.v1",
  "request-results.v1",
  "chair-takeover.v1",
  "operator-projection.v1",
  "operator-projection.v2",
  "operator-actions.v1",
  "launch-custody.v1",
  "launch-attestation.v1",
  "message-body-read.v1",
  "operator-repository-read.v1",
  "artifact-registry.v1",
  "artifact-content-read.v1",
  "lifecycle-control.v1",
  "workstreams.v1",
  "run-plan-declaration.v1",
  "chair-live-handoff.v1",
  "provider-review-evidence.v1",
  "provider-context-pressure.v1",
  "herdr-control.v1",
  "topology-wave.v1",
] as const satisfies readonly OperationFeature[];

export const RESULT_SHAPE_FEATURES = [
  "native-notification-projection.v1",
  "gate-system-supersession.v1",
  "run-session-projection.v1",
  "declared-run-progress.v2",
  "run-identity-projection.v2",
  "agent-topology-projection.v1",
  "work-facts-projection.v1",
  ACTIVITY_NARRATIVE_GROUPING_FEATURE,
  "mcp-bootstrap-credentials.v2",
] as const;

export const GATE_SYSTEM_SUPERSESSION_FEATURE = "gate-system-supersession.v1" as const;

export const AGENT_RESULT_SHAPE_FEATURES = [
  GATE_SYSTEM_SUPERSESSION_FEATURE,
] as const;

export const PROTOCOL_FEATURES = [
  ...OPERATION_FEATURES,
  ...RESULT_SHAPE_FEATURES,
] as const;

export const PROTOCOL_FEATURE_NAME_PATTERN =
  "^[a-z][a-z0-9]*(?:-[a-z0-9]+)*(?:\\.[a-z][a-z0-9]*(?:-[a-z0-9]+)*)*\\.v[1-9][0-9]*$";
export const MAXIMUM_PROTOCOL_FEATURE_NAMES = 64;
export const MAXIMUM_PROTOCOL_FEATURE_NAME_BYTES = 64;

export type ProtocolFeature = (typeof PROTOCOL_FEATURES)[number];

function buildFeatureOperations(): Readonly<Record<ProtocolFeature, readonly FabricOperation[]>> {
  const grouped = Object.fromEntries(PROTOCOL_FEATURES.map((feature) => [feature, [] as FabricOperation[]])) as
    Record<ProtocolFeature, FabricOperation[]>;
  for (const [operation, definition] of Object.entries(OPERATION_REGISTRY)) {
    grouped[definition.feature].push(operation as FabricOperation);
  }
  for (const operations of Object.values(grouped)) Object.freeze(operations);
  return Object.freeze(grouped);
}

export const FEATURE_OPERATIONS = buildFeatureOperations();

export function operationsForFeatures(features: readonly string[]): ReadonlySet<FabricOperation> {
  const operations: FabricOperation[] = [];
  for (const feature of features) {
    const owned = FEATURE_OPERATIONS[feature as ProtocolFeature];
    if (owned !== undefined) operations.push(...owned);
  }
  return new Set(operations);
}

export type ProtocolNegotiationRequest = {
  protocolVersion: number;
  requiredFeatures: readonly string[];
  optionalFeatures: readonly string[];
};

export type ProtocolOffer = {
  protocolVersion: number;
  features: readonly ProtocolFeature[];
};

export type ProtocolNegotiationResult =
  | {
      ok: true;
      protocolVersion: typeof FABRIC_PROTOCOL_VERSION;
      features: ProtocolFeature[];
    }
  | {
      ok: false;
      reason: "protocol-version-unsupported";
      requestedVersion: number;
      offeredVersion: number;
    }
  | {
      ok: false;
      reason: "required-features-unavailable";
      missingFeatures: string[];
    };

export function negotiateProtocol(
  request: ProtocolNegotiationRequest,
  offer: ProtocolOffer,
): ProtocolNegotiationResult {
  if (
    request.protocolVersion !== FABRIC_PROTOCOL_VERSION ||
    offer.protocolVersion !== FABRIC_PROTOCOL_VERSION
  ) {
    return {
      ok: false,
      reason: "protocol-version-unsupported",
      requestedVersion: request.protocolVersion,
      offeredVersion: offer.protocolVersion,
    };
  }

  const available = new Set<string>(offer.features);
  const missingFeatures = request.requiredFeatures.filter((feature) => !available.has(feature));
  if (missingFeatures.length > 0) {
    return { ok: false, reason: "required-features-unavailable", missingFeatures };
  }

  const requested = new Set([...request.requiredFeatures, ...request.optionalFeatures]);
  return {
    ok: true,
    protocolVersion: FABRIC_PROTOCOL_VERSION,
    features: offer.features.filter((feature) => requested.has(feature)),
  };
}
