import type { ProtocolFeature } from "./features.js";
import type {
  ArtifactContentReadRequest,
  ArtifactContentReadResult,
  EvidenceArtifactRegistration,
  EvidencePublishRequest,
} from "./artifacts.js";
import type {
  OperationInputForPrincipal,
} from "./operation-codecs.js";
import type {
  ScopedGate,
  ScopedGateCheckRequest,
  ScopedGateCheckResult,
  ScopedGateCreateRequest,
  ScopedGateReadRequest,
  ScopedGateReadResult,
  ScopedGateResolveRequest,
} from "./gates.js";
import type {
  Intake,
  IntakeDraft,
  IntakeDraftCreateRequest,
  IntakeReadRequest,
  IntakeRevisionRequest,
  IntakeSubmission,
} from "./intake.js";
import type { MembershipBindRequest, MembershipBindResult } from "./membership.js";
import {
  FABRIC_OPERATIONS,
  OPERATION_REGISTRY,
  type BaselineOperation,
  type FabricOperation,
  type OperationPrincipalKind,
  type PrincipalOperation,
} from "./operations.js";
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
  ChairTakeoverResult,
  OperationInputMap,
  OperationResultMap,
  ProtocolPrincipal,
  TaskCompletionCommit,
  TaskRequestCommit,
} from "./rpc-contract.js";
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

export interface ProtocolRpcTransport {
  readonly features: readonly ProtocolFeature[];
  readonly principal: ProtocolPrincipal;
  readonly allowedOperations: ReadonlySet<FabricOperation>;
  call<Operation extends keyof OperationInputMap & FabricOperation>(
    operation: Operation,
    input: OperationInputMap[Operation],
  ): Promise<OperationResultMap[Operation]>;
  close(): Promise<void>;
}

export interface ProjectSessionClient {
  create?(input: ProjectSessionCreateRequest): Promise<ProjectSession>;
  get?(input: ProjectSessionGetRequest): Promise<ProjectSession>;
  transition?(input: ProjectSessionTransitionRequest): Promise<ProjectSession>;
  close?(input: ProjectSessionCloseRequest): Promise<ProjectSession>;
  prepareImplementation?(input: ProjectSessionLaunchPacketPrepareRequest): Promise<ProjectSessionLaunchPacketPreparation>;
  prepareLaunch?(input: ProjectSessionLaunchPrepareRequest): Promise<OperatorActionPreview>;
  bindMembership?(input: Extract<MembershipBindRequest, { origin: "operator" }>): Promise<MembershipBindResult>;
}

export interface BaselineFabricClient {
  call<Operation extends BaselineOperation>(
    operation: Operation,
    input: OperationInputMap[Operation],
  ): Promise<OperationResultMap[Operation]>;
}

export interface OperatorControlClient {
  attach(input: OperatorAttachRequest): Promise<OperatorAttachment>;
  detach(input: OperatorDetachRequest): Promise<{ detached: true; revision: number }>;
  heartbeat(input: OperatorHeartbeatRequest): Promise<OperatorAttachment>;
}

export interface IntakeClient {
  createDraft(input: IntakeDraftCreateRequest): Promise<IntakeDraft>;
  read(input: IntakeReadRequest): Promise<Intake>;
  submit(input: IntakeSubmission): Promise<Intake>;
  revise(input: Extract<IntakeRevisionRequest, { origin: "operator" }>): Promise<Intake>;
}

export interface ScopedGateClient {
  create(input: ScopedGateCreateRequest): Promise<ScopedGate>;
  resolve(input: ScopedGateResolveRequest): Promise<ScopedGate>;
  check(input: ScopedGateCheckRequest): Promise<ScopedGateCheckResult>;
}

export type OperatorScopedGateClient = {
  create(input: Extract<ScopedGateCreateRequest, { origin: "operator" }>): Promise<ScopedGate>;
  resolve(input: ScopedGateResolveRequest): Promise<ScopedGate>;
};

export interface ResourceReservationClient {
  reserve(input: ResourceReservationRequest): Promise<ResourceReservation>;
  release(input: ResourceReleaseRequest): Promise<ResourceReservation>;
  reconcile(input: ResourceReconcileRequest): Promise<ResourceReservation>;
}

