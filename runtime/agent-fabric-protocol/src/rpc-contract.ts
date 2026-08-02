import type { BaselineOperationInputMap, BaselineOperationResultMap } from "./baseline-contracts.js";
import type {
  ArtifactContentReadRequest,
  ArtifactContentReadResult,
  EvidenceArtifactRegistration,
  EvidencePublishRequest,
} from "./artifacts.js";
import type { ProtocolFeature } from "./features.js";
import type {
  ScopedGate,
  ScopedGateCheckRequest,
  ScopedGateCheckResult,
  ScopedGateCreateRequest,
  ScopedGateReadRequest,
  ScopedGateReadResult,
  ScopedGateResolveRequest,
} from "./gates.js";
import type { HerdrSteerDispatchRequest, HerdrSteerDispatchResult } from "./herdr-control.js";
import type {
  Intake,
  IntakeDraft,
  IntakeDraftCreateRequest,
  IntakeReadRequest,
  IntakeRevisionRequest,
  IntakeSubmission,
} from "./intake.js";
import type { MembershipBindRequest, MembershipBindResult } from "./membership.js";
import type {
  LifecycleRecoveryCheckpointValidateRequestV1,
  LifecycleRecoveryCheckpointValidationV1,
} from "./lifecycle.js";
import { FABRIC_OPERATIONS, type FabricOperation } from "./operations.js";
import type {
  ChairTakeoverRequest,
  IntegrationInputAttestationRequest,
  OperatorInputAttestation,
} from "./operator.js";
import type {
  OperatorActionCommitRequest,
  OperatorActionPreview,
  OperatorActionPreviewRequest,
  OperatorActionReceipt,
  OperatorActionReconcileRequest,
  OperatorActionStatus,
  OperatorActionStatusRequest,
} from "./operator-actions.js";
import type {
  MessageBodyReadRequest,
  MessageBodyReadResult,
  GitRepositoryReadRequest,
  GitRepositoryReadResult,
  OperatorAttachRequest,
  OperatorAttachment,
  OperatorDetachRequest,
  OperatorHeartbeatRequest,
  OperatorProjectionSnapshot,
  OperatorDetailReadRequest,
  OperatorDetailReadResult,
  OperatorViewPageRequest,
  OperatorViewPageResult,
  RunProjectionPageRequest,
  RunProjectionPageResult,
  ProjectDiscoveryRequest,
  ProjectDiscoveryResult,
  ProjectionPageRequest,
  ProjectionPageResult,
  ProjectionEventsRequest,
  ProjectionEventsResult,
  ProjectionSnapshotRequest,
} from "./projection.js";
import type {
  ProjectSession,
  ProjectSessionCloseRequest,
  ProjectSessionCreateRequest,
  ProjectSessionGetRequest,
  ProjectSessionLaunchPacketPreparation,
  ProjectSessionLaunchPacketPrepareRequest,
  ProjectSessionLaunchPrepareRequest,
  ProjectSessionTransitionRequest,
} from "./project-session.js";
import type {
  AgentId,
  CallbackId,
  CommandId,
  JsonValue,
  IntegrationId,
  OperatorId,
  ProjectId,
  ProjectSessionId,
  Sha256Digest,
  Timestamp,
} from "./primitives.js";
import type {
  ResultDelivery,
  ResultDeliveryAbandonRequest,
  ResultDeliveryClaimRequest,
  ResultDeliveryConsumeRequest,
  ResultDeliveryProviderAcceptRequest,
  ResultDeliveryReassignRequest,
  ResultDeliveryRetryRequest,
  TaskCompleteWithReply,
  TaskRequest,
} from "./request-result.js";
import type {
  ResourceReconcileRequest,
  ResourceReleaseRequest,
  ResourceReservation,
  ResourceReservationRequest,
} from "./resources.js";
import type { RunPlanDeclaration, RunPlanDeclareRequest } from "./run-plan.js";
import type {
  WorkstreamCreateRequest,
  WorkstreamProjection,
  WorkstreamSettleRequest,
} from "./workstreams.js";
import type {
  ReviewTargetPreparationAcceptedV1,
  ReviewTargetPreparationReadRequestV1,
  ReviewTargetPreparationReadErrorV1,
  ReviewTargetPreparationReadV1,
  ReviewTargetPrepareV1,
  ReviewTargetRebindReceiptV1,
  ReviewTargetRebindV1,
} from "./provider-review-core.js";
import type {
  ReviewEvidenceAnnotationAppendRequestV1,
  ReviewEvidenceAnnotationCurrentReadRequestV1,
  ReviewEvidenceAnnotationCurrentReadResultV1,
  ReviewEvidenceAnnotationV1,
  ReviewEvidenceListRequestV1,
  ReviewEvidenceListResultV1,
  ReviewEvidenceReadRequestV1,
  ReviewEvidenceReadV1,
  ReviewFindingPageReadRequestV1,
  ReviewFindingPageReadResultV1,
  ReviewReadErrorV1,
} from "./provider-review-evidence.js";
import type { ReviewCompletionReadRequestV1, ReviewCompletionV1 } from "./provider-review-completion.js";
import type {
  ProviderRouteIntegrityRecoveryProjectionV1,
  ProviderRouteIntegrityRecoveryReadErrorV1,
  ProviderRouteIntegrityRecoveryReadRequestV1,
} from "./provider-review-recovery.js";
import type {
  ProviderContextPressureReadRequestV1,
  ProviderContextPressureReadV1,
} from "./route-lineage.js";
import type {
  TopologyWaveAppendReceiptV1,
  TopologyWaveAppendRequestV1,
  TopologyWaveCurrentReadRequestV1,
  TopologyWaveCurrentReadV1,
  TopologyWaveListRequestV1,
  TopologyWaveListV1,
} from "./topology-evaluation.js";

