import {
  arrayOf,
  boundedString,
  defineCodec,
  enumeration,
  integer,
  jsonValue,
  literal,
  nullable,
  objectCodec,
  parserBacked,
  relativePath,
  sha256,
  timestamp,
  unionOf,
  type Codec,
  type CodecOutput,
} from "./codec.js";
import { PROVIDER_ACTION_REF_V1_CODEC } from "./launch.js";
import { OPERATION_REGISTRY } from "./operations.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const nullableId = nullable(id256);
const nullableNonnegative = nullable(nonnegative);

export const RESOLVED_EFFORT_V1_CODEC = unionOf([
  objectCodec({ kind: literal("applied"), value: id256 }),
  objectCodec({ kind: literal("inapplicable") }),
]);
export type ResolvedEffortV1 = CodecOutput<typeof RESOLVED_EFFORT_V1_CODEC>;

const reasoningEffort = enumeration(["none", "low", "medium", "high", "xhigh", "max"]);
const nullableReasoningEffort = nullable(reasoningEffort);
const orchestrationMode = enumeration(["single", "native-subagents", "dynamic-workflow", "provider-multi-agent"]);
const snapshotSource = enumeration(["runtime-discovery", "capability-fixture", "unavailable"]);

const effortNormalizationCodec = objectCodec({
  rawProviderEffort: id256,
  normalizedReasoningEffort: reasoningEffort,
});
const effortCapabilitiesCodec = unionOf([
  objectCodec({
    kind: literal("applied"),
    normalizations: arrayOf(effortNormalizationCodec, { minimum: 1, maximum: 64, unique: true }),
  }),
  objectCodec({ kind: literal("inapplicable") }),
]);
const nativeModeNormalizationCodec = objectCodec({
  rawNativeMode: nullableId,
  orchestrationMode,
});
const modelCapabilityCodec = objectCodec({
  family: id256,
  model: id256,
  effort: effortCapabilitiesCodec,
  nativeModeNormalizations: arrayOf(nativeModeNormalizationCodec, { minimum: 1, maximum: 64, unique: true }),
});
const availableCapabilitiesCodec = objectCodec({
  kind: literal("available"),
  modelCatalog: arrayOf(modelCapabilityCodec, { minimum: 1, maximum: 256, unique: true }),
  context: objectCodec({
    reporting: enumeration(["reported", "estimated", "unavailable"]),
    compactInPlace: unionOf([literal(true), literal(false), literal("unknown")]),
    freshSession: unionOf([literal(true), literal(false), literal("unknown")]),
    boundaryInjection: enumeration(["verified", "unverified", "unavailable"]),
  }),
  orchestration: objectCodec({
    nativeSubagents: enumeration(["none", "bounded", "recursive", "unknown"]),
    maxDepth: nullableNonnegative,
    maxConcurrency: nullable(positive),
  }),
  safety: objectCodec({
    enforcedReadOnly: unionOf([literal(true), literal(false), literal("unknown")]),
    permissionSource: enumeration(["adapter", "host", "config-overlay", "unknown"]),
  }),
});
const unavailableCapabilitiesCodec = objectCodec({
  kind: literal("unavailable"),
  reason: boundedString({ maxBytes: 512, example: "adapter unavailable" }),
});

const adapterCapabilitySnapshotBaseCodec = objectCodec({
  schemaVersion: literal(1),
  snapshotId: id256,
  snapshotGeneration: positive,
  adapterId: id256,
  adapterContractDigest: sha256,
  hostId: id256,
  hostVersion: id256,
  source: snapshotSource,
  observedAt: timestamp,
  expiresAt: timestamp,
  capabilities: unionOf([availableCapabilitiesCodec, unavailableCapabilitiesCodec]),
  capabilityBodyDigest: sha256,
  snapshotDigest: sha256,
});

