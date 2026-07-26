// Bootstrap has a separate daemon-election handshake and no public project RPC.
export type OperationPrincipalKind = "agent" | "operator" | "integration";

export type OperationFeature =
  | "fabric-core.v1"
  | "project-sessions.v1"
  | "operator-control.v1"
  | "input-attestation.v1"
  | "intakes.v1"
  | "scoped-gates.v1"
  | "scoped-gate-read.v1"
  | "resource-reservations.v1"
  | "request-results.v1"
  | "chair-takeover.v1"
  | "operator-projection.v1"
  | "operator-projection.v2"
  | "run-scoped-projection.v1"
  | "operator-actions.v1"
  | "launch-custody.v1"
  | "launch-attestation.v1"
  | "message-body-read.v1"
  | "operator-repository-read.v1"
  | "artifact-registry.v1"
  | "artifact-content-read.v1"
  | "lifecycle-control.v1"
  | "workstreams.v1"
  | "run-plan-declaration.v1"
  | "chair-live-handoff.v1"
  | "provider-review-evidence.v1"
  | "provider-context-pressure.v1"
  | "herdr-control.v1"
  | "topology-wave.v1";

type OperationDefinition = {
  operation: `fabric.v1.${string}`;
  feature: OperationFeature;
  principals: readonly OperationPrincipalKind[];
  kind: "baseline" | "extension";
  grantScope?: "provider-launch";
  gateOwner?: "scoped-gate";
};

function defineOperations<const Registry extends Record<string, OperationDefinition>>(registry: Registry): Registry {
  return registry;
}