export type ProtocolLimits = {
  maximumFrameBytes: number;
  maximumPendingCalls: number;
  maximumInFlightPerConnection: number;
  idleTimeoutMs: number;
  requestTimeoutMs: number;
};

export const PROTOCOL_LIMITS: Readonly<ProtocolLimits> = Object.freeze({
  maximumFrameBytes: 1_048_576,
  maximumPendingCalls: 32,
  maximumInFlightPerConnection: 16,
  idleTimeoutMs: 300_000,
  requestTimeoutMs: 30_000,
});

export type ProtocolPrincipal =
  | {
      kind: "operator";
      operatorId: OperatorId;
      projectId: ProjectId;
      projectAuthorityGeneration: number;
      principalGeneration: number;
    }
  | {
      kind: "agent";
      agentId: AgentId;
      projectSessionId: ProjectSessionId;
      runId: string;
      principalGeneration: number;
    }
  | {
      kind: "integration";
      integrationId: IntegrationId;
      projectId: ProjectId;
      projectSessionId: ProjectSessionId;
      runId: string;
      principalGeneration: number;
      providerId: string;
      providerSessionRef: string;
    };

export type ProtocolInitializeRequest = {
  protocolVersion: 1;
  client: { name: string; version: string };
  authentication: {
    scheme: "capability";
    credential: string;
    clientNonce: string;
  };
  expectedPrincipalKind: ProtocolPrincipal["kind"];
  requiredFeatures: readonly string[];
  optionalFeatures: readonly string[];
};

export type ProtocolInitializeResult = {
  protocolVersion: 1;
  daemonVersion: string;
  daemonInstanceGeneration: number;
  principal: ProtocolPrincipal;
  clientNonce: string;
  connectionNonce: string;
  features: readonly ProtocolFeature[];
  allowedOperations: readonly FabricOperation[];
  limits: ProtocolLimits;
};