export const ADAPTER_CAPABILITY_SNAPSHOT_V1_CODEC = parserBacked(
  defineCodec(
    { ...adapterCapabilitySnapshotBaseCodec.schema, "x-adapterCapabilitySnapshotCorrelated": true },
    adapterCapabilitySnapshotBaseCodec.example,
    (input, path) => adapterCapabilitySnapshotBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const source = record.source;
    const capabilities = record.capabilities as Readonly<Record<string, unknown>>;
    const kind = capabilities.kind;
    if ((source === "unavailable") !== (kind === "unavailable")) {
      throw new TypeError(`${path}.source and capabilities.kind are crossed`);
    }
    if (kind === "available") {
      const catalogue = capabilities.modelCatalog as readonly Readonly<Record<string, unknown>>[];
      let previousModelKey: string | undefined;
      for (const [modelIndex, model] of catalogue.entries()) {
        const modelKey = `${String(model.family)}\u0000${String(model.model)}`;
        if (previousModelKey !== undefined && previousModelKey >= modelKey) {
          throw new TypeError(`${path}.capabilities.modelCatalog must be strictly sorted and unique by family/model`);
        }
        previousModelKey = modelKey;
        const effort = model.effort as Readonly<Record<string, unknown>>;
        if (effort.kind === "applied") {
          const mappings = effort.normalizations as readonly Readonly<Record<string, unknown>>[];
          let previousRawEffort: string | undefined;
          for (const mapping of mappings) {
            const raw = String(mapping.rawProviderEffort);
            if (previousRawEffort !== undefined && previousRawEffort >= raw) {
              throw new TypeError(`${path}.capabilities.modelCatalog[${String(modelIndex)}].effort.normalizations must be sorted and unique by rawProviderEffort`);
            }
            previousRawEffort = raw;
          }
        }
        const nativeMappings = model.nativeModeNormalizations as readonly Readonly<Record<string, unknown>>[];
        let previousNativeKey: string | undefined;
        for (const mapping of nativeMappings) {
          const raw = mapping.rawNativeMode;
          const key = raw === null ? "\u0000" : `\u0001${String(raw)}`;
          if (previousNativeKey !== undefined && previousNativeKey >= key) {
            throw new TypeError(`${path}.capabilities.modelCatalog[${String(modelIndex)}].nativeModeNormalizations must place null first and be sorted unique by rawNativeMode`);
          }
          previousNativeKey = key;
        }
      }
    }
    if (Date.parse(String(record.expiresAt)) <= Date.parse(String(record.observedAt))) {
      throw new TypeError(`${path}.expiresAt must be later than observedAt`);
    }
    return record;
  },
  { ...adapterCapabilitySnapshotBaseCodec.example, expiresAt: "2026-07-11T11:00:00Z" },
);
export type AdapterCapabilitySnapshotV1 = CodecOutput<typeof ADAPTER_CAPABILITY_SNAPSHOT_V1_CODEC>;

export const CAPABILITY_SNAPSHOT_REF_V1_CODEC = objectCodec({
  snapshotId: id256,
  snapshotGeneration: positive,
  snapshotDigest: sha256,
  capabilityBodyDigest: sha256,
});

const capabilitySnapshotPointCodec = objectCodec({
  snapshotRef: CAPABILITY_SNAPSHOT_REF_V1_CODEC,
  source: snapshotSource,
  observedAt: timestamp,
  expiresAt: timestamp,
});
export const CAPABILITY_SNAPSHOT_SUMMARY_V1_CODEC = objectCodec({
  admission: capabilitySnapshotPointCodec,
  dispatch: nullable(capabilitySnapshotPointCodec),
});

const visibleDescriptionCodec = boundedString({ maxBytes: 4096, example: "Visible capability" });
const visibleCatalogueEntryCodec = objectCodec({ name: id256, description: visibleDescriptionCodec });
const visibleToolCodec = objectCodec({ name: id256, description: visibleDescriptionCodec, inputSchema: jsonValue });
export const DISCOVERY_SURFACE_MANIFEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  hostId: id256,
  hostVersion: id256,
  providerProfile: id256,
  rawNativeMode: nullableId,
  principalScopeDigest: sha256,
  permissionProfileDigest: sha256,
  negotiatedFeatureSetDigest: sha256,
  rendererVersion: id256,
  bootstrapText: boundedString({ minBytes: 0, maxBytes: 1_048_576, example: "bootstrap" }),
  skills: arrayOf(visibleCatalogueEntryCodec, { maximum: 4096 }),
  tools: arrayOf(visibleToolCodec, { maximum: 4096 }),
  agentCommands: arrayOf(visibleCatalogueEntryCodec, { maximum: 4096 }),
  nativePreambleText: boundedString({ minBytes: 0, maxBytes: 1_048_576, example: "preamble" }),
  bootstrapDigest: sha256,
  skillCatalogueDigest: sha256,
  toolRegistryDigest: sha256,
  agentCommandRegistryDigest: sha256,
  nativePreambleDigest: sha256,
});