const DEFINITIONS = defineOperations({
  delegateAuthority: { operation: "fabric.v1.authority.delegate", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  registerAgent: { operation: "fabric.v1.agent.register", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  spawnAgent: { operation: "fabric.v1.agent.spawn", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  attachAgent: { operation: "fabric.v1.agent.attach", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  sendMessage: { operation: "fabric.v1.message.send", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  createDiscussionGroup: { operation: "fabric.v1.discussion-group.create", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  receiveMessages: { operation: "fabric.v1.message.receive", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  acknowledgeDelivery: { operation: "fabric.v1.delivery.acknowledge", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  abandonDelivery: { operation: "fabric.v1.delivery.abandon", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getMailboxState: { operation: "fabric.v1.mailbox.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  createTask: { operation: "fabric.v1.task.create", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  claimTask: { operation: "fabric.v1.task.claim", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  refreshTaskReadiness: { operation: "fabric.v1.task.readiness.refresh", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recordObjectiveCheck: { operation: "fabric.v1.task.objective-check.record", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  acknowledgeTaskHandoff: { operation: "fabric.v1.task.handoff.acknowledge", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getTask: { operation: "fabric.v1.task.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  updateTask: { operation: "fabric.v1.task.update", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recordTaskOwnerRecoveryProof: { operation: "fabric.v1.task.owner-recovery-proof.record", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recoverTaskOwner: { operation: "fabric.v1.task.owner.recover", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recordRevocationProof: { operation: "fabric.v1.lease.revocation-proof.record", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  revokeCapability: { operation: "fabric.v1.capability.revoke", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  rotateCapability: { operation: "fabric.v1.capability.rotate", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  acquireWriteLease: { operation: "fabric.v1.write-lease.acquire", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recoverWriteLease: { operation: "fabric.v1.write-lease.recover", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  renewWriteLease: { operation: "fabric.v1.write-lease.renew", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getWriteLease: { operation: "fabric.v1.write-lease.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  releaseWriteLease: { operation: "fabric.v1.write-lease.release", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  requestLifecycle: { operation: "fabric.v1.lifecycle.request", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getAgentLifecycle: { operation: "fabric.v1.lifecycle.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  reportProviderState: { operation: "fabric.v1.provider-state.report", feature: "fabric-core.v1", principals: ["agent", "integration"], kind: "baseline" },
  dispatchProviderAction: { operation: "fabric.v1.provider-action.dispatch", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  reconcileProviderAction: { operation: "fabric.v1.provider-action.reconcile", feature: "fabric-core.v1", principals: ["agent", "integration"], kind: "baseline" },
  getProviderAction: { operation: "fabric.v1.provider-action.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recordOperatorIntervention: { operation: "fabric.v1.operator-intervention.record", feature: "fabric-core.v1", principals: ["agent", "integration"], kind: "baseline" },
  recordVisibilityFailure: { operation: "fabric.v1.visibility-failure.record", feature: "fabric-core.v1", principals: ["agent", "integration"], kind: "baseline" },
  createTeam: { operation: "fabric.v1.team.create", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getTeam: { operation: "fabric.v1.team.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  freezeSubtree: { operation: "fabric.v1.subtree.freeze", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  adoptSubtree: { operation: "fabric.v1.subtree.adopt", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  closeSubtreeBarrier: { operation: "fabric.v1.subtree-barrier.close", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  reserveBudget: { operation: "fabric.v1.budget.reserve", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  recordBudgetUsage: { operation: "fabric.v1.budget.usage.record", feature: "fabric-core.v1", principals: ["agent", "integration"], kind: "baseline" },
  reconcileBudgetUsage: { operation: "fabric.v1.budget.usage.reconcile", feature: "fabric-core.v1", principals: ["agent", "integration"], kind: "baseline" },
  releaseBudget: { operation: "fabric.v1.budget.release", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getBudget: { operation: "fabric.v1.budget.read", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  publishArtifact: { operation: "fabric.v1.artifact.publish", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  closeBarrier: { operation: "fabric.v1.barrier.close", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  whoami: { operation: "fabric.v1.whoami", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },
  getRunStatus: { operation: "fabric.v1.run-status.read", feature: "fabric-core.v1", principals: ["agent", "operator"], kind: "baseline" },
  observeEvents: { operation: "fabric.v1.events.observe", feature: "fabric-core.v1", principals: ["agent", "operator"], kind: "baseline" },
  listTasks: { operation: "fabric.v1.task.list", feature: "fabric-core.v1", principals: ["agent", "operator"], kind: "baseline" },
  listAgents: { operation: "fabric.v1.agent.list", feature: "fabric-core.v1", principals: ["agent", "operator"], kind: "baseline" },
  listReceipts: { operation: "fabric.v1.receipt.list", feature: "fabric-core.v1", principals: ["agent", "operator"], kind: "baseline" },
  exportReceipt: { operation: "fabric.v1.receipt.export", feature: "fabric-core.v1", principals: ["agent"], kind: "baseline" },

  launchAttest: {
    operation: "fabric.v1.launch.attest",
    feature: "launch-attestation.v1",
    principals: ["agent"],
    kind: "extension",
    grantScope: "provider-launch",
  },

  projectSessionCreate: { operation: "fabric.v1.project-session.create", feature: "project-sessions.v1", principals: ["operator"], kind: "extension" },
  projectSessionGet: { operation: "fabric.v1.project-session.read", feature: "project-sessions.v1", principals: ["operator"], kind: "extension" },
  projectSessionTransition: { operation: "fabric.v1.project-session.transition", feature: "project-sessions.v1", principals: ["operator"], kind: "extension" },
  projectSessionClose: { operation: "fabric.v1.project-session.close", feature: "project-sessions.v1", principals: ["operator"], kind: "extension" },
  projectSessionLaunchPacketPrepare: { operation: "fabric.v1.project-session-launch.packet.prepare", feature: "launch-custody.v1", principals: ["operator"], kind: "extension" },
  projectSessionLaunchPrepare: { operation: "fabric.v1.project-session-launch.prepare", feature: "launch-custody.v1", principals: ["operator"], kind: "extension" },
  membershipBind: { operation: "fabric.v1.project-session.membership.bind", feature: "project-sessions.v1", principals: ["operator", "agent"], kind: "extension" },
  operatorAttach: { operation: "fabric.v1.operator.attach", feature: "operator-control.v1", principals: ["operator"], kind: "extension" },
  operatorDetach: { operation: "fabric.v1.operator.detach", feature: "operator-control.v1", principals: ["operator"], kind: "extension" },
  operatorHeartbeat: { operation: "fabric.v1.operator.heartbeat", feature: "operator-control.v1", principals: ["operator"], kind: "extension" },
  integrationInputAttest: { operation: "fabric.v1.integration.input-attest", feature: "input-attestation.v1", principals: ["integration"], kind: "extension" },
  intakeDraftCreate: { operation: "fabric.v1.intake.draft.create", feature: "intakes.v1", principals: ["operator"], kind: "extension" },
  intakeRead: { operation: "fabric.v1.intake.read", feature: "intakes.v1", principals: ["operator"], kind: "extension" },
  intakeSubmit: { operation: "fabric.v1.intake.submit", feature: "intakes.v1", principals: ["operator"], kind: "extension" },
  intakeRevise: { operation: "fabric.v1.intake.revise", feature: "intakes.v1", principals: ["operator", "agent"], kind: "extension" },
  scopedGateCreate: { operation: "fabric.v1.scoped-gate.create", feature: "scoped-gates.v1", principals: ["operator", "agent"], kind: "extension", gateOwner: "scoped-gate" },
  scopedGateResolve: { operation: "fabric.v1.scoped-gate.resolve", feature: "scoped-gates.v1", principals: ["operator"], kind: "extension", gateOwner: "scoped-gate" },
  scopedGateCheck: { operation: "fabric.v1.scoped-gate.check", feature: "scoped-gates.v1", principals: ["agent"], kind: "extension", gateOwner: "scoped-gate" },
  scopedGateRead: { operation: "fabric.v1.scoped-gate.read", feature: "scoped-gate-read.v1", principals: ["operator"], kind: "extension", gateOwner: "scoped-gate" },
  resourceReserve: { operation: "fabric.v1.resource.reserve", feature: "resource-reservations.v1", principals: ["agent"], kind: "extension" },
  resourceRelease: { operation: "fabric.v1.resource.release", feature: "resource-reservations.v1", principals: ["agent"], kind: "extension" },
  resourceReconcile: { operation: "fabric.v1.resource.reconcile", feature: "resource-reservations.v1", principals: ["agent", "integration"], kind: "extension" },
  taskRequest: { operation: "fabric.v1.task.request", feature: "request-results.v1", principals: ["agent"], kind: "extension" },
  taskCompleteWithReply: { operation: "fabric.v1.task.complete-with-reply", feature: "request-results.v1", principals: ["agent"], kind: "extension" },
  resultDeliveryClaim: { operation: "fabric.v1.result-delivery.claim", feature: "request-results.v1", principals: ["agent", "integration"], kind: "extension" },
  resultDeliveryProviderAccept: { operation: "fabric.v1.result-delivery.provider-accept", feature: "request-results.v1", principals: ["integration"], kind: "extension" },
  resultDeliveryConsume: { operation: "fabric.v1.result-delivery.consume", feature: "request-results.v1", principals: ["agent", "integration"], kind: "extension" },
  resultDeliveryRetry: { operation: "fabric.v1.result-delivery.retry", feature: "request-results.v1", principals: ["agent"], kind: "extension" },
  resultDeliveryReassign: { operation: "fabric.v1.result-delivery.reassign", feature: "request-results.v1", principals: ["agent"], kind: "extension" },
  resultDeliveryAbandon: { operation: "fabric.v1.result-delivery.abandon", feature: "request-results.v1", principals: ["agent"], kind: "extension" },
  workstreamCreate: { operation: "fabric.v1.workstream.create", feature: "workstreams.v1", principals: ["agent"], kind: "extension" },
  workstreamSettle: { operation: "fabric.v1.workstream.settle", feature: "workstreams.v1", principals: ["agent"], kind: "extension" },
  runPlanDeclare: { operation: "fabric.v1.run.plan.declare", feature: "run-plan-declaration.v1", principals: ["agent"], kind: "extension" },
  chairTakeover: { operation: "fabric.v1.chair.takeover", feature: "chair-takeover.v1", principals: ["operator"], kind: "extension" },
  projectDiscover: { operation: "fabric.v1.project.discover", feature: "operator-projection.v1", principals: ["operator"], kind: "extension" },
  projectionSnapshot: { operation: "fabric.v1.operator-projection.snapshot", feature: "operator-projection.v1", principals: ["operator"], kind: "extension" },
  projectionPage: { operation: "fabric.v1.operator-projection.page", feature: "operator-projection.v1", principals: ["operator"], kind: "extension" },
  projectionEvents: { operation: "fabric.v1.operator-projection.events", feature: "operator-projection.v1", principals: ["operator"], kind: "extension" },
  projectionViewPage: { operation: "fabric.v1.operator-projection.view-page", feature: "operator-projection.v2", principals: ["operator"], kind: "extension" },
  projectionDetailRead: { operation: "fabric.v1.operator-projection.detail.read", feature: "operator-projection.v2", principals: ["operator"], kind: "extension" },
  projectionRunPage: { operation: "fabric.v1.operator-projection.run-page", feature: "run-scoped-projection.v1", principals: ["operator"], kind: "extension" },
  operatorActionPreview: { operation: "fabric.v1.operator-action.preview", feature: "operator-actions.v1", principals: ["operator"], kind: "extension" },
  operatorActionCommit: { operation: "fabric.v1.operator-action.commit", feature: "operator-actions.v1", principals: ["operator"], kind: "extension" },
  operatorActionStatus: { operation: "fabric.v1.operator-action.status", feature: "operator-actions.v1", principals: ["operator"], kind: "extension" },
  operatorActionReconcile: { operation: "fabric.v1.operator-action.reconcile", feature: "operator-actions.v1", principals: ["operator"], kind: "extension" },
  agentLifecycleRecoveryCheckpointValidate: { operation: "fabric.v1.agent-lifecycle-recovery-checkpoint.validate", feature: "lifecycle-control.v1", principals: ["operator"], kind: "extension", gateOwner: "scoped-gate" },
  messageBodyRead: { operation: "fabric.v1.message-body.read", feature: "message-body-read.v1", principals: ["operator"], kind: "extension" },
  operatorRepositoryRead: { operation: "fabric.v1.operator-repository.read", feature: "operator-repository-read.v1", principals: ["operator"], kind: "extension" },
  evidencePublish: { operation: "fabric.v1.evidence.publish", feature: "artifact-registry.v1", principals: ["agent"], kind: "extension" },
  operatorArtifactContentRead: { operation: "fabric.v1.operator-artifact-content.read", feature: "artifact-content-read.v1", principals: ["operator"], kind: "extension" },
  reviewTargetPrepare: { operation: "fabric.v1.review-target.prepare", feature: "provider-review-evidence.v1", principals: ["agent"], kind: "extension" },
  reviewTargetPreparationRead: { operation: "fabric.v1.review-target-preparation.read", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  reviewTargetRebind: { operation: "fabric.v1.review-target.rebind", feature: "provider-review-evidence.v1", principals: ["agent"], kind: "extension" },
  reviewEvidenceRead: { operation: "fabric.v1.review-evidence.read", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  reviewEvidenceList: { operation: "fabric.v1.review-evidence.list", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  reviewEvidenceAnnotate: { operation: "fabric.v1.review-evidence.annotate", feature: "provider-review-evidence.v1", principals: ["agent"], kind: "extension" },
  reviewEvidenceAnnotationCurrentRead: { operation: "fabric.v1.review-evidence-annotation.current.read", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  reviewFindingPageRead: { operation: "fabric.v1.review-finding-page.read", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  reviewCompletionRead: { operation: "fabric.v1.review-completion.read", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  providerRouteIntegrityRecoveryRead: { operation: "fabric.v1.provider-route-integrity-recovery.read", feature: "provider-review-evidence.v1", principals: ["agent", "operator"], kind: "extension" },
  providerContextPressureRead: { operation: "fabric.v1.provider-context-pressure.read", feature: "provider-context-pressure.v1", principals: ["agent", "operator"], kind: "extension" },
  herdrSteerDispatch: { operation: "fabric.v1.herdr-steer.dispatch", feature: "herdr-control.v1", principals: ["agent"], kind: "extension" },
  topologyWaveAppend: { operation: "fabric.v1.topology-wave.append", feature: "topology-wave.v1", principals: ["agent"], kind: "extension" },
  topologyWaveCurrentRead: { operation: "fabric.v1.topology-wave.current.read", feature: "topology-wave.v1", principals: ["agent", "operator"], kind: "extension" },
  topologyWaveList: { operation: "fabric.v1.topology-wave.list", feature: "topology-wave.v1", principals: ["agent", "operator"], kind: "extension" },
});

type OperationConstants = {
  readonly [Key in keyof typeof DEFINITIONS]: (typeof DEFINITIONS)[Key]["operation"];
};

function buildOperationConstants(): OperationConstants {
  const constants: Record<string, `fabric.v1.${string}`> = {};
  for (const [key, definition] of Object.entries(DEFINITIONS)) constants[key] = definition.operation;
  return Object.freeze(constants) as OperationConstants;
}

export const FABRIC_OPERATIONS = buildOperationConstants();
export type FabricOperation = (typeof FABRIC_OPERATIONS)[keyof typeof FABRIC_OPERATIONS];
export type BaselineOperation = {
  [Key in keyof typeof DEFINITIONS]: (typeof DEFINITIONS)[Key]["kind"] extends "baseline"
    ? (typeof DEFINITIONS)[Key]["operation"]
    : never;
}[keyof typeof DEFINITIONS];
export type PrincipalOperation<Principal extends OperationPrincipalKind> = {
  [Key in keyof typeof DEFINITIONS]: Principal extends (typeof DEFINITIONS)[Key]["principals"][number]
    ? (typeof DEFINITIONS)[Key]["operation"]
    : never;
}[keyof typeof DEFINITIONS];

export type OperationRegistryEntry = OperationDefinition & { key: keyof typeof DEFINITIONS };

function buildWireRegistry(): Readonly<Record<FabricOperation, OperationRegistryEntry>> {
  const registry: Partial<Record<FabricOperation, OperationRegistryEntry>> = {};
  for (const [key, definition] of Object.entries(DEFINITIONS)) {
    const typedKey = key as keyof typeof DEFINITIONS;
    registry[definition.operation] = { ...definition, key: typedKey };
  }
  return Object.freeze(registry) as Readonly<Record<FabricOperation, OperationRegistryEntry>>;
}

export const OPERATION_REGISTRY = buildWireRegistry();

export const BASELINE_OPERATIONS = Object.freeze(
  Object.entries(OPERATION_REGISTRY)
    .filter(([, definition]) => definition.kind === "baseline")
    .map(([operation]) => operation as BaselineOperation),
);

const operationSet: ReadonlySet<string> = new Set(Object.keys(OPERATION_REGISTRY));

export function isFabricOperation(value: string): value is FabricOperation {
  return operationSet.has(value);
}

export function isBaselineOperation(operation: FabricOperation): boolean {
  return OPERATION_REGISTRY[operation].kind === "baseline";
}

export function isActiveFabricOperation(value: string): value is FabricOperation {
  return isFabricOperation(value);
}

export function isDaemonGrantableOperation(operation: FabricOperation): boolean {
  return OPERATION_REGISTRY[operation].grantScope !== "provider-launch";
}

export function operationsForPrincipal<Principal extends OperationPrincipalKind>(
  principal: Principal,
): ReadonlySet<PrincipalOperation<Principal>> {
  const operations = Object.entries(OPERATION_REGISTRY)
    .filter(([, definition]) => definition.principals.includes(principal))
    .map(([operation]) => operation as PrincipalOperation<Principal>);
  return new Set(operations);
}