export interface RequestResultClient {
  request(input: TaskRequest): Promise<TaskRequestCommit>;
  completeWithReply(input: TaskCompleteWithReply): Promise<TaskCompletionCommit>;
  claim(input: ResultDeliveryClaimRequest): Promise<ResultDelivery>;
  providerAccept(input: ResultDeliveryProviderAcceptRequest): Promise<ResultDelivery>;
  consume(input: ResultDeliveryConsumeRequest): Promise<ResultDelivery>;
  retry(input: ResultDeliveryRetryRequest): Promise<ResultDelivery>;
  reassign(input: ResultDeliveryReassignRequest): Promise<ResultDelivery>;
  abandon(input: ResultDeliveryAbandonRequest): Promise<ResultDelivery>;
}

export type AgentRequestResultClient = Omit<RequestResultClient, "providerAccept">;

export interface WorkstreamClient {
  create(input: WorkstreamCreateRequest): Promise<WorkstreamProjection>;
  settle(input: WorkstreamSettleRequest): Promise<WorkstreamProjection>;
}

export interface RunPlanClient {
  declare(input: RunPlanDeclareRequest): Promise<RunPlanDeclaration>;
}

export type PrincipalOperationFacade<Principal extends OperationPrincipalKind> = {
  readonly [Operation in PrincipalOperation<Principal> & keyof OperationInputMap]?: (
    input: OperationInputForPrincipal<Operation, Principal>,
  ) => Promise<OperationResultMap[Operation]>;
};

export interface TakeoverClient {
  takeOver(input: ChairTakeoverRequest): Promise<ChairTakeoverResult>;
}

export interface ProjectionClient {
  discover(input: ProjectDiscoveryRequest): Promise<ProjectDiscoveryResult>;
  snapshot(input: ProjectionSnapshotRequest): Promise<OperatorProjectionSnapshot>;
  page(input: ProjectionPageRequest): Promise<ProjectionPageResult>;
  events(input: ProjectionEventsRequest): Promise<ProjectionEventsResult>;
}

export interface OperatorActionClient {
  preview(input: OperatorActionPreviewRequest): Promise<OperatorActionPreview>;
  commit(input: OperatorActionCommitRequest): Promise<OperatorActionReceipt>;
  status(input: OperatorActionStatusRequest): Promise<OperatorActionStatus>;
  reconcile(input: OperatorActionReconcileRequest): Promise<OperatorActionStatus>;
}

type OperatorConsoleReadSurface = {
  gates: { read(input: ScopedGateReadRequest): Promise<ScopedGateReadResult> };
  projection: {
    viewPage(input: OperatorViewPageRequest): Promise<OperatorViewPageResult>;
    runPage(input: RunProjectionPageRequest): Promise<RunProjectionPageResult>;
    readDetail(input: OperatorDetailReadRequest): Promise<OperatorDetailReadResult>;
  };
};

export type OperatorConsoleClient = OperatorConsoleReadSurface & (
  | { readOnly: true; launchAvailable: false; actions?: undefined }
  | { readOnly: false; launchAvailable: boolean; actions: OperatorActionClient }
);

export interface InputAttestationClient {
  attestInput(input: IntegrationInputAttestationRequest): Promise<OperatorInputAttestation>;
}

export interface MessageBodyClient {
  read(input: MessageBodyReadRequest): Promise<MessageBodyReadResult>;
}

export interface GitRepositoryReadClient {
  read(input: GitRepositoryReadRequest): Promise<GitRepositoryReadResult>;
}

export interface ArtifactContentClient {
  readContent(input: ArtifactContentReadRequest): Promise<ArtifactContentReadResult>;
}

export interface EvidenceRegistryClient {
  publish(input: EvidencePublishRequest): Promise<EvidenceArtifactRegistration>;
}

