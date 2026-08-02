import {
  MAXIMUM_PROTOCOL_FEATURE_NAME_BYTES,
  MAXIMUM_PROTOCOL_FEATURE_NAMES,
  PROTOCOL_FEATURE_NAME_PATTERN,
  PROTOCOL_FEATURES,
} from "./features.js";
import {
  BOUNDED_JSON_VALUE_SCHEMA,
  JSON_VALUE_NODE_SCHEMA,
  protocolClientField,
  protocolFailureMessage,
  secret,
} from "./codec.js";
import { OPERATION_CODECS } from "./operation-codecs/registry.js";
import {
  LAUNCH_ADAPTER_OUTCOME_V1_CODEC,
  LAUNCH_PROVIDER_ACTION_JOURNAL_REF_V1_CODEC,
  LAUNCH_PACKET_V1_CODEC,
  LAUNCH_RESOURCE_PLAN_V1_CODEC,
  PROJECT_SESSION_LAUNCH_CURRENT_STATE_CODEC,
  PROJECT_SESSION_LAUNCH_INTENT_CODEC,
  PROVIDER_ACTION_REF_V1_CODEC,
} from "./launch.js";
import {
  ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC,
  ADAPTER_CAPABILITY_SNAPSHOT_V1_CODEC,
  ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC,
  ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC,
  CAPABILITY_SNAPSHOT_REF_V1_CODEC,
  CAPABILITY_SNAPSHOT_SUMMARY_V1_CODEC,
  DEPLOYED_ROUTE_ADMISSION_V1_CODEC,
  DEPLOYED_ROUTE_DISPATCH_V1_CODEC,
  DEPLOYED_ROUTE_OBSERVATION_V1_CODEC,
  DISCOVERY_SURFACE_MANIFEST_V1_CODEC,
  DISCOVERY_SURFACE_REF_V1_CODEC,
  FABRIC_OPERATIONAL_SPAN_V1_CODEC,
  PROVIDER_CONTEXT_PRESSURE_READ_REQUEST_V1_CODEC,
  PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC,
  PROVIDER_CONTEXT_PRESSURE_V1_CODEC,
  PROVIDER_ROUTE_V1_CODEC,
} from "./route-lineage.js";
import {
  EVALUATED_ROUTE_IDENTITY_V1_CODEC,
  ROUTE_EVALUATION_EVIDENCE_V1_CODEC,
  TOPOLOGY_WAVE_APPEND_RECEIPT_V1_CODEC,
  TOPOLOGY_WAVE_APPEND_REQUEST_V1_CODEC,
  TOPOLOGY_WAVE_CURRENT_READ_REQUEST_V1_CODEC,
  TOPOLOGY_WAVE_CURRENT_READ_V1_CODEC,
  TOPOLOGY_WAVE_LIST_REQUEST_V1_CODEC,
  TOPOLOGY_WAVE_LIST_V1_CODEC,
  TOPOLOGY_WAVE_PLAN_CURRENT_V1_CODEC,
  TOPOLOGY_WAVE_PLAN_INPUT_V1_CODEC,
  TOPOLOGY_WAVE_PLAN_REF_V1_CODEC,
  TOPOLOGY_WAVE_PLAN_V1_CODEC,
} from "./topology-evaluation.js";
import {
  REVIEW_BUNDLE_PORTAL_ERROR_V1_CODEC,
  REVIEW_BUNDLE_READ_ARGS_V1_CODEC,
  REVIEW_BUNDLE_READ_RESULT_V1_CODEC,
  REVIEW_BUNDLE_SEARCH_ARGS_V1_CODEC,
  REVIEW_BUNDLE_SEARCH_RESULT_V1_CODEC,
  REVIEW_PORTAL_REQUEST_V1_CODEC,
  REVIEW_PORTAL_RESPONSE_V1_CODEC,
} from "./review-portal.js";
import {
  REVIEW_RESULT_V1_CODEC,
  REVIEW_TARGET_PREPARATION_READ_V1_CODEC,
  TERMINAL_RESULT_IDENTITY_V1_CODEC,
} from "./provider-review-core.js";
import {
  COVERAGE_SUMMARY_V1_CODEC,
  REPAIR_CURRENCY_V1_CODEC,
  REVIEW_CERTIFICATION_BASIS_V1_CODEC,
  REVIEW_EVIDENCE_CURRENCY_V1_CODEC,
  REVIEW_EVIDENCE_RECORD_V1_CODEC,
  REVIEW_EVIDENCE_READ_V1_CODEC,
  REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC,
  PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC,
  PROVIDER_ROUTE_PROJECTION_V1_CODEC,
} from "./provider-review-evidence.js";
import { REVIEW_COMPLETION_V1_CODEC, REVIEW_SLOT_V1_CODEC } from "./provider-review-completion.js";
import { PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC } from "./provider-review-recovery.js";
import { RESOLVED_REVIEW_PROFILE_V1_CODEC } from "./review-profile.js";
import {
  AGENT_LIFECYCLE_RECOVERY_INTENT_V1_CODEC,
  LIFECYCLE_ACCEPTED_SUSPENDED_V1_CODEC,
  LIFECYCLE_CURRENT_STATE_V1_CODEC,
  LIFECYCLE_CUSTODY_ROW_V1_CODEC,
  LIFECYCLE_GENERATION_LOSS_ROW_V1_CODEC,
  LIFECYCLE_RECOVERY_SOURCE_V1_CODEC,
  LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATE_REQUEST_V1_CODEC,
  LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATION_V1_CODEC,
} from "./lifecycle.js";
import { OPERATION_REGISTRY } from "./operations.js";
import { parseJsonValue, type JsonValue } from "./primitives.js";
import { PROTOCOL_ERROR_CODES, PROTOCOL_LIMITS, type ProtocolOperation } from "./rpc-contract.js";
import { AUTHORITY_ENVELOPE_V2_CODEC } from "./authority.js";