type ExtensionOperationInputMap = {
  [FABRIC_OPERATIONS.launchAttest]: { challengeResponse: string };
  [FABRIC_OPERATIONS.projectSessionCreate]: ProjectSessionCreateRequest;
  [FABRIC_OPERATIONS.projectSessionGet]: ProjectSessionGetRequest;
  [FABRIC_OPERATIONS.projectSessionTransition]: ProjectSessionTransitionRequest;
  [FABRIC_OPERATIONS.projectSessionClose]: ProjectSessionCloseRequest;
  [FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare]: ProjectSessionLaunchPacketPrepareRequest;
  [FABRIC_OPERATIONS.projectSessionLaunchPrepare]: ProjectSessionLaunchPrepareRequest;
  [FABRIC_OPERATIONS.membershipBind]: MembershipBindRequest;
  [FABRIC_OPERATIONS.operatorAttach]: OperatorAttachRequest;
  [FABRIC_OPERATIONS.operatorDetach]: OperatorDetachRequest;
  [FABRIC_OPERATIONS.operatorHeartbeat]: OperatorHeartbeatRequest;
  [FABRIC_OPERATIONS.integrationInputAttest]: IntegrationInputAttestationRequest;
  [FABRIC_OPERATIONS.intakeDraftCreate]: IntakeDraftCreateRequest;
  [FABRIC_OPERATIONS.intakeRead]: IntakeReadRequest;
  [FABRIC_OPERATIONS.intakeSubmit]: IntakeSubmission;
  [FABRIC_OPERATIONS.intakeRevise]: IntakeRevisionRequest;
  [FABRIC_OPERATIONS.scopedGateCreate]: ScopedGateCreateRequest;
  [FABRIC_OPERATIONS.scopedGateResolve]: ScopedGateResolveRequest;
  [FABRIC_OPERATIONS.scopedGateCheck]: ScopedGateCheckRequest;
  [FABRIC_OPERATIONS.scopedGateRead]: ScopedGateReadRequest;
  [FABRIC_OPERATIONS.resourceReserve]: ResourceReservationRequest;
  [FABRIC_OPERATIONS.resourceRelease]: ResourceReleaseRequest;
  [FABRIC_OPERATIONS.resourceReconcile]: ResourceReconcileRequest;
  [FABRIC_OPERATIONS.taskRequest]: TaskRequest;
  [FABRIC_OPERATIONS.taskCompleteWithReply]: TaskCompleteWithReply;
  [FABRIC_OPERATIONS.resultDeliveryClaim]: ResultDeliveryClaimRequest;
  [FABRIC_OPERATIONS.resultDeliveryProviderAccept]: ResultDeliveryProviderAcceptRequest;
  [FABRIC_OPERATIONS.resultDeliveryConsume]: ResultDeliveryConsumeRequest;
  [FABRIC_OPERATIONS.resultDeliveryRetry]: ResultDeliveryRetryRequest;
  [FABRIC_OPERATIONS.resultDeliveryReassign]: ResultDeliveryReassignRequest;
  [FABRIC_OPERATIONS.resultDeliveryAbandon]: ResultDeliveryAbandonRequest;
  [FABRIC_OPERATIONS.workstreamCreate]: WorkstreamCreateRequest;
  [FABRIC_OPERATIONS.workstreamSettle]: WorkstreamSettleRequest;
  [FABRIC_OPERATIONS.runPlanDeclare]: RunPlanDeclareRequest;
  [FABRIC_OPERATIONS.chairTakeover]: ChairTakeoverRequest;
  [FABRIC_OPERATIONS.projectDiscover]: ProjectDiscoveryRequest;
  [FABRIC_OPERATIONS.projectionSnapshot]: ProjectionSnapshotRequest;
  [FABRIC_OPERATIONS.projectionPage]: ProjectionPageRequest;
  [FABRIC_OPERATIONS.projectionEvents]: ProjectionEventsRequest;
  [FABRIC_OPERATIONS.projectionViewPage]: OperatorViewPageRequest;
  [FABRIC_OPERATIONS.projectionDetailRead]: OperatorDetailReadRequest;
  [FABRIC_OPERATIONS.projectionRunPage]: RunProjectionPageRequest;
  [FABRIC_OPERATIONS.operatorActionPreview]: OperatorActionPreviewRequest;
  [FABRIC_OPERATIONS.operatorActionCommit]: OperatorActionCommitRequest;
  [FABRIC_OPERATIONS.operatorActionStatus]: OperatorActionStatusRequest;
  [FABRIC_OPERATIONS.operatorActionReconcile]: OperatorActionReconcileRequest;
  [FABRIC_OPERATIONS.agentLifecycleRecoveryCheckpointValidate]: LifecycleRecoveryCheckpointValidateRequestV1;
  [FABRIC_OPERATIONS.messageBodyRead]: MessageBodyReadRequest;
  [FABRIC_OPERATIONS.operatorRepositoryRead]: GitRepositoryReadRequest;
  [FABRIC_OPERATIONS.evidencePublish]: EvidencePublishRequest;
  [FABRIC_OPERATIONS.operatorArtifactContentRead]: ArtifactContentReadRequest;
  [FABRIC_OPERATIONS.reviewTargetPrepare]: ReviewTargetPrepareV1;
  [FABRIC_OPERATIONS.reviewTargetPreparationRead]: ReviewTargetPreparationReadRequestV1;
  [FABRIC_OPERATIONS.reviewTargetRebind]: ReviewTargetRebindV1;
  [FABRIC_OPERATIONS.reviewEvidenceRead]: ReviewEvidenceReadRequestV1;
  [FABRIC_OPERATIONS.reviewEvidenceList]: ReviewEvidenceListRequestV1;
  [FABRIC_OPERATIONS.reviewEvidenceAnnotate]: ReviewEvidenceAnnotationAppendRequestV1;
  [FABRIC_OPERATIONS.reviewEvidenceAnnotationCurrentRead]: ReviewEvidenceAnnotationCurrentReadRequestV1;
  [FABRIC_OPERATIONS.reviewFindingPageRead]: ReviewFindingPageReadRequestV1;
  [FABRIC_OPERATIONS.reviewCompletionRead]: ReviewCompletionReadRequestV1;
  [FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead]: ProviderRouteIntegrityRecoveryReadRequestV1;
  [FABRIC_OPERATIONS.providerContextPressureRead]: ProviderContextPressureReadRequestV1;
  [FABRIC_OPERATIONS.herdrSteerDispatch]: HerdrSteerDispatchRequest;
  [FABRIC_OPERATIONS.topologyWaveAppend]: TopologyWaveAppendRequestV1;
  [FABRIC_OPERATIONS.topologyWaveCurrentRead]: TopologyWaveCurrentReadRequestV1;
  [FABRIC_OPERATIONS.topologyWaveList]: TopologyWaveListRequestV1;
};

