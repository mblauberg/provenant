import {
  boundedString,
  defineCodec,
  enumeration,
  integer,
  literal,
  nullable,
  objectCodec,
  relativePath,
  sha256,
  unionOf,
  type Codec,
} from "./codec.js";
import { PROVIDER_ACTION_REF_V1_CODEC } from "./launch.js";
import { LIFECYCLE_CUSTODY_REF_V1_CODEC } from "./provider-review-core.js";

const positive = integer({ minimum: 1 });
const nonnegative = integer();
const id256 = boundedString({ maxBytes: 256, example: "id_01" });
const nullableDigest = nullable(sha256);

type ProviderActionRef = { readonly adapterId: string; readonly actionId: string };
type CustodyRef = {
  readonly schemaVersion: 1;
  readonly runId: string;
  readonly agentId: string;
  readonly custodyId: string;
  readonly custodyRevision: number;
};
type GenerationLossRef = {
  readonly schemaVersion: 1;
  readonly runId: string;
  readonly agentId: string;
  readonly generationLossId: string;
  readonly generationLossRevision: number;
};
type CheckpointRef = { readonly checkpointId: string; readonly checkpointRevision: number };

type LifecycleCustodyCommon = {
  readonly schemaVersion: 1;
  readonly sourceKind: "custody";
  readonly agentId: string;
  readonly custodyId: string;
  readonly custodyRevision: number;
  readonly actionRef: ProviderActionRef;
  readonly sourceProviderGeneration: number;
  readonly sourcePrincipalGeneration: number;
  readonly sourceBridgeGeneration: number;
  readonly targetProviderGeneration: number;
  readonly targetPrincipalGeneration: number;
  readonly targetBridgeGeneration: number;
  readonly checkpointDigest: string;
};
export type LifecycleCustodyRowV1 = LifecycleCustodyCommon & (
  | { readonly state: "awaiting-boundary" | "prepared" | "dispatched" | "accepted" | "ambiguous"; readonly disposition: null; readonly terminalEvidenceDigest: null }
  | { readonly state: "provider-terminal" | "committing"; readonly disposition: null; readonly terminalEvidenceDigest: string }
  | { readonly state: "finalized"; readonly disposition: "adopted" | "no-effect" | "quarantined" | "superseded" | "abandoned"; readonly terminalEvidenceDigest: string }
);

type LifecycleGenerationLossCommon = {
  readonly schemaVersion: 1;
  readonly sourceKind: "generation-loss";
  readonly agentId: string;
  readonly generationLossId: string;
  readonly generationLossRevision: number;
  readonly lossKind: "generation-advance" | "context-advance";
  readonly oldProviderGeneration: number;
  readonly newProviderGeneration: number;
  readonly oldContextRevision: number | null;
  readonly newContextRevision: number;
  readonly checkpointState: "absent" | "invalid" | "last-validated";
  readonly checkpointDigest: string | null;
  readonly lossEvidenceDigest: string;
};
export type LifecycleGenerationLossRowV1 = LifecycleGenerationLossCommon & (
  | { readonly recoveryActionRef: null; readonly abandonKind: "none"; readonly state: "open"; readonly disposition: null; readonly terminalEvidenceDigest: null }
  | { readonly recoveryActionRef: ProviderActionRef; readonly abandonKind: "none"; readonly state: "recovery-in-progress"; readonly disposition: null; readonly terminalEvidenceDigest: null }
  | { readonly recoveryActionRef: ProviderActionRef; readonly abandonKind: "none"; readonly state: "recovered-adopted"; readonly disposition: "recovered-adopted"; readonly terminalEvidenceDigest: string }
  | { readonly recoveryActionRef: null; readonly abandonKind: "direct-open"; readonly state: "abandoned"; readonly disposition: "abandoned"; readonly terminalEvidenceDigest: string }
  | { readonly recoveryActionRef: ProviderActionRef; readonly abandonKind: "recovery-attempt"; readonly state: "abandoned"; readonly disposition: "abandoned"; readonly terminalEvidenceDigest: string }
);