const artifactRefCodec = objectCodec({ path: relativePath, digest: sha256 });
export const DISCOVERY_SURFACE_REF_V1_CODEC = objectCodec({
  evidenceId: id256,
  evidenceRevision: positive,
  artifactRef: artifactRefCodec,
  hostId: id256,
  hostVersion: id256,
  providerProfile: id256,
  rawNativeMode: nullableId,
  evidenceKind: literal("discovery-surface.v1"),
  producer: literal("fabric-daemon"),
  manifestDigest: sha256,
});

export const ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC = objectCodec({
  configurationId: id256,
  configurationRevision: positive,
  configurationDigest: sha256,
});

const requestedRouteCodec = objectCodec({
  adapterAlias: id256,
  modelAlias: id256,
  explicitModel: nullableId,
  rawProviderEffort: nullableId,
  rawNativeMode: nullableId,
});
const admittedRouteCodec = objectCodec({
  hostId: id256,
  adapterId: id256,
  adapterContractDigest: sha256,
  endpointProvider: id256,
  family: id256,
  model: id256,
  resolvedEffort: RESOLVED_EFFORT_V1_CODEC,
  normalizedReasoningEffort: nullableReasoningEffort,
  rawNativeMode: nullableId,
  orchestrationMode,
  capabilitySnapshotRef: CAPABILITY_SNAPSHOT_REF_V1_CODEC,
  effectiveConfigurationRef: ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC,
  requestedConfigurationDigest: sha256,
  effectiveConfigurationDigest: sha256,
  permissionProfileDigest: sha256,
  discoverySurfaceRef: DISCOVERY_SURFACE_REF_V1_CODEC,
});
const deployedRouteAdmissionBaseCodec = objectCodec({
  schemaVersion: literal(1),
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  routeRequestDigest: sha256,
  routeReceiptDigest: sha256,
  requested: requestedRouteCodec,
  admitted: admittedRouteCodec,
  routePolicyRevision: positive,
  harnessRevision: positive,
  harnessDigest: sha256,
  contextPolicyRevision: positive,
  contextPolicyDigest: sha256,
  admissionDigest: sha256,
});
export const DEPLOYED_ROUTE_ADMISSION_V1_CODEC = parserBacked(
  defineCodec(
    { ...deployedRouteAdmissionBaseCodec.schema, "x-deployedRouteAdmissionCorrelated": true },
    deployedRouteAdmissionBaseCodec.example,
    (input, path) => deployedRouteAdmissionBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const actionRef = record.actionRef as Readonly<Record<string, unknown>>;
    const admitted = record.admitted as Readonly<Record<string, unknown>>;
    if (actionRef.adapterId !== admitted.adapterId) {
      throw new TypeError(`${path}.actionRef.adapterId must equal admitted.adapterId`);
    }
    const requested = record.requested as Readonly<Record<string, unknown>>;
    const resolvedEffort = admitted.resolvedEffort as Readonly<Record<string, unknown>>;
    if (resolvedEffort.kind === "inapplicable" && requested.rawProviderEffort !== null) {
      throw new TypeError(`${path}.requested.rawProviderEffort must be null for inapplicable effort`);
    }
    if (resolvedEffort.kind === "inapplicable" && admitted.normalizedReasoningEffort !== null) {
      throw new TypeError(`${path}.admitted.normalizedReasoningEffort must be null for inapplicable effort`);
    }
    return record;
  },
  deployedRouteAdmissionBaseCodec.example,
);

export const DEPLOYED_ROUTE_DISPATCH_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  admissionDigest: sha256,
  dispatchOrdinal: positive,
  capabilitySnapshotRef: CAPABILITY_SNAPSHOT_REF_V1_CODEC,
  effectiveConfigurationRef: ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC,
  permissionProfileDigest: sha256,
  discoverySurfaceRef: DISCOVERY_SURFACE_REF_V1_CODEC,
  dispatchedAt: timestamp,
  dispatchDigest: sha256,
});