export type OperationInputMap = BaselineOperationInputMap & ExtensionOperationInputMap;

export type ChairTakeoverResult = {
  projectSessionId: ProjectSessionId;
  sessionRevision: number;
  runRevision: number;
  chairAgentId: AgentId;
  chairGeneration: number;
};

export type TaskRequestCommit = {
  taskRevision: number;
  requestRevision: number;
  callbackId: CallbackId;
  callbackGeneration: number;
};

export type TaskCompletionCommit = {
  taskRevision: number;
  replyRevision: number;
  resultDelivery: ResultDelivery;
};

type ExtensionOperationResultMap = {
  [FABRIC_OPERATIONS.launchAttest]: { attested: true; challengeDigest: Sha256Digest };
  [FABRIC_OPERATIONS.projectSessionCreate]: ProjectSession;
  [FABRIC_OPERATIONS.projectSessionGet]: ProjectSession;
  [FABRIC_OPERATIONS.projectSessionTransition]: ProjectSession;
  [FABRIC_OPERATIONS.projectSessionClose]: ProjectSession;
  [FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare]: ProjectSessionLaunchPacketPreparation;
  [FABRIC_OPERATIONS.projectSessionLaunchPrepare]: OperatorActionPreview;
  [FABRIC_OPERATIONS.membershipBind]: MembershipBindResult;
  [FABRIC_OPERATIONS.operatorAttach]: OperatorAttachment;
  [FABRIC_OPERATIONS.operatorDetach]: { detached: true; revision: number };
  [FABRIC_OPERATIONS.operatorHeartbeat]: OperatorAttachment;
  [FABRIC_OPERATIONS.integrationInputAttest]: OperatorInputAttestation;
  [FABRIC_OPERATIONS.intakeDraftCreate]: IntakeDraft;
  [FABRIC_OPERATIONS.intakeRead]: Intake;
  [FABRIC_OPERATIONS.intakeSubmit]: Intake;
  [FABRIC_OPERATIONS.intakeRevise]: Intake;
  [FABRIC_OPERATIONS.scopedGateCreate]: ScopedGate;
  [FABRIC_OPERATIONS.scopedGateResolve]: ScopedGate;
  [FABRIC_OPERATIONS.scopedGateCheck]: ScopedGateCheckResult;
  [FABRIC_OPERATIONS.scopedGateRead]: ScopedGateReadResult;
  [FABRIC_OPERATIONS.resourceReserve]: ResourceReservation;
  [FABRIC_OPERATIONS.resourceRelease]: ResourceReservation;
  [FABRIC_OPERATIONS.resourceReconcile]: ResourceReservation;
  [FABRIC_OPERATIONS.taskRequest]: TaskRequestCommit;
  [FABRIC_OPERATIONS.taskCompleteWithReply]: TaskCompletionCommit;
  [FABRIC_OPERATIONS.resultDeliveryClaim]: ResultDelivery;
  [FABRIC_OPERATIONS.resultDeliveryProviderAccept]: ResultDelivery;
  [FABRIC_OPERATIONS.resultDeliveryConsume]: ResultDelivery;
  [FABRIC_OPERATIONS.resultDeliveryRetry]: ResultDelivery;
  [FABRIC_OPERATIONS.resultDeliveryReassign]: ResultDelivery;
  [FABRIC_OPERATIONS.resultDeliveryAbandon]: ResultDelivery;
  [FABRIC_OPERATIONS.workstreamCreate]: WorkstreamProjection;
  [FABRIC_OPERATIONS.workstreamSettle]: WorkstreamProjection;
  [FABRIC_OPERATIONS.runPlanDeclare]: RunPlanDeclaration;
  [FABRIC_OPERATIONS.chairTakeover]: ChairTakeoverResult;
  [FABRIC_OPERATIONS.projectDiscover]: ProjectDiscoveryResult;
  [FABRIC_OPERATIONS.projectionSnapshot]: OperatorProjectionSnapshot;
  [FABRIC_OPERATIONS.projectionPage]: ProjectionPageResult;
  [FABRIC_OPERATIONS.projectionEvents]: ProjectionEventsResult;
  [FABRIC_OPERATIONS.projectionViewPage]: OperatorViewPageResult;
  [FABRIC_OPERATIONS.projectionDetailRead]: OperatorDetailReadResult;
  [FABRIC_OPERATIONS.projectionRunPage]: RunProjectionPageResult;
  [FABRIC_OPERATIONS.operatorActionPreview]: OperatorActionPreview;
  [FABRIC_OPERATIONS.operatorActionCommit]: OperatorActionReceipt;
  [FABRIC_OPERATIONS.operatorActionStatus]: OperatorActionStatus;
  [FABRIC_OPERATIONS.operatorActionReconcile]: OperatorActionStatus;
  [FABRIC_OPERATIONS.agentLifecycleRecoveryCheckpointValidate]: LifecycleRecoveryCheckpointValidationV1;
  [FABRIC_OPERATIONS.messageBodyRead]: MessageBodyReadResult;
  [FABRIC_OPERATIONS.operatorRepositoryRead]: GitRepositoryReadResult;
  [FABRIC_OPERATIONS.evidencePublish]: EvidenceArtifactRegistration;
  [FABRIC_OPERATIONS.operatorArtifactContentRead]: ArtifactContentReadResult;
  [FABRIC_OPERATIONS.reviewTargetPrepare]: ReviewTargetPreparationAcceptedV1;
  [FABRIC_OPERATIONS.reviewTargetPreparationRead]: ReviewTargetPreparationReadV1 | ReviewTargetPreparationReadErrorV1;
  [FABRIC_OPERATIONS.reviewTargetRebind]: ReviewTargetRebindReceiptV1;
  [FABRIC_OPERATIONS.reviewEvidenceRead]: ReviewEvidenceReadV1 | ReviewReadErrorV1;
  [FABRIC_OPERATIONS.reviewEvidenceList]: ReviewEvidenceListResultV1 | ReviewReadErrorV1;
  [FABRIC_OPERATIONS.reviewEvidenceAnnotate]: ReviewEvidenceAnnotationV1 | ReviewReadErrorV1;
  [FABRIC_OPERATIONS.reviewEvidenceAnnotationCurrentRead]: ReviewEvidenceAnnotationCurrentReadResultV1 | ReviewReadErrorV1;
  [FABRIC_OPERATIONS.reviewFindingPageRead]: ReviewFindingPageReadResultV1 | ReviewReadErrorV1;
  [FABRIC_OPERATIONS.reviewCompletionRead]: ReviewCompletionV1 | ReviewReadErrorV1;
  [FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead]: ProviderRouteIntegrityRecoveryProjectionV1 | ProviderRouteIntegrityRecoveryReadErrorV1;
  [FABRIC_OPERATIONS.providerContextPressureRead]: ProviderContextPressureReadV1;
  [FABRIC_OPERATIONS.herdrSteerDispatch]: HerdrSteerDispatchResult;
  [FABRIC_OPERATIONS.topologyWaveAppend]: TopologyWaveAppendReceiptV1;
  [FABRIC_OPERATIONS.topologyWaveCurrentRead]: TopologyWaveCurrentReadV1;
  [FABRIC_OPERATIONS.topologyWaveList]: TopologyWaveListV1;
};