type GenerationLossRecoverySource = {
  readonly kind: "generation-loss";
  readonly oldCustodyRef: null;
  readonly generationLossRef: GenerationLossRef;
  readonly lossKind: "generation-advance" | "context-advance";
  readonly oldProviderSessionRef: string;
  readonly newProviderSessionRef: string;
  readonly oldProviderGeneration: number;
  readonly newProviderGeneration: number;
  readonly oldContextRevision: number | null;
  readonly newContextRevision: number;
  readonly sourceBridgeRef: { readonly bridgeId: string; readonly bridgeRevision: number };
  readonly sourceCapabilityHash: string;
  readonly checkpointState: "absent" | "invalid" | "last-validated";
  readonly checkpointRef: CheckpointRef | null;
  readonly checkpointDigest: string | null;
  readonly lossEvidenceDigest: string;
};
export type LifecycleRecoverySourceV1 =
  | { readonly kind: "custody"; readonly custodyRef: CustodyRef }
  | GenerationLossRecoverySource;

export type LifecycleAcceptedSuspendedV1 = {
  readonly schemaVersion: 1;
  readonly kind: "accepted-suspended";
  readonly projectSessionId: string;
  readonly coordinationRunId: string;
  readonly action: "compact" | "rotate";
  readonly agentId: string;
  readonly taskId: string;
  readonly taskRevision: number;
  readonly lifecycle: "suspended";
  readonly custodyRef: CustodyRef;
  readonly actionRef: ProviderActionRef;
  readonly checkpointDigest: string;
  readonly openWorkSetDigest: string;
  readonly deliveryCutWatermark: number;
  readonly predecessorTurnSetDigest: string;
  readonly sourceProviderGeneration: number;
  readonly sourcePrincipalGeneration: number;
  readonly sourceBridgeGeneration: number;
  readonly targetProviderGeneration: number;
  readonly targetPrincipalGeneration: number;
  readonly targetBridgeGeneration: number;
  readonly acceptedReceiptDigest: string;
};

export type LifecycleCurrentStateV1 = {
  readonly schemaVersion: 1;
  readonly kind: "current-state";
  readonly agentId: string;
  readonly lifecycle: "starting" | "ready" | "busy" | "checkpointing" | "idle" | "suspended" | "archived";
  readonly contextState: "current" | "context-unreconciled";
  readonly principalGeneration: number;
  readonly providerSessionGeneration: number;
  readonly bridgeGeneration: number;
  readonly contextRevision: number;
  readonly currentSource: LifecycleCustodyRowV1 | LifecycleGenerationLossRowV1 | null;
  readonly stateDigest: string;
};

type AgentLifecycleRecoveryCommon = {
  readonly kind: "agent-lifecycle-recovery";
  readonly schemaVersion: 1;
  readonly projectSessionId: string;
  readonly coordinationRunId: string;
  readonly agentId: string;
  readonly source: LifecycleRecoverySourceV1;
  readonly expectedSessionRevision: number;
  readonly expectedSessionGeneration: number;
  readonly expectedRunRevision: number;
  readonly expectedAgentRevision: number;
  readonly expectedSourceRevision: number;
  readonly expectedPrincipalGeneration: number;
  readonly expectedProviderGeneration: number;
  readonly expectedBridgeGeneration: number;
  readonly expectedContextRevision: number;
  readonly bridgeOwnerKind: "chair" | "child";
  readonly expectedChairLeaseGeneration: number | null;
  readonly gateId: string;
  readonly expectedGateRevision: number;
  readonly expectedGateStatus: "approved";
};
export type AgentLifecycleRecoveryIntentV1 = AgentLifecycleRecoveryCommon & (
  | {
      readonly path: "fresh-rotate";
      readonly recoveryCapabilityId: string;
      readonly expectedRecoveryCapabilityRevision: number;
      readonly recoveryCapabilityHash: string;
      readonly replacementAdapterId: string;
      readonly replacementContractDigest: string;
      readonly replacementActionRef: ProviderActionRef;
      readonly checkpointRef: CheckpointRef;
      readonly checkpointDigest: string;
      readonly checkpointValidationReceiptDigest: string | null;
    }
  | {
      readonly path: "abandon";
      readonly reason: string;
      readonly directInputAttestationId: string;
      readonly destructiveConfirmationDigest: string;
    }
);

export type LifecycleRecoveryCheckpointValidateRequestV1 = {
  readonly schemaVersion: 1;
  readonly projectSessionId: string;
  readonly coordinationRunId: string;
  readonly agentId: string;
  readonly source: LifecycleRecoverySourceV1;
  readonly checkpointArtifactRef: { readonly path: string; readonly digest: string };
  readonly expectedSessionRevision: number;
  readonly expectedSessionGeneration: number;
  readonly expectedRunRevision: number;
  readonly expectedAgentRevision: number;
  readonly expectedSourceRevision: number;
  readonly gateId: string;
  readonly expectedGateRevision: number;
  readonly expectedGateStatus: "approved";
};