export type NegotiatedOperatorClient = {
  kind: "operator";
  features: readonly ProtocolFeature[];
  projectSessions?: ProjectSessionClient;
  operatorControl?: OperatorControlClient;
  intakes?: IntakeClient;
  operations: PrincipalOperationFacade<"operator">;
  gates?: OperatorScopedGateClient;
  takeover?: TakeoverClient;
  projection?: ProjectionClient;
  messages?: MessageBodyClient;
  repository?: GitRepositoryReadClient;
  artifacts?: ArtifactContentClient;
  console?: OperatorConsoleClient;
  close(): Promise<void>;
};

export type NegotiatedAgentClient = {
  kind: "agent";
  features: readonly ProtocolFeature[];
  operations: PrincipalOperationFacade<"agent">;
  core?: BaselineFabricClient;
  gates?: Pick<ScopedGateClient, "check">;
  resources?: ResourceReservationClient;
  requestResults?: AgentRequestResultClient;
  workstreams?: WorkstreamClient;
  runPlans?: RunPlanClient;
  evidence?: EvidenceRegistryClient;
  close(): Promise<void>;
};

export type NegotiatedIntegrationClient = {
  kind: "integration";
  features: readonly ProtocolFeature[];
  operations: PrincipalOperationFacade<"integration">;
  inputAttestation?: InputAttestationClient;
  close(): Promise<void>;
};

function hasFeature(transport: ProtocolRpcTransport, feature: ProtocolFeature): boolean {
  return transport.features.includes(feature);
}

function hasOperation(transport: ProtocolRpcTransport, operation: FabricOperation): boolean {
  return transport.allowedOperations.has(operation);
}

function hasOperations(transport: ProtocolRpcTransport, operations: readonly FabricOperation[]): boolean {
  return operations.every((operation) => hasOperation(transport, operation));
}

function principalOperations<Principal extends OperationPrincipalKind>(
  principal: Principal,
  transport: ProtocolRpcTransport,
): PrincipalOperationFacade<Principal> {
  if (transport.principal.kind !== principal) {
    throw new TypeError(`transport principal is ${transport.principal.kind}, not ${principal}`);
  }
  const facade: Partial<Record<FabricOperation, (input: never) => Promise<unknown>>> = {};
  for (const operation of transport.allowedOperations) {
    if (!OPERATION_REGISTRY[operation].principals.includes(principal)) {
      throw new TypeError(`transport granted ${operation} to illegal ${principal} principal`);
    }
    facade[operation] = (input) => transport.call(operation, input);
  }
  return facade as PrincipalOperationFacade<Principal>;
}