type JsonSchema = Readonly<Record<string, JsonValue>>;

const idSchema = { type: "string", pattern: "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$" } as const;
const timestampSchema = { type: "string", format: "date-time" } as const;
const digestSchema = { type: "string", pattern: "^sha256:[a-f0-9]{64}$" } as const;
const operations = Object.keys(OPERATION_REGISTRY) as ProtocolOperation[];
const activeOperations = operations;
const boundedJsonDefinitions = {
  boundedJsonValue: BOUNDED_JSON_VALUE_SCHEMA,
  jsonValueNode: JSON_VALUE_NODE_SCHEMA,
} as const;

function standaloneLaunchSchema(schema: JsonSchema): JsonSchema {
  return { ...schema, "$defs": boundedJsonDefinitions };
}

export const LAUNCH_CONTRACT_SCHEMAS = Object.freeze({
  projectSessionLaunchIntent: standaloneLaunchSchema(PROJECT_SESSION_LAUNCH_INTENT_CODEC.schema),
  launchPacketV1: standaloneLaunchSchema(LAUNCH_PACKET_V1_CODEC.schema),
  launchResourcePlanV1: standaloneLaunchSchema(LAUNCH_RESOURCE_PLAN_V1_CODEC.schema),
  projectSessionLaunchCurrentState: standaloneLaunchSchema(PROJECT_SESSION_LAUNCH_CURRENT_STATE_CODEC.schema),
  launchAdapterOutcomeV1: standaloneLaunchSchema(LAUNCH_ADAPTER_OUTCOME_V1_CODEC.schema),
  providerActionRefV1: standaloneLaunchSchema(PROVIDER_ACTION_REF_V1_CODEC.schema),
  launchProviderActionJournalRefV1: standaloneLaunchSchema(LAUNCH_PROVIDER_ACTION_JOURNAL_REF_V1_CODEC.schema),
});

export const CORE_CONTRACT_SCHEMAS = Object.freeze({
  "authority-envelope.v2": AUTHORITY_ENVELOPE_V2_CODEC.schema,
});