export type LifecycleRecoveryCheckpointValidationV1 =
  | {
      readonly schemaVersion: 1;
      readonly status: "validated";
      readonly source: LifecycleRecoverySourceV1;
      readonly checkpointRef: CheckpointRef;
      readonly checkpointDigest: string;
      readonly checkpointVectorDigest: string;
      readonly validationReceiptDigest: string;
    }
  | {
      readonly schemaVersion: 1;
      readonly status: "rejected";
      readonly reason: "artifact-missing" | "artifact-integrity-failed" | "checkpoint-invalid" | "source-mismatch" | "gate-not-current" | "state-changed";
      readonly evidenceDigest: string;
    };

export const LIFECYCLE_GENERATION_LOSS_REF_V1_CODEC = objectCodec({
  schemaVersion: literal(1),
  runId: id256,
  agentId: id256,
  generationLossId: id256,
  generationLossRevision: positive,
});

export const LIFECYCLE_BRIDGE_REF_V1_CODEC = objectCodec({
  bridgeId: id256,
  bridgeRevision: positive,
});

export const LIFECYCLE_CHECKPOINT_REF_V1_CODEC = objectCodec({
  checkpointId: id256,
  checkpointRevision: positive,
});

const custodyCommon = {
  schemaVersion: literal(1),
  sourceKind: literal("custody"),
  agentId: id256,
  custodyId: id256,
  custodyRevision: positive,
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  sourceProviderGeneration: positive,
  sourcePrincipalGeneration: positive,
  sourceBridgeGeneration: positive,
  targetProviderGeneration: positive,
  targetPrincipalGeneration: positive,
  targetBridgeGeneration: positive,
  checkpointDigest: sha256,
} as const;

const lifecycleCustodyRowBase = unionOf([
  objectCodec({
    ...custodyCommon,
    state: enumeration(["awaiting-boundary", "prepared", "dispatched", "accepted", "ambiguous"]),
    disposition: literal(null),
    terminalEvidenceDigest: literal(null),
  }),
  objectCodec({
    ...custodyCommon,
    state: enumeration(["provider-terminal", "committing"]),
    disposition: literal(null),
    terminalEvidenceDigest: sha256,
  }),
  objectCodec({
    ...custodyCommon,
    state: literal("finalized"),
    disposition: enumeration(["adopted", "no-effect", "quarantined", "superseded", "abandoned"]),
    terminalEvidenceDigest: sha256,
  }),
]);

export const LIFECYCLE_CUSTODY_ROW_V1_CODEC: Codec<LifecycleCustodyRowV1> = defineCodec(
  { ...lifecycleCustodyRowBase.schema, "x-lifecycleCustodyCorrelated": true },
  {
    schemaVersion: 1,
    sourceKind: "custody",
    agentId: "agent_01",
    custodyId: "custody_01",
    custodyRevision: 1,
    actionRef: { adapterId: "agy", actionId: "action_01" },
    state: "awaiting-boundary",
    disposition: null,
    sourceProviderGeneration: 1,
    sourcePrincipalGeneration: 1,
    sourceBridgeGeneration: 1,
    targetProviderGeneration: 2,
    targetPrincipalGeneration: 2,
    targetBridgeGeneration: 2,
    checkpointDigest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    terminalEvidenceDigest: null,
  } as LifecycleCustodyRowV1,
  (value, path) => {
    const row = lifecycleCustodyRowBase.parse(value, path) as unknown as LifecycleCustodyRowV1;
    if (row.targetProviderGeneration !== row.sourceProviderGeneration + 1) {
      throw new TypeError(`${path}.targetProviderGeneration must be source high-water plus one`);
    }
    if (row.targetPrincipalGeneration !== row.sourcePrincipalGeneration + 1) {
      throw new TypeError(`${path}.targetPrincipalGeneration must be source high-water plus one`);
    }
    if (row.targetBridgeGeneration !== row.sourceBridgeGeneration + 1) {
      throw new TypeError(`${path}.targetBridgeGeneration must be source high-water plus one`);
    }
    return row;
  },
);