function observedValueCodec<T>(valueCodec: Codec<T>): Codec<unknown> {
  return unionOf([
    objectCodec({
      state: literal("observed"),
      value: valueCodec,
      source: literal("provider-result"),
      confidence: literal("exact"),
    }),
    objectCodec({
      state: literal("observed"),
      value: valueCodec,
      source: literal("adapter-attestation"),
      confidence: literal("attested"),
    }),
    objectCodec({
      state: literal("unavailable"),
      value: literal(null),
      source: literal("unavailable"),
      confidence: literal("unknown"),
    }),
  ]);
}

function requiredObservedValueCodec<T>(valueCodec: Codec<T>): Codec<unknown> {
  return unionOf([
    objectCodec({
      state: literal("observed"),
      value: valueCodec,
      source: literal("provider-result"),
      confidence: literal("exact"),
    }),
    objectCodec({
      state: literal("observed"),
      value: valueCodec,
      source: literal("adapter-attestation"),
      confidence: literal("attested"),
    }),
  ]);
}

export const OBSERVED_IDENTIFIER_V1_CODEC = observedValueCodec(id256);
export const OBSERVED_RESOLVED_EFFORT_V1_CODEC = observedValueCodec(RESOLVED_EFFORT_V1_CODEC);
export const OBSERVED_REASONING_EFFORT_V1_CODEC = observedValueCodec(nullableReasoningEffort);
export const OBSERVED_NULL_NATIVE_MODE_V1_CODEC = observedValueCodec(nullableId);
export const OBSERVED_ORCHESTRATION_MODE_V1_CODEC = observedValueCodec(orchestrationMode);

function validateObservedEffortCorrelation(record: Readonly<Record<string, unknown>>, path: string): void {
  const resolvedEffort = record.resolvedEffort as Readonly<Record<string, unknown>>;
  const normalizedEffort = record.normalizedReasoningEffort as Readonly<Record<string, unknown>>;
  if (normalizedEffort.state === "observed" && normalizedEffort.value === null) {
    const resolved = resolvedEffort.value as Readonly<Record<string, unknown>> | null;
    if (resolvedEffort.state !== "observed" || resolved === null || resolved.kind !== "inapplicable") {
      throw new TypeError(`${path}.normalizedReasoningEffort observed null requires observed inapplicable resolvedEffort`);
    }
  }
  if (resolvedEffort.state === "observed" && normalizedEffort.state === "observed") {
    const resolved = resolvedEffort.value as Readonly<Record<string, unknown>>;
    if (resolved.kind === "inapplicable" && normalizedEffort.value !== null) {
      throw new TypeError(`${path}.normalizedReasoningEffort must be null for inapplicable resolvedEffort`);
    }
  }
}

const deployedRouteObservationBaseCodec = objectCodec({
  schemaVersion: literal(1),
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  admissionDigest: sha256,
  hostId: OBSERVED_IDENTIFIER_V1_CODEC,
  adapterId: OBSERVED_IDENTIFIER_V1_CODEC,
  endpointProvider: OBSERVED_IDENTIFIER_V1_CODEC,
  family: OBSERVED_IDENTIFIER_V1_CODEC,
  model: OBSERVED_IDENTIFIER_V1_CODEC,
  resolvedEffort: OBSERVED_RESOLVED_EFFORT_V1_CODEC,
  normalizedReasoningEffort: OBSERVED_REASONING_EFFORT_V1_CODEC,
  rawNativeMode: OBSERVED_NULL_NATIVE_MODE_V1_CODEC,
  orchestrationMode: OBSERVED_ORCHESTRATION_MODE_V1_CODEC,
  observedAt: timestamp,
  observationDigest: sha256,
});
export const DEPLOYED_ROUTE_OBSERVATION_V1_CODEC = parserBacked(
  defineCodec(
    { ...deployedRouteObservationBaseCodec.schema, "x-deployedRouteObservationCorrelated": true },
    deployedRouteObservationBaseCodec.example,
    (input, path) => deployedRouteObservationBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    validateObservedEffortCorrelation(record, path);
    return record;
  },
  deployedRouteObservationBaseCodec.example,
);