export const FABRIC_CONTRACT_SCHEMAS = Object.freeze({
  "adapter-capability-snapshot.v1": ADAPTER_CAPABILITY_SNAPSHOT_V1_CODEC.schema,
  "capability-snapshot-ref.v1": CAPABILITY_SNAPSHOT_REF_V1_CODEC.schema,
  "capability-snapshot-summary.v1": CAPABILITY_SNAPSHOT_SUMMARY_V1_CODEC.schema,
  "discovery-surface-manifest.v1": DISCOVERY_SURFACE_MANIFEST_V1_CODEC.schema,
  "discovery-surface-ref.v1": DISCOVERY_SURFACE_REF_V1_CODEC.schema,
  "deployed-route-admission.v1": DEPLOYED_ROUTE_ADMISSION_V1_CODEC.schema,
  "deployed-route-dispatch.v1": DEPLOYED_ROUTE_DISPATCH_V1_CODEC.schema,
  "deployed-route-observation.v1": DEPLOYED_ROUTE_OBSERVATION_V1_CODEC.schema,
  "actual-review-route-identity.v1": ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC.schema,
  "adapter-effective-configuration.v1": ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC.schema,
  "adapter-effective-configuration-ref.v1": ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC.schema,
  "provider-context-pressure.v1": PROVIDER_CONTEXT_PRESSURE_V1_CODEC.schema,
  "provider-context-pressure-read-request.v1": PROVIDER_CONTEXT_PRESSURE_READ_REQUEST_V1_CODEC.schema,
  "provider-context-pressure-read.v1": PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC.schema,
  "provider-route.v1": PROVIDER_ROUTE_V1_CODEC.schema,
  "topology-wave-plan-ref.v1": TOPOLOGY_WAVE_PLAN_REF_V1_CODEC.schema,
  "topology-wave-plan.v1": TOPOLOGY_WAVE_PLAN_V1_CODEC.schema,
  "topology-wave-plan-current.v1": TOPOLOGY_WAVE_PLAN_CURRENT_V1_CODEC.schema,
  "topology-wave-plan-input.v1": TOPOLOGY_WAVE_PLAN_INPUT_V1_CODEC.schema,
  "topology-wave-append-request.v1": TOPOLOGY_WAVE_APPEND_REQUEST_V1_CODEC.schema,
  "topology-wave-append-receipt.v1": TOPOLOGY_WAVE_APPEND_RECEIPT_V1_CODEC.schema,
  "topology-wave-current-read-request.v1": TOPOLOGY_WAVE_CURRENT_READ_REQUEST_V1_CODEC.schema,
  "topology-wave-current-read.v1": TOPOLOGY_WAVE_CURRENT_READ_V1_CODEC.schema,
  "topology-wave-list-request.v1": TOPOLOGY_WAVE_LIST_REQUEST_V1_CODEC.schema,
  "topology-wave-list.v1": TOPOLOGY_WAVE_LIST_V1_CODEC.schema,
  "fabric-operational-span.v1": FABRIC_OPERATIONAL_SPAN_V1_CODEC.schema,
  "evaluated-route-identity.v1": EVALUATED_ROUTE_IDENTITY_V1_CODEC.schema,
  "route-evaluation-evidence.v1": ROUTE_EVALUATION_EVIDENCE_V1_CODEC.schema,
  "terminal-result-identity.v1": TERMINAL_RESULT_IDENTITY_V1_CODEC.schema,
  "review-result.v1": REVIEW_RESULT_V1_CODEC.schema,
  "review-certification-basis.v1": REVIEW_CERTIFICATION_BASIS_V1_CODEC.schema,
  "coverage-summary.v1": COVERAGE_SUMMARY_V1_CODEC.schema,
  "review-evidence-record.v1": REVIEW_EVIDENCE_RECORD_V1_CODEC.schema,
  "provider-route-projection.v1": PROVIDER_ROUTE_PROJECTION_V1_CODEC.schema,
  "provider-action-terminal-projection.v1": PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC.schema,
  "review-evidence-mutation-receipt.v1": REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC.schema,
  "review-target-preparation-read.v1": REVIEW_TARGET_PREPARATION_READ_V1_CODEC.schema,
  "review-evidence-read.v1": REVIEW_EVIDENCE_READ_V1_CODEC.schema,
  "review-completion.v1": REVIEW_COMPLETION_V1_CODEC.schema,
  "review-bundle-read-args.v1": REVIEW_BUNDLE_READ_ARGS_V1_CODEC.schema,
  "review-bundle-read-result.v1": REVIEW_BUNDLE_READ_RESULT_V1_CODEC.schema,
  "review-bundle-search-args.v1": REVIEW_BUNDLE_SEARCH_ARGS_V1_CODEC.schema,
  "review-bundle-search-result.v1": REVIEW_BUNDLE_SEARCH_RESULT_V1_CODEC.schema,
  "review-bundle-portal-error.v1": REVIEW_BUNDLE_PORTAL_ERROR_V1_CODEC.schema,
  "review-bundle-portal-request.v1": REVIEW_PORTAL_REQUEST_V1_CODEC.schema,
  "review-bundle-portal-response.v1": REVIEW_PORTAL_RESPONSE_V1_CODEC.schema,
  "lifecycle-custody-row.v1": LIFECYCLE_CUSTODY_ROW_V1_CODEC.schema,
  "lifecycle-generation-loss-row.v1": LIFECYCLE_GENERATION_LOSS_ROW_V1_CODEC.schema,
  "lifecycle-recovery-source.v1": LIFECYCLE_RECOVERY_SOURCE_V1_CODEC.schema,
  "lifecycle-recovery-checkpoint-validate-request.v1": LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATE_REQUEST_V1_CODEC.schema,
  "lifecycle-recovery-checkpoint-validation.v1": LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATION_V1_CODEC.schema,
  "lifecycle-accepted-suspended.v1": LIFECYCLE_ACCEPTED_SUSPENDED_V1_CODEC.schema,
  "lifecycle-current-state.v1": LIFECYCLE_CURRENT_STATE_V1_CODEC.schema,
  "agent-lifecycle-recovery-intent.v1": AGENT_LIFECYCLE_RECOVERY_INTENT_V1_CODEC.schema,
  "certifying-review-four-slot-v1": RESOLVED_REVIEW_PROFILE_V1_CODEC.schema,
});