const generationLossCommon = {
  schemaVersion: literal(1),
  sourceKind: literal("generation-loss"),
  agentId: id256,
  generationLossId: id256,
  generationLossRevision: positive,
  lossKind: enumeration(["generation-advance", "context-advance"]),
  oldProviderGeneration: positive,
  newProviderGeneration: positive,
  oldContextRevision: nullable(nonnegative),
  newContextRevision: nonnegative,
  checkpointState: enumeration(["absent", "invalid", "last-validated"]),
  checkpointDigest: nullableDigest,
  lossEvidenceDigest: sha256,
} as const;

const lifecycleGenerationLossRowBase = unionOf([
  objectCodec({
    ...generationLossCommon,
    recoveryActionRef: literal(null),
    abandonKind: literal("none"),
    state: literal("open"),
    disposition: literal(null),
    terminalEvidenceDigest: literal(null),
  }),
  objectCodec({
    ...generationLossCommon,
    recoveryActionRef: PROVIDER_ACTION_REF_V1_CODEC,
    abandonKind: literal("none"),
    state: literal("recovery-in-progress"),
    disposition: literal(null),
    terminalEvidenceDigest: literal(null),
  }),
  objectCodec({
    ...generationLossCommon,
    recoveryActionRef: PROVIDER_ACTION_REF_V1_CODEC,
    abandonKind: literal("none"),
    state: literal("recovered-adopted"),
    disposition: literal("recovered-adopted"),
    terminalEvidenceDigest: sha256,
  }),
  objectCodec({
    ...generationLossCommon,
    recoveryActionRef: literal(null),
    abandonKind: literal("direct-open"),
    state: literal("abandoned"),
    disposition: literal("abandoned"),
    terminalEvidenceDigest: sha256,
  }),
  objectCodec({
    ...generationLossCommon,
    recoveryActionRef: PROVIDER_ACTION_REF_V1_CODEC,
    abandonKind: literal("recovery-attempt"),
    state: literal("abandoned"),
    disposition: literal("abandoned"),
    terminalEvidenceDigest: sha256,
  }),
]);

function assertGenerationLoss(
  row: LifecycleGenerationLossRowV1,
  path: string,
): void {
  if (row.checkpointState === "last-validated") {
    if (row.checkpointDigest === null) throw new TypeError(`${path}.checkpointDigest must identify the last validated checkpoint`);
  } else if (row.checkpointDigest !== null) {
    throw new TypeError(`${path}.checkpointDigest must be null for an absent or invalid checkpoint`);
  }
  if (row.lossKind === "generation-advance") {
    if (row.newProviderGeneration <= row.oldProviderGeneration) {
      throw new TypeError(`${path}.newProviderGeneration must advance`);
    }
    return;
  }
  if (
    row.newProviderGeneration !== row.oldProviderGeneration ||
    row.oldContextRevision === null ||
    row.newContextRevision <= row.oldContextRevision
  ) {
    throw new TypeError(`${path} context revision must strictly advance within one provider generation`);
  }
}

export const LIFECYCLE_GENERATION_LOSS_ROW_V1_CODEC: Codec<LifecycleGenerationLossRowV1> = defineCodec(
  { ...lifecycleGenerationLossRowBase.schema, "x-lifecycleGenerationLossCorrelated": true },
  {
    schemaVersion: 1,
    sourceKind: "generation-loss",
    agentId: "agent_01",
    generationLossId: "loss_01",
    generationLossRevision: 1,
    lossKind: "generation-advance",
    recoveryActionRef: null,
    abandonKind: "none",
    state: "open",
    disposition: null,
    oldProviderGeneration: 1,
    newProviderGeneration: 2,
    oldContextRevision: 9,
    newContextRevision: 0,
    checkpointState: "absent",
    checkpointDigest: null,
    lossEvidenceDigest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    terminalEvidenceDigest: null,
  } as LifecycleGenerationLossRowV1,
  (value, path) => {
    const row = lifecycleGenerationLossRowBase.parse(value, path) as unknown as LifecycleGenerationLossRowV1;
    assertGenerationLoss(row, path);
    return row;
  },
);

const generationLossRecoverySourceBase = objectCodec({
  kind: literal("generation-loss"),
  oldCustodyRef: literal(null),
  generationLossRef: LIFECYCLE_GENERATION_LOSS_REF_V1_CODEC,
  lossKind: enumeration(["generation-advance", "context-advance"]),
  oldProviderSessionRef: id256,
  newProviderSessionRef: id256,
  oldProviderGeneration: positive,
  newProviderGeneration: positive,
  oldContextRevision: nullable(nonnegative),
  newContextRevision: nonnegative,
  sourceBridgeRef: LIFECYCLE_BRIDGE_REF_V1_CODEC,
  sourceCapabilityHash: sha256,
  checkpointState: enumeration(["absent", "invalid", "last-validated"]),
  checkpointRef: nullable(LIFECYCLE_CHECKPOINT_REF_V1_CODEC),
  checkpointDigest: nullableDigest,
  lossEvidenceDigest: sha256,
});