export type OperationResultMap = BaselineOperationResultMap & ExtensionOperationResultMap;

export type ProtocolOperation = keyof OperationInputMap & keyof OperationResultMap & FabricOperation;

export type ProtocolRequest<Operation extends ProtocolOperation = ProtocolOperation> =
  Operation extends ProtocolOperation
    ? { id: string; operation: Operation; input: OperationInputMap[Operation] }
    : never;

export const PROTOCOL_ERROR_CODES = [
  "PROTOCOL_INVALID",
  "PROTOCOL_INCOMPATIBLE",
  "PROTOCOL_UNSUPPORTED",
  "FEATURE_UNAVAILABLE",
  "AUTHENTICATION_FAILED",
  "AUTHORITY_WIDENING",
  "ARTIFACT_DIGEST_INVALID",
  "ARTIFACT_PATH_FORBIDDEN",
  "ADAPTER_ARTIFACT_MISSING",
  "ADAPTER_COMPATIBILITY_INVALID",
  "ADAPTER_DISABLED",
  "ADAPTER_MODEL_REQUIRED",
  "ADAPTER_FAMILY_FORBIDDEN",
  "BARRIER_PRECONDITION_FAILED",
  "BUDGET_EXCEEDED",
  "CAPABILITY_FORBIDDEN",
  "CAPABILITY_UNAVAILABLE",
  "CAPABILITY_EXPIRED",
  "CAPABILITY_REVOKED",
  "CONFIG_UNTRUSTED_FIELD",
  "CONFIG_WIDENING_FORBIDDEN",
  "ACTION_INPUT_CONFLICT",
  "DEDUPE_CONFLICT",
  "DELIVERY_ALREADY_RESOLVED",
  "DELIVERY_REASON_REQUIRED",
  "LEASE_NOT_EXPIRED",
  "LEASE_EXPIRED",
  "LEASE_QUARANTINED",
  "CHECKPOINT_INCOMPLETE",
  "CONTEXT_UNRECONCILED",
  "LIFECYCLE_PRECONDITION_FAILED",
  "MODEL_REQUIRED",
  "MODEL_NOT_ALLOWED",
  "MODEL_FAMILY_NOT_ALLOWED",
  "MESSAGE_RELATIONSHIP_FORBIDDEN",
  "MESSAGE_HOP_LIMIT_EXCEEDED",
  "MESSAGE_QUOTA_EXCEEDED",
  "NOT_FOUND",
  "PROVIDER_TURN_ACTIVE",
  "STALE_LEASE_GENERATION",
  "STALE_PRINCIPAL_GENERATION",
  "TASK_NOT_OWNER",
  "TASK_DEPENDENCY_BLOCKED",
  "TASK_SUBTREE_CONFLICT",
  "TASK_REVISION_CONFLICT",
  "TEAM_DEPTH_EXCEEDED",
  "STALE_TEAM_GENERATION",
  "BUDGET_USAGE_UNKNOWN",
  "WRITE_SCOPE_CONFLICT",
  "WRITE_SCOPE_RECOVERY_REQUIRED",
  "WRITE_SCOPE_QUARANTINED",
  "WRONG_PROJECT",
  "STALE_GENERATION",
  "STALE_REVISION",
  "GATE_BLOCKED",
  "RESOURCE_EXHAUSTED",
  "RESOURCE_USAGE_UNKNOWN",
  "OVERLOADED",
  "DEADLINE_EXCEEDED",
  "CONFLICT",
  "RECOVERY_REQUIRED",
  "PROJECTION_RESNAPSHOT_REQUIRED",
] as const;

export type ProtocolErrorCode = (typeof PROTOCOL_ERROR_CODES)[number];

export type ProtocolFailure = {
  code: ProtocolErrorCode;
  message: string;
  retryable: boolean;
  details?: JsonValue;
};

export type ProtocolResponse<Operation extends ProtocolOperation = ProtocolOperation> =
  Operation extends ProtocolOperation
    ?
      | { id: string; ok: true; operation: Operation; result: OperationResultMap[Operation] }
      | { id: string; ok: false; operation: Operation; error: ProtocolFailure }
    : never;

export type AuditedCommandReceipt = {
  commandId: CommandId;
  committedAt: Timestamp;
  beforeDigest: Sha256Digest;
  afterDigest: Sha256Digest;
};