const principalSchemas = {
  operator: {
    type: "object",
    additionalProperties: false,
    required: ["kind", "operatorId", "projectId", "projectAuthorityGeneration", "principalGeneration"],
    properties: {
      kind: { const: "operator" },
      operatorId: idSchema,
      projectId: idSchema,
      projectAuthorityGeneration: { type: "integer", minimum: 1 },
      principalGeneration: { type: "integer", minimum: 1 },
    },
  },
  agent: {
    type: "object",
    additionalProperties: false,
    required: ["kind", "agentId", "projectSessionId", "runId", "principalGeneration"],
    properties: {
      kind: { const: "agent" },
      agentId: idSchema,
      projectSessionId: idSchema,
      runId: idSchema,
      principalGeneration: { type: "integer", minimum: 1 },
    },
  },
  integration: {
    type: "object",
    additionalProperties: false,
    required: ["kind", "integrationId", "projectId", "projectSessionId", "runId", "principalGeneration", "providerId", "providerSessionRef"],
    properties: {
      kind: { const: "integration" },
      integrationId: idSchema,
      projectId: idSchema,
      projectSessionId: idSchema,
      runId: idSchema,
      principalGeneration: { type: "integer", minimum: 1 },
      providerId: idSchema,
      providerSessionRef: idSchema,
    },
  },
} as const;

const protocolFailureSchema = {
  type: "object",
  additionalProperties: false,
  required: ["code", "message", "retryable"],
  properties: {
    code: { type: "string", enum: PROTOCOL_ERROR_CODES },
    message: protocolFailureMessage.schema,
    retryable: { type: "boolean" },
    details: { "$ref": "#/$defs/boundedJsonValue" },
  },
} as const;

const limitsSchema = {
  type: "object",
  additionalProperties: false,
  required: Object.keys(PROTOCOL_LIMITS),
  properties: {
    maximumFrameBytes: { type: "integer", minimum: 1, maximum: PROTOCOL_LIMITS.maximumFrameBytes },
    maximumPendingCalls: { type: "integer", minimum: 1, maximum: PROTOCOL_LIMITS.maximumPendingCalls },
    maximumInFlightPerConnection: { type: "integer", minimum: 1, maximum: PROTOCOL_LIMITS.maximumInFlightPerConnection },
    idleTimeoutMs: { type: "integer", minimum: 1, maximum: PROTOCOL_LIMITS.idleTimeoutMs },
    requestTimeoutMs: { type: "integer", minimum: 1, maximum: PROTOCOL_LIMITS.requestTimeoutMs },
  },
} as const;

const initializeInputSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "protocolVersion",
    "client",
    "authentication",
    "expectedPrincipalKind",
    "requiredFeatures",
    "optionalFeatures",
  ],
  properties: {
    protocolVersion: { const: 1 },
    client: {
      type: "object",
      additionalProperties: false,
      required: ["name", "version"],
      properties: {
        name: protocolClientField.schema,
        version: protocolClientField.schema,
      },
    },
    authentication: {
      type: "object",
      additionalProperties: false,
      required: ["scheme", "credential", "clientNonce"],
      properties: {
        scheme: { const: "capability" },
        credential: secret.schema,
        clientNonce: idSchema,
      },
    },
    expectedPrincipalKind: { enum: ["operator", "agent", "integration"] },
    requiredFeatures: {
      type: "array",
      maxItems: MAXIMUM_PROTOCOL_FEATURE_NAMES,
      uniqueItems: true,
      items: {
        type: "string",
        pattern: PROTOCOL_FEATURE_NAME_PATTERN,
        maxLength: MAXIMUM_PROTOCOL_FEATURE_NAME_BYTES,
        "x-maxUtf8Bytes": MAXIMUM_PROTOCOL_FEATURE_NAME_BYTES,
      },
    },
    optionalFeatures: {
      type: "array",
      maxItems: MAXIMUM_PROTOCOL_FEATURE_NAMES,
      uniqueItems: true,
      items: {
        type: "string",
        pattern: PROTOCOL_FEATURE_NAME_PATTERN,
        maxLength: MAXIMUM_PROTOCOL_FEATURE_NAME_BYTES,
        "x-maxUtf8Bytes": MAXIMUM_PROTOCOL_FEATURE_NAME_BYTES,
      },
    },
  },
  "x-combinedMaxFeatureNames": MAXIMUM_PROTOCOL_FEATURE_NAMES,
  "x-crossArrayUnique": ["requiredFeatures", "optionalFeatures"],
} as const;

const initializeResultSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "protocolVersion",
    "daemonVersion",
    "daemonInstanceGeneration",
    "principal",
    "clientNonce",
    "connectionNonce",
    "features",
    "allowedOperations",
    "limits",
  ],
  properties: {
    protocolVersion: { const: 1 },
    daemonVersion: protocolClientField.schema,
    daemonInstanceGeneration: { type: "integer", minimum: 1 },
    principal: { oneOf: Object.values(principalSchemas) },
    clientNonce: idSchema,
    connectionNonce: idSchema,
    features: { type: "array", maxItems: PROTOCOL_FEATURES.length, uniqueItems: true, items: { enum: PROTOCOL_FEATURES } },
    allowedOperations: { type: "array", maxItems: activeOperations.length, uniqueItems: true, items: { enum: activeOperations } },
    limits: limitsSchema,
  },
} as const;

function requestVariant(operation: ProtocolOperation): JsonSchema {
  return {
    type: "object",
    additionalProperties: false,
    required: ["id", "operation", "input"],
    properties: {
      id: idSchema,
      operation: { const: operation },
      input: OPERATION_CODECS[operation].input.schema,
    },
  };
}

function successVariant(operation: ProtocolOperation): JsonSchema {
  return {
    type: "object",
    additionalProperties: false,
    required: ["id", "operation", "ok", "result"],
    properties: {
      id: idSchema,
      operation: { const: operation },
      ok: { const: true },
      result: OPERATION_CODECS[operation].result.schema,
    },
  };
}

function failureVariant(operation: ProtocolOperation | "initialize"): JsonSchema {
  return {
    type: "object",
    additionalProperties: false,
    required: ["id", "operation", "ok", "error"],
    properties: {
      id: idSchema,
      operation: { const: operation },
      ok: { const: false },
      error: protocolFailureSchema,
    },
  };
}

const initializeRequestEnvelope = {
  type: "object",
  additionalProperties: false,
  required: ["id", "operation", "input"],
  properties: { id: idSchema, operation: { const: "initialize" }, input: initializeInputSchema },
} as const;
const initializeSuccessEnvelope = {
  type: "object",
  additionalProperties: false,
  required: ["id", "operation", "ok", "result"],
  properties: { id: idSchema, operation: { const: "initialize" }, ok: { const: true }, result: initializeResultSchema },
} as const;

const capabilityBaseProperties = {
  capabilityId: idSchema,
  operatorId: idSchema,
  projectId: idSchema,
  projectAuthorityGeneration: { type: "integer", minimum: 1 },
  principalGeneration: { type: "integer", minimum: 1 },
  issuedAt: timestampSchema,
  expiresAt: timestampSchema,
  status: { const: "active" },
};
function capabilityVariant(kind: "project-launch" | "session" | "takeover", actions: readonly string[]): JsonSchema {
  const sessionFields = kind === "project-launch" ? {} : {
    projectSessionId: idSchema,
    sessionGeneration: { type: "integer", minimum: 1 },
  };
  const takeoverFields = kind === "takeover" ? {
    takeoverBinding: {
      type: "object",
      additionalProperties: false,
      required: ["handoffDigest", "oldChairGeneration", "expectedRunId", "expectedRunRevision", "expectedSessionRevision", "targetRevision"],
      properties: {
        handoffDigest: digestSchema,
        oldChairGeneration: { type: "integer", minimum: 1 },
        expectedRunId: idSchema,
        expectedRunRevision: { type: "integer", minimum: 0 },
        expectedSessionRevision: { type: "integer", minimum: 0 },
        targetRevision: { type: "integer", minimum: 1 },
      },
    },
  } : {};
  return {
    type: "object",
    additionalProperties: false,
    required: [
      "capabilityId", "operatorId", "projectId", "projectAuthorityGeneration", "principalGeneration", "issuedAt", "expiresAt", "status", "kind", "actions",
      ...(kind === "project-launch" ? [] : ["projectSessionId", "sessionGeneration"]),
      ...(kind === "takeover" ? ["takeoverBinding"] : []),
    ],
    properties: {
      ...capabilityBaseProperties,
      kind: { const: kind },
      actions: {
        type: "array",
        minItems: 1,
        maxItems: actions.length,
        uniqueItems: true,
        items: { enum: actions },
        ...(kind === "takeover" ? { contains: { const: "takeover" } } : {}),
      },
      ...sessionFields,
      ...takeoverFields,
    },
  };
}