const generationLossRecoverySourceCodec: Codec<GenerationLossRecoverySource> = defineCodec(
  { ...generationLossRecoverySourceBase.schema, "x-lifecycleRecoverySourceCorrelated": true },
  {
    kind: "generation-loss",
    oldCustodyRef: null,
    generationLossRef: LIFECYCLE_GENERATION_LOSS_REF_V1_CODEC.example,
    lossKind: "generation-advance",
    oldProviderSessionRef: "provider_session_01",
    newProviderSessionRef: "provider_session_02",
    oldProviderGeneration: 1,
    newProviderGeneration: 2,
    oldContextRevision: 9,
    newContextRevision: 0,
    sourceBridgeRef: LIFECYCLE_BRIDGE_REF_V1_CODEC.example,
    sourceCapabilityHash: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    checkpointState: "absent",
    checkpointRef: null,
    checkpointDigest: null,
    lossEvidenceDigest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  } as GenerationLossRecoverySource,
  (value, path) => {
    const source = generationLossRecoverySourceBase.parse(value, path) as unknown as GenerationLossRecoverySource;
    if (source.generationLossRef.agentId.length === 0) throw new TypeError(`${path}.generationLossRef.agentId is required`);
    if (source.checkpointState === "last-validated") {
      if (source.checkpointRef === null || source.checkpointDigest === null) {
        throw new TypeError(`${path} last-validated checkpoint ref and digest must both be non-null`);
      }
    } else if (source.checkpointRef !== null || source.checkpointDigest !== null) {
      throw new TypeError(`${path} absent or invalid checkpoint ref and digest must both be null`);
    }
    if (source.lossKind === "generation-advance") {
      if (source.newProviderGeneration <= source.oldProviderGeneration) {
        throw new TypeError(`${path}.newProviderGeneration must advance`);
      }
    } else if (
      source.newProviderGeneration !== source.oldProviderGeneration ||
      source.oldContextRevision === null ||
      source.newContextRevision <= source.oldContextRevision
    ) {
      throw new TypeError(`${path} context revision must strictly advance within one provider generation`);
    }
    return source;
  },
);

export const LIFECYCLE_RECOVERY_SOURCE_V1_CODEC = unionOf([
  objectCodec({ kind: literal("custody"), custodyRef: LIFECYCLE_CUSTODY_REF_V1_CODEC }),
  generationLossRecoverySourceCodec,
]) as unknown as Codec<LifecycleRecoverySourceV1>;

const lifecycleRecoveryCheckpointValidateRequestBaseCodec = objectCodec({
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  agentId: id256,
  source: LIFECYCLE_RECOVERY_SOURCE_V1_CODEC,
  checkpointArtifactRef: objectCodec({ path: relativePath, digest: sha256 }),
  expectedSessionRevision: positive,
  expectedSessionGeneration: positive,
  expectedRunRevision: positive,
  expectedAgentRevision: positive,
  expectedSourceRevision: positive,
  gateId: id256,
  expectedGateRevision: positive,
  expectedGateStatus: literal("approved"),
});
export const LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATE_REQUEST_V1_CODEC: Codec<LifecycleRecoveryCheckpointValidateRequestV1> = defineCodec(
  { ...lifecycleRecoveryCheckpointValidateRequestBaseCodec.schema, "x-lifecycleCheckpointValidateCorrelated": true },
  {
    ...lifecycleRecoveryCheckpointValidateRequestBaseCodec.example,
    coordinationRunId: "run_01",
    agentId: "agent_01",
    source: { kind: "custody", custodyRef: { ...LIFECYCLE_CUSTODY_REF_V1_CODEC.example, runId: "run_01", agentId: "agent_01" } },
    expectedSourceRevision: LIFECYCLE_CUSTODY_REF_V1_CODEC.example.custodyRevision,
  } as unknown as LifecycleRecoveryCheckpointValidateRequestV1,
  (value, path) => {
    const request = lifecycleRecoveryCheckpointValidateRequestBaseCodec.parse(value, path) as unknown as LifecycleRecoveryCheckpointValidateRequestV1;
    const sourceRunId = request.source.kind === "custody" ? request.source.custodyRef.runId : request.source.generationLossRef.runId;
    const sourceAgentId = request.source.kind === "custody" ? request.source.custodyRef.agentId : request.source.generationLossRef.agentId;
    const sourceRevision = request.source.kind === "custody"
      ? request.source.custodyRef.custodyRevision
      : request.source.generationLossRef.generationLossRevision;
    if (sourceRunId !== request.coordinationRunId || sourceAgentId !== request.agentId) {
      throw new TypeError(`${path}.source must bind the exact outer run and agent`);
    }
    if (sourceRevision !== request.expectedSourceRevision) {
      throw new TypeError(`${path}.expectedSourceRevision must equal the exact source revision`);
    }
    return request;
  },
);