const actualReviewRouteIdentityBaseCodec = objectCodec({
  schemaVersion: literal(1),
  admissionDigest: sha256,
  observationDigest: sha256,
  hostId: OBSERVED_IDENTIFIER_V1_CODEC,
  adapterId: OBSERVED_IDENTIFIER_V1_CODEC,
  endpointProvider: requiredObservedValueCodec(id256),
  family: requiredObservedValueCodec(id256),
  model: requiredObservedValueCodec(id256),
  resolvedEffort: OBSERVED_RESOLVED_EFFORT_V1_CODEC,
  normalizedReasoningEffort: OBSERVED_REASONING_EFFORT_V1_CODEC,
  rawNativeMode: OBSERVED_NULL_NATIVE_MODE_V1_CODEC,
  orchestrationMode: OBSERVED_ORCHESTRATION_MODE_V1_CODEC,
  actualRouteIdentityDigest: sha256,
});
export const ACTUAL_REVIEW_ROUTE_IDENTITY_V1_CODEC = parserBacked(
  defineCodec(
    { ...actualReviewRouteIdentityBaseCodec.schema, "x-actualReviewRouteCorrelated": true },
    actualReviewRouteIdentityBaseCodec.example,
    (input, path) => actualReviewRouteIdentityBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const rawNativeMode = record.rawNativeMode as Readonly<Record<string, unknown>>;
    const orchestrationMode = record.orchestrationMode as Readonly<Record<string, unknown>>;
    if (rawNativeMode.state === "observed" && rawNativeMode.value === null &&
      !(orchestrationMode.state === "observed" && orchestrationMode.value === "single")) {
      throw new TypeError(`${path}.rawNativeMode observed null requires observed single orchestrationMode`);
    }
    if (orchestrationMode.state === "observed" && orchestrationMode.value !== "single" &&
      !(rawNativeMode.state === "observed" && rawNativeMode.value !== null)) {
      throw new TypeError(`${path}.orchestrationMode non-single requires observed nonnull rawNativeMode`);
    }
    validateObservedEffortCorrelation(record, path);
    const resolvedEffort = record.resolvedEffort as Readonly<Record<string, unknown>>;
    const normalizedEffort = record.normalizedReasoningEffort as Readonly<Record<string, unknown>>;
    if (resolvedEffort.state === "observed" && normalizedEffort.state === "observed") {
      const effort = resolvedEffort.value as Readonly<Record<string, unknown>>;
      if (effort.kind === "applied" && normalizedEffort.value === null) {
        throw new TypeError(`${path}.normalizedReasoningEffort must be nonnull for applied resolvedEffort`);
      }
      if (effort.kind === "inapplicable" && normalizedEffort.value !== null) {
        throw new TypeError(`${path}.normalizedReasoningEffort must be null for inapplicable resolvedEffort`);
      }
    }
    return record;
  },
  actualReviewRouteIdentityBaseCodec.example,
);