export const PROTOCOL_SCHEMA = {
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://local.invalid/agent-fabric-protocol/v1/protocol.schema.json",
  title: "Agent Fabric public protocol v1",
  oneOf: [
    { "$ref": "#/$defs/initializeRequest" },
    { "$ref": "#/$defs/initializeSuccess" },
    { "$ref": "#/$defs/initializeFailure" },
    { "$ref": "#/$defs/rpcRequest" },
    { "$ref": "#/$defs/rpcResponse" },
  ],
  "$defs": {
    ...boundedJsonDefinitions,
    projectSessionLaunchIntent: PROJECT_SESSION_LAUNCH_INTENT_CODEC.schema,
    launchPacketV1: LAUNCH_PACKET_V1_CODEC.schema,
    launchResourcePlanV1: LAUNCH_RESOURCE_PLAN_V1_CODEC.schema,
    projectSessionLaunchCurrentState: PROJECT_SESSION_LAUNCH_CURRENT_STATE_CODEC.schema,
    launchAdapterOutcomeV1: LAUNCH_ADAPTER_OUTCOME_V1_CODEC.schema,
    providerActionRefV1: PROVIDER_ACTION_REF_V1_CODEC.schema,
    launchProviderActionJournalRefV1: LAUNCH_PROVIDER_ACTION_JOURNAL_REF_V1_CODEC.schema,
    ...FABRIC_CONTRACT_SCHEMAS,
    fabricOperation: { type: "string", enum: operations },
    activeFabricOperation: { type: "string", enum: activeOperations },
    operatorPrincipal: principalSchemas.operator,
    agentPrincipal: principalSchemas.agent,
    integrationPrincipal: principalSchemas.integration,
    principal: { oneOf: Object.values(principalSchemas) },
    operatorCapability: {
      oneOf: [
        capabilityVariant("project-launch", ["read", "launch"]),
        capabilityVariant("session", ["read", "decide", "steer", "pause", "resume", "cancel", "drain", "stop", "launch", "git", "agent-lifecycle-recovery-issue", "external-effect"]),
        capabilityVariant("takeover", ["read", "decide", "steer", "pause", "resume", "cancel", "drain", "stop", "launch", "takeover", "git", "agent-lifecycle-recovery-issue", "external-effect"]),
      ],
    },
    protocolFailure: protocolFailureSchema,
    protocolLimits: limitsSchema,
    initializeInput: initializeInputSchema,
    initializeResult: initializeResultSchema,
    initializeRequest: initializeRequestEnvelope,
    initializeSuccess: initializeSuccessEnvelope,
    initializeFailure: failureVariant("initialize"),
    rpcRequest: { oneOf: operations.map(requestVariant), "$defs": boundedJsonDefinitions },
    rpcResponse: { oneOf: operations.flatMap((operation) => [
      successVariant(operation),
      failureVariant(operation),
    ]), "$defs": boundedJsonDefinitions },
  },
} as const satisfies Readonly<Record<string, JsonValue>>;

export function protocolRequestSchemaFor(operation: ProtocolOperation): JsonSchema {
  return { ...requestVariant(operation), "$defs": boundedJsonDefinitions };
}

export function protocolResponseSchemasFor(operation: ProtocolOperation): readonly JsonSchema[] {
  const variants = [successVariant(operation), failureVariant(operation)];
  return variants.map((variant) => ({ ...variant, "$defs": boundedJsonDefinitions }));
}

export function initializeRequestSchema(): JsonSchema {
  return { ...initializeRequestEnvelope, "$defs": boundedJsonDefinitions };
}

export function initializeResponseSchemas(): readonly JsonSchema[] {
  return [initializeSuccessEnvelope, failureVariant("initialize")]
    .map((variant) => ({ ...variant, "$defs": boundedJsonDefinitions }));
}

type SchemaKeywordDefinition = {
  keyword: string;
  schemaType: "array" | "boolean" | "number";
  type?: "string" | "object" | "array";
  errors: false;
  validate(schema: unknown, data: unknown): boolean;
};

export type ProtocolSchemaKeywordTarget = {
  addKeyword(definition: SchemaKeywordDefinition): unknown;
};