export const LIFECYCLE_RECOVERY_CHECKPOINT_VALIDATION_V1_CODEC = unionOf([
  objectCodec({
    schemaVersion: literal(1),
    status: literal("validated"),
    source: LIFECYCLE_RECOVERY_SOURCE_V1_CODEC,
    checkpointRef: LIFECYCLE_CHECKPOINT_REF_V1_CODEC,
    checkpointDigest: sha256,
    checkpointVectorDigest: sha256,
    validationReceiptDigest: sha256,
  }),
  objectCodec({
    schemaVersion: literal(1),
    status: literal("rejected"),
    reason: enumeration(["artifact-missing", "artifact-integrity-failed", "checkpoint-invalid", "source-mismatch", "gate-not-current", "state-changed"]),
    evidenceDigest: sha256,
  }),
]) as unknown as Codec<LifecycleRecoveryCheckpointValidationV1>;

const lifecycleAcceptedSuspendedBaseCodec = objectCodec({
  schemaVersion: literal(1),
  kind: literal("accepted-suspended"),
  projectSessionId: id256,
  coordinationRunId: id256,
  action: enumeration(["compact", "rotate"]),
  agentId: id256,
  taskId: id256,
  taskRevision: positive,
  lifecycle: literal("suspended"),
  custodyRef: LIFECYCLE_CUSTODY_REF_V1_CODEC,
  actionRef: PROVIDER_ACTION_REF_V1_CODEC,
  checkpointDigest: sha256,
  openWorkSetDigest: sha256,
  deliveryCutWatermark: nonnegative,
  predecessorTurnSetDigest: sha256,
  sourceProviderGeneration: positive,
  sourcePrincipalGeneration: positive,
  sourceBridgeGeneration: positive,
  targetProviderGeneration: positive,
  targetPrincipalGeneration: positive,
  targetBridgeGeneration: positive,
  acceptedReceiptDigest: sha256,
});
export const LIFECYCLE_ACCEPTED_SUSPENDED_V1_CODEC: Codec<LifecycleAcceptedSuspendedV1> = defineCodec(
  { ...lifecycleAcceptedSuspendedBaseCodec.schema, "x-lifecycleAcceptedSuspendedCorrelated": true },
  {
    ...lifecycleAcceptedSuspendedBaseCodec.example,
    projectSessionId: "ps_01",
    coordinationRunId: "run_01",
    agentId: "agent_01",
    custodyRef: { ...LIFECYCLE_CUSTODY_REF_V1_CODEC.example, runId: "run_01", agentId: "agent_01" },
    sourceProviderGeneration: 1,
    targetProviderGeneration: 2,
    sourcePrincipalGeneration: 1,
    targetPrincipalGeneration: 2,
    sourceBridgeGeneration: 1,
    targetBridgeGeneration: 2,
  } as unknown as LifecycleAcceptedSuspendedV1,
  (value, path) => {
    const receipt = lifecycleAcceptedSuspendedBaseCodec.parse(value, path) as unknown as LifecycleAcceptedSuspendedV1;
    if (receipt.custodyRef.runId !== receipt.coordinationRunId || receipt.custodyRef.agentId !== receipt.agentId) {
      throw new TypeError(`${path}.custodyRef must bind the exact outer run and agent`);
    }
    for (const [source, target] of [
      [receipt.sourceProviderGeneration, receipt.targetProviderGeneration],
      [receipt.sourcePrincipalGeneration, receipt.targetPrincipalGeneration],
      [receipt.sourceBridgeGeneration, receipt.targetBridgeGeneration],
    ] as const) {
      if (target !== source + 1) throw new TypeError(`${path} target generation must be source generation plus one`);
    }
    return receipt;
  },
);