const routeTargetChairCodec = objectCodec({
  agentId: id256,
  bindingGeneration: positive,
  principalGeneration: positive,
  chairLeaseGeneration: positive,
  providerSessionGeneration: positive,
  bridgeGeneration: positive,
  adapterId: id256,
  adapterContractDigest: sha256,
  modelFamily: id256,
  model: id256,
  routeReceiptDigest: nullable(sha256),
});
export const LOCAL_PROVIDER_ROUTE_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  routeRequestDigest: sha256,
  routeReceiptDigest: sha256,
  adapterId: id256,
  adapterContractDigest: sha256,
  providerFamily: id256,
  resolvedModel: id256,
  requestedEffort: nullableId,
  resolvedEffort: RESOLVED_EFFORT_V1_CODEC,
  targetGeneration: nullable(positive),
  slot: nullable(enumeration(["native", "other-primary", "cursor-grok", "agy-gemini"])),
  reviewedArtifactRef: nullableId,
  publicationLineageDigest: nullable(sha256),
  bundleDigest: nullable(sha256),
  manifestRootDigest: nullable(sha256),
  coverageDigest: nullable(sha256),
  bundleSearchIndexDigest: nullable(sha256),
  riskReadMapDigest: nullable(sha256),
  mandatoryReadSetDigest: nullable(sha256),
  finalPromptDigest: nullable(sha256),
  targetChair: nullable(routeTargetChairCodec),
  profileDigest: nullable(sha256),
  slotHeadGeneration: nullable(nonnegative),
  attemptGeneration: nullable(positive),
});
const providerRouteBaseCodec = objectCodec({
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  taskId: id256,
  route: LOCAL_PROVIDER_ROUTE_V1_CODEC,
  admission: DEPLOYED_ROUTE_ADMISSION_V1_CODEC,
  capabilitySummary: CAPABILITY_SNAPSHOT_SUMMARY_V1_CODEC,
  latestDispatch: nullable(DEPLOYED_ROUTE_DISPATCH_V1_CODEC),
  observation: nullable(DEPLOYED_ROUTE_OBSERVATION_V1_CODEC),
});
export const PROVIDER_ROUTE_V1_CODEC = parserBacked(
  defineCodec(
    { ...providerRouteBaseCodec.schema, "x-providerRouteCorrelated": true },
    providerRouteBaseCodec.example,
    (input, path) => providerRouteBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const actionRef = record.actionRef as Readonly<Record<string, unknown>>;
    const route = record.route as Readonly<Record<string, unknown>>;
    const admission = record.admission as Readonly<Record<string, unknown>>;
    const admitted = admission.admitted as Readonly<Record<string, unknown>>;
    const requested = admission.requested as Readonly<Record<string, unknown>>;
    const same = (left: unknown, right: unknown): boolean => JSON.stringify(left) === JSON.stringify(right);
    if (!same(actionRef, admission.actionRef)) throw new TypeError(`${path}.actionRef must equal admission.actionRef`);
    if (actionRef.adapterId !== route.adapterId) throw new TypeError(`${path}.actionRef.adapterId must equal route.adapterId`);
    for (const field of ["routeRequestDigest", "routeReceiptDigest"] as const) {
      if (admission[field] !== route[field]) throw new TypeError(`${path}.admission.${field} must equal route.${field}`);
    }
    if (requested.rawProviderEffort !== route.requestedEffort) throw new TypeError(`${path}.requested effort must equal route.requestedEffort`);
    if (admitted.adapterId !== route.adapterId || admitted.adapterContractDigest !== route.adapterContractDigest || admitted.family !== route.providerFamily || admitted.model !== route.resolvedModel || !same(admitted.resolvedEffort, route.resolvedEffort)) {
      throw new TypeError(`${path}.admitted route identity must equal route projection`);
    }
    const summary = record.capabilitySummary as Readonly<Record<string, unknown>>;
    const admissionSummary = summary.admission as Readonly<Record<string, unknown>>;
    if (!same(admissionSummary.snapshotRef, admitted.capabilitySnapshotRef)) throw new TypeError(`${path}.capabilitySummary.admission must bind admission snapshot`);
    const latestDispatch = record.latestDispatch as Readonly<Record<string, unknown>> | null;
    const dispatchSummary = summary.dispatch as Readonly<Record<string, unknown>> | null;
    if ((latestDispatch === null) !== (dispatchSummary === null)) throw new TypeError(`${path}.dispatch summary and latestDispatch must be null together`);
    if (latestDispatch !== null) {
      if (!same(latestDispatch.actionRef, actionRef) || latestDispatch.admissionDigest !== admission.admissionDigest || !same(latestDispatch.capabilitySnapshotRef, dispatchSummary?.snapshotRef) || !same(latestDispatch.effectiveConfigurationRef, admitted.effectiveConfigurationRef)) {
        throw new TypeError(`${path}.latestDispatch must equality-bind admission and dispatch snapshot`);
      }
    }
    const observation = record.observation as Readonly<Record<string, unknown>> | null;
    if (observation !== null && (!same(observation.actionRef, actionRef) || observation.admissionDigest !== admission.admissionDigest)) {
      throw new TypeError(`${path}.observation must equality-bind action and admission`);
    }
    return record;
  },
  providerRouteBaseCodec.example,
);