function projectSessions(transport: ProtocolRpcTransport): ProjectSessionClient {
  return {
    ...(hasFeature(transport, "project-sessions.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.projectSessionCreate)
      ? { create: (input: ProjectSessionCreateRequest) =>
          transport.call(FABRIC_OPERATIONS.projectSessionCreate, input) }
      : {}),
    ...(hasFeature(transport, "project-sessions.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.projectSessionGet)
      ? { get: (input: ProjectSessionGetRequest) =>
          transport.call(FABRIC_OPERATIONS.projectSessionGet, input) }
      : {}),
    ...(hasFeature(transport, "project-sessions.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.projectSessionTransition)
      ? { transition: (input: ProjectSessionTransitionRequest) =>
          transport.call(FABRIC_OPERATIONS.projectSessionTransition, input) }
      : {}),
    ...(hasFeature(transport, "project-sessions.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.projectSessionClose)
      ? { close: (input: ProjectSessionCloseRequest) =>
          transport.call(FABRIC_OPERATIONS.projectSessionClose, input) }
      : {}),
    ...(hasFeature(transport, "launch-custody.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare)
      ? { prepareImplementation: (input: ProjectSessionLaunchPacketPrepareRequest) =>
          transport.call(FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare, input) }
      : {}),
    ...(hasFeature(transport, "launch-custody.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.projectSessionLaunchPrepare)
      ? { prepareLaunch: (input: ProjectSessionLaunchPrepareRequest) =>
          transport.call(FABRIC_OPERATIONS.projectSessionLaunchPrepare, input) }
      : {}),
    ...(hasFeature(transport, "project-sessions.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.membershipBind)
      ? { bindMembership: (input: Extract<MembershipBindRequest, { origin: "operator" }>) =>
          transport.call(FABRIC_OPERATIONS.membershipBind, input) }
      : {}),
  };
}

function operatorControl(transport: ProtocolRpcTransport): OperatorControlClient {
  return {
    attach: (input) => transport.call(FABRIC_OPERATIONS.operatorAttach, input),
    detach: (input) => transport.call(FABRIC_OPERATIONS.operatorDetach, input),
    heartbeat: (input) => transport.call(FABRIC_OPERATIONS.operatorHeartbeat, input),
  };
}

function intakes(transport: ProtocolRpcTransport): IntakeClient {
  return {
    createDraft: (input) => transport.call(FABRIC_OPERATIONS.intakeDraftCreate, input),
    read: (input) => transport.call(FABRIC_OPERATIONS.intakeRead, input),
    submit: (input) => transport.call(FABRIC_OPERATIONS.intakeSubmit, input),
    revise: (input) => transport.call(FABRIC_OPERATIONS.intakeRevise, input),
  };
}

function resources(transport: ProtocolRpcTransport): ResourceReservationClient {
  return {
    reserve: (input) => transport.call(FABRIC_OPERATIONS.resourceReserve, input),
    release: (input) => transport.call(FABRIC_OPERATIONS.resourceRelease, input),
    reconcile: (input) => transport.call(FABRIC_OPERATIONS.resourceReconcile, input),
  };
}

function operatorActions(transport: ProtocolRpcTransport): OperatorActionClient {
  return {
    preview: (input) => transport.call(FABRIC_OPERATIONS.operatorActionPreview, input),
    commit: (input) => transport.call(FABRIC_OPERATIONS.operatorActionCommit, input),
    status: (input) => transport.call(FABRIC_OPERATIONS.operatorActionStatus, input),
    reconcile: (input) => transport.call(FABRIC_OPERATIONS.operatorActionReconcile, input),
  };
}

function operatorConsole(transport: ProtocolRpcTransport): OperatorConsoleClient {
  const reads: OperatorConsoleReadSurface = {
    gates: { read: (input) => transport.call(FABRIC_OPERATIONS.scopedGateRead, input) },
    projection: {
      viewPage: (input) => transport.call(FABRIC_OPERATIONS.projectionViewPage, input),
      runPage: (input) => transport.call(FABRIC_OPERATIONS.projectionRunPage, input),
      readDetail: (input) => transport.call(FABRIC_OPERATIONS.projectionDetailRead, input),
    },
  };
  const actionOperations = [
    FABRIC_OPERATIONS.operatorActionPreview,
    FABRIC_OPERATIONS.operatorActionCommit,
    FABRIC_OPERATIONS.operatorActionStatus,
    FABRIC_OPERATIONS.operatorActionReconcile,
  ] as const;
  return hasFeature(transport, "operator-actions.v1") && hasOperations(transport, actionOperations)
    ? {
        ...reads,
        readOnly: false,
        launchAvailable: hasFeature(transport, "launch-custody.v1"),
        actions: operatorActions(transport),
      }
    : { ...reads, readOnly: true, launchAvailable: false };
}

export function createOperatorClient(transport: ProtocolRpcTransport): NegotiatedOperatorClient {
  return {
    kind: "operator",
    features: [...transport.features],
    operations: principalOperations("operator", transport),
    ...((
      (hasFeature(transport, "project-sessions.v1") && (
        hasOperation(transport, FABRIC_OPERATIONS.projectSessionCreate) ||
        hasOperation(transport, FABRIC_OPERATIONS.projectSessionGet) ||
        hasOperation(transport, FABRIC_OPERATIONS.projectSessionTransition) ||
        hasOperation(transport, FABRIC_OPERATIONS.projectSessionClose) ||
        hasOperation(transport, FABRIC_OPERATIONS.membershipBind)
      )) ||
      (hasFeature(transport, "launch-custody.v1") &&
        (hasOperation(transport, FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare) ||
          hasOperation(transport, FABRIC_OPERATIONS.projectSessionLaunchPrepare)))
    ) ? { projectSessions: projectSessions(transport) } : {}),
    ...(hasFeature(transport, "operator-control.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.operatorAttach,
      FABRIC_OPERATIONS.operatorDetach,
      FABRIC_OPERATIONS.operatorHeartbeat,
    ]) ? { operatorControl: operatorControl(transport) } : {}),
    ...(hasFeature(transport, "intakes.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.intakeDraftCreate,
      FABRIC_OPERATIONS.intakeRead,
      FABRIC_OPERATIONS.intakeSubmit,
      FABRIC_OPERATIONS.intakeRevise,
    ]) ? { intakes: intakes(transport) } : {}),
    ...(hasFeature(transport, "scoped-gates.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.scopedGateCreate,
      FABRIC_OPERATIONS.scopedGateResolve,
    ]) ? {
      gates: {
        create: (input: ScopedGateCreateRequest) => transport.call(FABRIC_OPERATIONS.scopedGateCreate, input),
        resolve: (input: ScopedGateResolveRequest) => transport.call(FABRIC_OPERATIONS.scopedGateResolve, input),
      },
    } : {}),
    ...(hasFeature(transport, "chair-takeover.v1") && hasOperation(transport, FABRIC_OPERATIONS.chairTakeover)
      ? { takeover: { takeOver: (input: ChairTakeoverRequest) => transport.call(FABRIC_OPERATIONS.chairTakeover, input) } }
      : {}),
    ...(hasFeature(transport, "operator-projection.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.projectDiscover,
      FABRIC_OPERATIONS.projectionSnapshot,
      FABRIC_OPERATIONS.projectionPage,
      FABRIC_OPERATIONS.projectionEvents,
    ])
      ? {
          projection: {
            discover: (input: ProjectDiscoveryRequest) => transport.call(FABRIC_OPERATIONS.projectDiscover, input),
            snapshot: (input: ProjectionSnapshotRequest) => transport.call(FABRIC_OPERATIONS.projectionSnapshot, input),
            page: (input: ProjectionPageRequest) => transport.call(FABRIC_OPERATIONS.projectionPage, input),
            events: (input: ProjectionEventsRequest) => transport.call(FABRIC_OPERATIONS.projectionEvents, input),
          },
        }
      : {}),
    ...(hasFeature(transport, "message-body-read.v1") && hasOperation(transport, FABRIC_OPERATIONS.messageBodyRead)
      ? { messages: { read: (input: MessageBodyReadRequest) => transport.call(FABRIC_OPERATIONS.messageBodyRead, input) } }
      : {}),
    ...(hasFeature(transport, "operator-repository-read.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.operatorRepositoryRead)
      ? {
          repository: {
            read: (input: GitRepositoryReadRequest) => transport.call(FABRIC_OPERATIONS.operatorRepositoryRead, input),
          },
        }
      : {}),
    ...(hasFeature(transport, "artifact-content-read.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.operatorArtifactContentRead)
      ? {
          artifacts: {
            readContent: (input: ArtifactContentReadRequest) =>
              transport.call(FABRIC_OPERATIONS.operatorArtifactContentRead, input),
          },
        }
      : {}),
    ...(hasFeature(transport, "scoped-gate-read.v1") &&
      hasFeature(transport, "operator-projection.v2") &&
      hasFeature(transport, "run-scoped-projection.v1") &&
      hasFeature(transport, "agent-topology-projection.v1") &&
      hasFeature(transport, "work-facts-projection.v1") &&
      hasOperations(transport, [
        FABRIC_OPERATIONS.scopedGateRead,
        FABRIC_OPERATIONS.projectionViewPage,
        FABRIC_OPERATIONS.projectionRunPage,
        FABRIC_OPERATIONS.projectionDetailRead,
      ])
      ? { console: operatorConsole(transport) }
      : {}),
    close: () => transport.close(),
  };
}

export function createAgentClient(transport: ProtocolRpcTransport): NegotiatedAgentClient {
  return {
    kind: "agent",
    features: [...transport.features],
    operations: principalOperations("agent", transport),
    ...(hasFeature(transport, "fabric-core.v1")
      ? {
          core: {
            call: <Operation extends BaselineOperation>(
              operation: Operation,
              input: OperationInputMap[Operation],
            ) => transport.call(operation, input),
          },
        }
      : {}),
    ...(hasFeature(transport, "scoped-gates.v1") && hasOperation(transport, FABRIC_OPERATIONS.scopedGateCheck)
      ? { gates: { check: (input: ScopedGateCheckRequest) => transport.call(FABRIC_OPERATIONS.scopedGateCheck, input) } }
      : {}),
    ...(hasFeature(transport, "resource-reservations.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.resourceReserve,
      FABRIC_OPERATIONS.resourceRelease,
      FABRIC_OPERATIONS.resourceReconcile,
    ]) ? { resources: resources(transport) } : {}),
    ...(hasFeature(transport, "request-results.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.taskRequest,
      FABRIC_OPERATIONS.taskCompleteWithReply,
      FABRIC_OPERATIONS.resultDeliveryClaim,
      FABRIC_OPERATIONS.resultDeliveryConsume,
      FABRIC_OPERATIONS.resultDeliveryRetry,
      FABRIC_OPERATIONS.resultDeliveryReassign,
      FABRIC_OPERATIONS.resultDeliveryAbandon,
    ]) ? {
      requestResults: {
        request: (input: TaskRequest) => transport.call(FABRIC_OPERATIONS.taskRequest, input),
        completeWithReply: (input: TaskCompleteWithReply) => transport.call(FABRIC_OPERATIONS.taskCompleteWithReply, input),
        claim: (input: ResultDeliveryClaimRequest) => transport.call(FABRIC_OPERATIONS.resultDeliveryClaim, input),
        consume: (input: ResultDeliveryConsumeRequest) => transport.call(FABRIC_OPERATIONS.resultDeliveryConsume, input),
        retry: (input: ResultDeliveryRetryRequest) => transport.call(FABRIC_OPERATIONS.resultDeliveryRetry, input),
        reassign: (input: ResultDeliveryReassignRequest) => transport.call(FABRIC_OPERATIONS.resultDeliveryReassign, input),
        abandon: (input: ResultDeliveryAbandonRequest) => transport.call(FABRIC_OPERATIONS.resultDeliveryAbandon, input),
      },
    } : {}),
    ...(hasFeature(transport, "workstreams.v1") && hasOperations(transport, [
      FABRIC_OPERATIONS.workstreamCreate,
      FABRIC_OPERATIONS.workstreamSettle,
    ]) ? {
      workstreams: {
        create: (input: WorkstreamCreateRequest) => transport.call(FABRIC_OPERATIONS.workstreamCreate, input),
        settle: (input: WorkstreamSettleRequest) => transport.call(FABRIC_OPERATIONS.workstreamSettle, input),
      },
    } : {}),
    ...(hasFeature(transport, "run-plan-declaration.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.runPlanDeclare)
      ? {
          runPlans: {
            declare: (input: RunPlanDeclareRequest) =>
              transport.call(FABRIC_OPERATIONS.runPlanDeclare, input),
          },
        }
      : {}),
    ...(hasFeature(transport, "artifact-registry.v1") &&
      hasOperation(transport, FABRIC_OPERATIONS.evidencePublish)
      ? {
          evidence: {
            publish: (input: EvidencePublishRequest) => transport.call(FABRIC_OPERATIONS.evidencePublish, input),
          },
        }
      : {}),
    close: () => transport.close(),
  };
}

export function createIntegrationClient(transport: ProtocolRpcTransport): NegotiatedIntegrationClient {
  return {
    kind: "integration",
    features: [...transport.features],
    operations: principalOperations("integration", transport),
    ...(hasFeature(transport, "input-attestation.v1") && hasOperation(transport, FABRIC_OPERATIONS.integrationInputAttest)
      ? {
          inputAttestation: {
            attestInput: (input: IntegrationInputAttestationRequest) =>
              transport.call(FABRIC_OPERATIONS.integrationInputAttest, input),
          },
        }
      : {}),
    close: () => transport.close(),
  };
}