const lifecycleSourceCodec = nullable(unionOf([
  LIFECYCLE_CUSTODY_ROW_V1_CODEC,
  LIFECYCLE_GENERATION_LOSS_ROW_V1_CODEC,
]));

const lifecycleCurrentStateBaseCodec = objectCodec({
  schemaVersion: literal(1),
  kind: literal("current-state"),
  agentId: id256,
  lifecycle: enumeration(["starting", "ready", "busy", "checkpointing", "idle", "suspended", "archived"]),
  contextState: enumeration(["current", "context-unreconciled"]),
  principalGeneration: positive,
  providerSessionGeneration: positive,
  bridgeGeneration: positive,
  contextRevision: nonnegative,
  currentSource: lifecycleSourceCodec,
  stateDigest: sha256,
});
export const LIFECYCLE_CURRENT_STATE_V1_CODEC: Codec<LifecycleCurrentStateV1> = defineCodec(
  { ...lifecycleCurrentStateBaseCodec.schema, "x-lifecycleCurrentStateCorrelated": true },
  lifecycleCurrentStateBaseCodec.example as unknown as LifecycleCurrentStateV1,
  (value, path) => {
    const current = lifecycleCurrentStateBaseCodec.parse(value, path) as unknown as LifecycleCurrentStateV1;
    const source = current.currentSource;
    if (source === null) {
      if (current.contextState !== "current") throw new TypeError(`${path}.contextState requires a current lifecycle source`);
      return current;
    }
    if (source.agentId !== current.agentId) throw new TypeError(`${path}.currentSource agent must equal outer agent`);
    if (source.sourceKind === "generation-loss") {
      if (
        current.providerSessionGeneration !== source.newProviderGeneration ||
        current.contextRevision !== source.newContextRevision ||
        current.contextState !== "context-unreconciled" ||
        current.lifecycle !== "suspended"
      ) {
        throw new TypeError(`${path} generation-loss source must equality-bind provider generation, context revision and suspended unreconciled state`);
      }
      return current;
    }
    const adopted = source.state === "finalized" && source.disposition === "adopted";
    const providerGeneration = adopted ? source.targetProviderGeneration : source.sourceProviderGeneration;
    const principalGeneration = adopted ? source.targetPrincipalGeneration : source.sourcePrincipalGeneration;
    const bridgeGeneration = adopted ? source.targetBridgeGeneration : source.sourceBridgeGeneration;
    if (
      current.providerSessionGeneration !== providerGeneration ||
      current.principalGeneration !== principalGeneration ||
      current.bridgeGeneration !== bridgeGeneration ||
      current.contextState !== "current"
    ) {
      throw new TypeError(`${path} custody source must equality-bind current provider/principal/bridge generations and contextState`);
    }
    if (source.state !== "finalized" && current.lifecycle !== "suspended") {
      throw new TypeError(`${path} nonfinal custody requires suspended lifecycle`);
    }
    return current;
  },
);

const operatorRecoveryCommon = {
  kind: literal("agent-lifecycle-recovery"),
  schemaVersion: literal(1),
  projectSessionId: id256,
  coordinationRunId: id256,
  agentId: id256,
  source: LIFECYCLE_RECOVERY_SOURCE_V1_CODEC,
  expectedSessionRevision: positive,
  expectedSessionGeneration: positive,
  expectedRunRevision: positive,
  expectedAgentRevision: positive,
  expectedSourceRevision: positive,
  expectedPrincipalGeneration: positive,
  expectedProviderGeneration: positive,
  expectedBridgeGeneration: positive,
  expectedContextRevision: nonnegative,
  bridgeOwnerKind: enumeration(["chair", "child"]),
  expectedChairLeaseGeneration: nullable(positive),
  gateId: id256,
  expectedGateRevision: positive,
  expectedGateStatus: literal("approved"),
} as const;

const agentLifecycleRecoveryIntentBase = unionOf([
  objectCodec({
    ...operatorRecoveryCommon,
    path: literal("fresh-rotate"),
    recoveryCapabilityId: id256,
    expectedRecoveryCapabilityRevision: positive,
    recoveryCapabilityHash: sha256,
    replacementAdapterId: id256,
    replacementContractDigest: sha256,
    replacementActionRef: PROVIDER_ACTION_REF_V1_CODEC,
    checkpointRef: LIFECYCLE_CHECKPOINT_REF_V1_CODEC,
    checkpointDigest: sha256,
    checkpointValidationReceiptDigest: nullableDigest,
  }),
  objectCodec({
    ...operatorRecoveryCommon,
    path: literal("abandon"),
    reason: boundedString({ maxBytes: 4096, example: "Human confirmed lifecycle retirement." }),
    directInputAttestationId: id256,
    destructiveConfirmationDigest: sha256,
  }),
]);