const observationAuditRefCodec = objectCodec({
  sourceEventId: id256,
  providerGeneration: positive,
  contextRevision: nonnegative,
  evidenceDigest: sha256,
});
const providerContextPressureBaseCodec = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  agentId: id256,
  adapterId: id256,
  providerGeneration: positive,
  contextRevision: nonnegative,
  observationAuditRef: observationAuditRefCodec,
  pressure: enumeration(["low", "medium", "high", "unknown"]),
  source: enumeration(["native-exact", "native-estimated", "hook-boundary", "unavailable"]),
  confidence: enumeration(["exact", "estimated", "unknown"]),
  windowTokens: nullableNonnegative,
  usedTokens: nullableNonnegative,
  remainingTokens: nullableNonnegative,
  observedAt: timestamp,
  expiresAt: timestamp,
  evidenceDigest: sha256,
  revision: positive,
});
export const PROVIDER_CONTEXT_PRESSURE_V1_CODEC = parserBacked(
  defineCodec(
    { ...providerContextPressureBaseCodec.schema, "x-providerContextPressureCorrelated": true },
    providerContextPressureBaseCodec.example,
    (input, path) => providerContextPressureBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    const audit = record.observationAuditRef as Readonly<Record<string, unknown>>;
    if (audit.providerGeneration !== record.providerGeneration || audit.contextRevision !== record.contextRevision) {
      throw new TypeError(`${path}.observationAuditRef must equality-bind generation and context revision`);
    }
    const tokens = [record.windowTokens, record.usedTokens, record.remainingTokens];
    const allNull = tokens.every((entry) => entry === null);
    const allNumbers = tokens.every((entry) => typeof entry === "number");
    if (!allNull && !allNumbers) throw new TypeError(`${path} token fields must be all null or all nonnull`);
    if (allNumbers && Number(record.usedTokens) + Number(record.remainingTokens) !== Number(record.windowTokens)) {
      throw new TypeError(`${path}.usedTokens and remainingTokens must sum to windowTokens`);
    }
    if (record.source === "unavailable") {
      if (!allNull || record.pressure !== "unknown" || record.confidence !== "unknown") {
        throw new TypeError(`${path} unavailable source requires unknown pressure/confidence and null tokens`);
      }
    } else if (record.source === "native-exact" && (record.confidence !== "exact" || !allNumbers)) {
      throw new TypeError(`${path} native-exact requires exact confidence and token counts`);
    } else if (record.source === "native-estimated" && (record.confidence !== "estimated" || !allNumbers)) {
      throw new TypeError(`${path} native-estimated requires estimated confidence and token counts`);
    }
    if (record.confidence === "unknown" && record.pressure !== "unknown") {
      throw new TypeError(`${path} unknown confidence requires unknown pressure`);
    }
    if (Date.parse(String(record.expiresAt)) <= Date.parse(String(record.observedAt))) {
      throw new TypeError(`${path}.expiresAt must be later than observedAt`);
    }
    return record;
  },
  {
    ...providerContextPressureBaseCodec.example,
    pressure: "unknown",
    source: "unavailable",
    confidence: "unknown",
    windowTokens: null,
    usedTokens: null,
    remainingTokens: null,
    expiresAt: "2026-07-11T11:00:00Z",
  },
);

export const PROVIDER_CONTEXT_PRESSURE_READ_REQUEST_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  agentId: id256,
});

const providerContextPressureReadBaseCodec = unionOf([
  objectCodec({
    schemaVersion: literal(1),
    currency: literal("current"),
    pressure: PROVIDER_CONTEXT_PRESSURE_V1_CODEC,
    readAt: timestamp,
    ageSeconds: nonnegative,
  }),
  objectCodec({
    schemaVersion: literal(1),
    currency: literal("stale"),
    pressure: PROVIDER_CONTEXT_PRESSURE_V1_CODEC,
    readAt: timestamp,
    ageSeconds: nonnegative,
  }),
  objectCodec({
    schemaVersion: literal(1),
    currency: literal("unavailable"),
    pressure: literal(null),
    readAt: timestamp,
    ageSeconds: literal(null),
  }),
]);
export const PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC = parserBacked(
  defineCodec(
    { ...providerContextPressureReadBaseCodec.schema, "x-providerContextPressureReadCorrelated": true },
    providerContextPressureReadBaseCodec.example,
    (input, path) => providerContextPressureReadBaseCodec.parse(input, path),
  ),
  (value, path) => {
    const record = value as Readonly<Record<string, unknown>>;
    if (record.currency !== "unavailable") {
      const pressure = record.pressure as Readonly<Record<string, unknown>>;
      const readAt = Date.parse(String(record.readAt));
      const observedAt = Date.parse(String(pressure.observedAt));
      const expiresAt = Date.parse(String(pressure.expiresAt));
      if (readAt < observedAt) throw new TypeError(`${path}.readAt must be at or after pressure.observedAt`);
      const expectedAge = Math.floor((readAt - observedAt) / 1_000);
      if (record.ageSeconds !== expectedAge) {
        throw new TypeError(`${path}.ageSeconds must equal the whole-second readAt-observedAt difference`);
      }
      if (record.currency === "current" && readAt >= expiresAt) {
        throw new TypeError(`${path}.current readAt must be before pressure.expiresAt`);
      }
      if (record.currency === "stale" && readAt < expiresAt) {
        throw new TypeError(`${path}.stale readAt must be at or after pressure.expiresAt`);
      }
    }
    return record;
  },
  { ...providerContextPressureReadBaseCodec.example, ageSeconds: 0 },
);