export function addProtocolSchemaKeywords(target: ProtocolSchemaKeywordTarget): void {
  target.addKeyword({
    keyword: "x-combinedMaxFeatureNames",
    schemaType: "number",
    type: "object",
    errors: false,
    validate: (maximum, data) => {
      if (typeof maximum !== "number" || data === null || typeof data !== "object" || Array.isArray(data)) return false;
      const record = data as Readonly<Record<string, unknown>>;
      return Array.isArray(record.requiredFeatures) && Array.isArray(record.optionalFeatures) &&
        record.requiredFeatures.length + record.optionalFeatures.length <= maximum;
    },
  });
  target.addKeyword({
    keyword: "x-crossArrayUnique",
    schemaType: "array",
    type: "object",
    errors: false,
    validate: (fields, data) => {
      if (!Array.isArray(fields) || !fields.every((field) => typeof field === "string") ||
        data === null || typeof data !== "object" || Array.isArray(data)) return false;
      const record = data as Readonly<Record<string, unknown>>;
      const seen = new Set<unknown>();
      for (const field of fields) {
        const values = record[field];
        if (!Array.isArray(values)) return false;
        for (const value of values) {
          if (seen.has(value)) return false;
          seen.add(value);
        }
      }
      return true;
    },
  });
  target.addKeyword({
    keyword: "x-minUtf8Bytes",
    schemaType: "number",
    type: "string",
    errors: false,
    validate: (minimum, data) => (
      typeof minimum === "number" && typeof data === "string" && Buffer.byteLength(data, "utf8") >= minimum
    ),
  });
  const correlatedCodecKeyword = (
    keyword: string,
    codec: { parse(value: unknown, path: string): unknown },
  ): void => {
    target.addKeyword({
      keyword,
      schemaType: "boolean",
      errors: false,
      validate: (enabled, data) => {
        if (enabled !== true) return true;
        try {
          codec.parse(data, keyword);
          return true;
        } catch {
          return false;
        }
      },
    });
  };
  correlatedCodecKeyword("x-reviewPreparationCorrelated", REVIEW_TARGET_PREPARATION_READ_V1_CODEC);
  correlatedCodecKeyword("x-routeRecoveryCorrelated", PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC);
  correlatedCodecKeyword("x-providerActionTerminalCorrelated", PROVIDER_ACTION_TERMINAL_PROJECTION_V1_CODEC);
  correlatedCodecKeyword("x-reviewEvidenceMutationReceiptCorrelated", REVIEW_EVIDENCE_MUTATION_RECEIPT_V1_CODEC);
  correlatedCodecKeyword("x-reviewEvidenceCorrelated", REVIEW_EVIDENCE_RECORD_V1_CODEC);
  correlatedCodecKeyword("x-reviewEvidenceCurrencyCorrelated", REVIEW_EVIDENCE_CURRENCY_V1_CODEC);
  correlatedCodecKeyword("x-reviewEvidenceReadCorrelated", REVIEW_EVIDENCE_READ_V1_CODEC);
  correlatedCodecKeyword("x-reviewSlotCorrelated", REVIEW_SLOT_V1_CODEC);
  correlatedCodecKeyword("x-reviewCompletionCorrelated", REVIEW_COMPLETION_V1_CODEC);
  correlatedCodecKeyword("x-repairCurrencyOrdered", REPAIR_CURRENCY_V1_CODEC);
  correlatedCodecKeyword("x-deployedRouteObservationCorrelated", DEPLOYED_ROUTE_OBSERVATION_V1_CODEC);
  correlatedCodecKeyword("x-actualReviewRouteCorrelated", ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC);
  correlatedCodecKeyword("x-fourSlotProfileMatrix", RESOLVED_REVIEW_PROFILE_V1_CODEC);
  correlatedCodecKeyword("x-topologyAppendReceiptCorrelated", TOPOLOGY_WAVE_APPEND_RECEIPT_V1_CODEC);
  correlatedCodecKeyword("x-topologyCurrentReadCorrelated", TOPOLOGY_WAVE_CURRENT_READ_V1_CODEC);
  correlatedCodecKeyword("x-providerContextPressureReadCorrelated", PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC);
  correlatedCodecKeyword("x-reviewPortalResponseBound", REVIEW_PORTAL_RESPONSE_V1_CODEC);
  correlatedCodecKeyword("x-reviewCertificationBasisCorrelated", REVIEW_CERTIFICATION_BASIS_V1_CODEC);
  correlatedCodecKeyword("x-coverageGroupsOrdered", COVERAGE_SUMMARY_V1_CODEC);
  correlatedCodecKeyword("x-adapterCapabilitySnapshotCorrelated", ADAPTER_CAPABILITY_SNAPSHOT_V1_CODEC);
  correlatedCodecKeyword("x-deployedRouteAdmissionCorrelated", DEPLOYED_ROUTE_ADMISSION_V1_CODEC);
  correlatedCodecKeyword("x-providerRouteCorrelated", PROVIDER_ROUTE_V1_CODEC);
  correlatedCodecKeyword("x-providerContextPressureCorrelated", PROVIDER_CONTEXT_PRESSURE_V1_CODEC);
  correlatedCodecKeyword("x-routeEvaluationEvidenceCorrelated", ROUTE_EVALUATION_EVIDENCE_V1_CODEC);
  correlatedCodecKeyword("x-lifecycleCustodyCorrelated", LIFECYCLE_CUSTODY_ROW_V1_CODEC);
  correlatedCodecKeyword("x-lifecycleGenerationLossCorrelated", LIFECYCLE_GENERATION_LOSS_ROW_V1_CODEC);
  correlatedCodecKeyword("x-lifecycleRecoverySourceCorrelated", LIFECYCLE_RECOVERY_SOURCE_V1_CODEC);
  correlatedCodecKeyword("x-lifecycleAcceptedSuspendedCorrelated", LIFECYCLE_ACCEPTED_SUSPENDED_V1_CODEC);
  correlatedCodecKeyword("x-lifecycleCurrentStateCorrelated", LIFECYCLE_CURRENT_STATE_V1_CODEC);
  correlatedCodecKeyword("x-lifecycleCheckpointValidateCorrelated", LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATE_REQUEST_V1_CODEC);
  correlatedCodecKeyword("x-agentLifecycleRecoveryIntentCorrelated", AGENT_LIFECYCLE_RECOVERY_INTENT_V1_CODEC);
  target.addKeyword({
    keyword: "x-maxUtf8Bytes",
    schemaType: "number",
    type: "string",
    errors: false,
    validate: (maximum, data) => (
      typeof maximum === "number" && typeof data === "string" && Buffer.byteLength(data, "utf8") <= maximum
    ),
  });
  target.addKeyword({
    keyword: "x-boundedJson",
    schemaType: "boolean",
    errors: false,
    validate: (enabled, data) => {
      if (enabled !== true) return true;
      try {
        parseJsonValue(data, "jsonValue");
        return true;
      } catch {
        return false;
      }
    },
  });
  target.addKeyword({
    keyword: "x-base64LengthMatches",
    schemaType: "boolean",
    type: "object",
    errors: false,
    validate: (enabled, data) => {
      if (enabled !== true) return true;
      if (data === null || typeof data !== "object" || Array.isArray(data)) return false;
      const record = data as Readonly<Record<string, unknown>>;
      return typeof record.payload === "string" && typeof record.rawByteLength === "number" &&
        Buffer.from(record.payload, "base64").byteLength === record.rawByteLength;
    },
  });
  target.addKeyword({
    keyword: "x-searchEntriesCanonical",
    schemaType: "boolean",
    type: "object",
    errors: false,
    validate: (enabled, data) => {
      if (enabled !== true) return true;
      if (data === null || typeof data !== "object" || Array.isArray(data)) return false;
      const entries = (data as Readonly<Record<string, unknown>>).entries;
      if (!Array.isArray(entries)) return false;
      let previous = "";
      for (const entry of entries) {
        if (entry === null || typeof entry !== "object" || Array.isArray(entry)) return false;
        const record = entry as Readonly<Record<string, unknown>>;
        if (typeof record.snippet !== "string" || typeof record.rawByteLength !== "number") return false;
        const decodedLength = Buffer.from(record.snippet, "base64").byteLength;
        if (decodedLength !== record.rawByteLength || decodedLength > 65_536) return false;
        const key = `${String(record.objectDigest)}\u0000${String(record.offset).padStart(16, "0")}\u0000${String(record.rawByteLength).padStart(16, "0")}`;
        if (previous !== "" && previous >= key) return false;
        previous = key;
      }
      return true;
    },
  });
  target.addKeyword({
    keyword: "x-maxCanonicalJsonBytes",
    schemaType: "number",
    errors: false,
    validate: (maximum, data) => {
      if (typeof maximum !== "number") return false;
      const canonical = (value: unknown): string => {
        if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") return JSON.stringify(value);
        if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
        if (typeof value !== "object") return "";
        const record = value as Readonly<Record<string, unknown>>;
        return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonical(record[key])}`).join(",")}}`;
      };
      return Buffer.byteLength(canonical(data), "utf8") <= maximum;
    },
  });
}