export const AGENT_LIFECYCLE_RECOVERY_INTENT_V1_CODEC: Codec<AgentLifecycleRecoveryIntentV1> = defineCodec(
  { ...agentLifecycleRecoveryIntentBase.schema, "x-agentLifecycleRecoveryIntentCorrelated": true },
  {
    kind: "agent-lifecycle-recovery",
    schemaVersion: 1,
    path: "abandon",
    projectSessionId: "ps_01",
    coordinationRunId: "run_01",
    agentId: "agent_01",
    source: LIFECYCLE_RECOVERY_SOURCE_V1_CODEC.example,
    expectedSessionRevision: 1,
    expectedSessionGeneration: 1,
    expectedRunRevision: 1,
    expectedAgentRevision: 1,
    expectedSourceRevision: 1,
    expectedPrincipalGeneration: 1,
    expectedProviderGeneration: 1,
    expectedBridgeGeneration: 1,
    expectedContextRevision: 0,
    bridgeOwnerKind: "child",
    expectedChairLeaseGeneration: null,
    gateId: "gate_01",
    expectedGateRevision: 1,
    expectedGateStatus: "approved",
    reason: "Human confirmed lifecycle retirement.",
    directInputAttestationId: "attestation_01",
    destructiveConfirmationDigest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  } as AgentLifecycleRecoveryIntentV1,
  (value, path) => {
    const intent = agentLifecycleRecoveryIntentBase.parse(value, path) as unknown as AgentLifecycleRecoveryIntentV1;
    if ((intent.bridgeOwnerKind === "chair") !== (intent.expectedChairLeaseGeneration !== null)) {
      throw new TypeError(`${path}.expectedChairLeaseGeneration must be non-null exactly for a chair bridge owner`);
    }
    const sourceAgentId = intent.source.kind === "custody"
      ? intent.source.custodyRef.agentId
      : intent.source.generationLossRef.agentId;
    const sourceRunId = intent.source.kind === "custody"
      ? intent.source.custodyRef.runId
      : intent.source.generationLossRef.runId;
    if (sourceAgentId !== intent.agentId || sourceRunId !== intent.coordinationRunId) {
      throw new TypeError(`${path}.source must bind the same run and agent`);
    }
    const sourceRevision = intent.source.kind === "custody"
      ? intent.source.custodyRef.custodyRevision
      : intent.source.generationLossRef.generationLossRevision;
    if (intent.expectedSourceRevision !== sourceRevision) {
      throw new TypeError(`${path}.expectedSourceRevision must equal the exact recovery source revision`);
    }
    if (intent.source.kind === "generation-loss" && (
      intent.expectedProviderGeneration !== intent.source.newProviderGeneration ||
      intent.expectedContextRevision !== intent.source.newContextRevision
    )) {
      throw new TypeError(`${path} expected provider generation and context revision must equal the observed generation loss`);
    }
    if (intent.source.kind === "custody" && intent.expectedContextRevision < 0) {
      throw new TypeError(`${path}.expectedContextRevision must be current`);
    }
    if (intent.path === "fresh-rotate") {
      if (intent.replacementActionRef.adapterId !== intent.replacementAdapterId) {
        throw new TypeError(`${path}.replacementActionRef adapter must match replacementAdapterId`);
      }
      if (intent.source.kind === "generation-loss") {
        if (intent.source.checkpointState === "last-validated") {
          if (
            intent.source.checkpointRef?.checkpointId !== intent.checkpointRef.checkpointId ||
            intent.source.checkpointRef.checkpointRevision !== intent.checkpointRef.checkpointRevision ||
            intent.source.checkpointDigest !== intent.checkpointDigest ||
            intent.checkpointValidationReceiptDigest !== null
          ) {
            throw new TypeError(`${path} must bind the exact last-validated checkpoint without a validation receipt`);
          }
        } else if (intent.checkpointValidationReceiptDigest === null) {
          throw new TypeError(`${path}.checkpointValidationReceiptDigest is required for an absent or invalid loss checkpoint`);
        }
      }
    }
    return intent;
  },
);