export const REGISTERED_EVIDENCE_REF_V1_CODEC = objectCodec({
  evidenceId: id256,
  evidenceRevision: positive,
  artifactRef: artifactRefCodec,
});

const adapterEffectiveConfigurationCommon = {
  schemaVersion: literal(1),
  configurationId: id256,
  configurationRevision: positive,
  adapterId: id256,
  adapterContractDigest: sha256,
  executableIdentityDigest: sha256,
  capabilitySnapshotRef: CAPABILITY_SNAPSHOT_REF_V1_CODEC,
  subjectRefDigest: sha256,
  requestedConfigurationDigest: sha256,
  effectiveConfigurationDigest: sha256,
  permissionProfileDigest: sha256,
  discoverySurfaceRef: DISCOVERY_SURFACE_REF_V1_CODEC,
  ignoredOrUnsupportedFields: arrayOf(boundedString({ maxBytes: 512, example: "settings.field" }), { maximum: 1024, unique: true }),
  permissionSource: enumeration(["adapter", "host", "config-overlay", "unknown"]),
  observedAt: timestamp,
  configurationDigest: sha256,
} as const;
export const ADAPTER_EFFECTIVE_CONFIGURATION_V1_CODEC = unionOf([
  objectCodec({
    ...adapterEffectiveConfigurationCommon,
    subjectKind: literal("activation"),
    subjectRef: objectCodec({ activationId: id256, activationRevision: positive }),
    activationConfigurationRef: literal(null),
  }),
  objectCodec({
    ...adapterEffectiveConfigurationCommon,
    subjectKind: literal("provider-smoke"),
    subjectRef: objectCodec({ smokeId: id256, actionRef: PROVIDER_ACTION_REF_V1_CODEC }),
    activationConfigurationRef: ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC,
  }),
  objectCodec({
    ...adapterEffectiveConfigurationCommon,
    subjectKind: literal("provider-action"),
    subjectRef: objectCodec({ actionRef: PROVIDER_ACTION_REF_V1_CODEC }),
    activationConfigurationRef: ADAPTER_EFFECTIVE_CONFIGURATION_REF_V1_CODEC,
  }),
]);

const fabricOperationCodec = enumeration(
  Object.keys(OPERATION_REGISTRY) as [`fabric.v1.${string}`, ...`fabric.v1.${string}`[]],
);
export const FABRIC_OPERATIONAL_SPAN_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  spanId: id256,
  parentSpanId: nullableId,
  runId: id256,
  taskId: nullableId,
  agentId: nullableId,
  actionRef: nullable(PROVIDER_ACTION_REF_V1_CODEC),
  routeAdmissionDigest: nullable(sha256),
  operation: fabricOperationCodec,
  status: enumeration(["ok", "error", "cancelled", "unknown"]),
  durationMs: nonnegative,
  inputTokens: nullableNonnegative,
  outputTokens: nullableNonnegative,
  retryCount: nonnegative,
  errorCode: nullableId,
  observedAt: timestamp,
});

export type DeployedRouteAdmissionV1 = CodecOutput<typeof DEPLOYED_ROUTE_ADMISSION_V1_CODEC>;
export type DeployedRouteDispatchV1 = CodecOutput<typeof DEPLOYED_ROUTE_DISPATCH_V1_CODEC>;
export type DeployedRouteObservationV1 = CodecOutput<typeof DEPLOYED_ROUTE_OBSERVATION_V1_CODEC>;
export type ProviderContextPressureReadRequestV1 = CodecOutput<typeof PROVIDER_CONTEXT_PRESSURE_READ_REQUEST_V1_CODEC>;
export type ProviderContextPressureReadV1 = CodecOutput<typeof PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC>;
