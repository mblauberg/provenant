import {
  FABRIC_OPERATIONS,
  isActiveFabricOperation,
  operationsForPrincipal,
  type FabricOperation,
  type OperatorAction,
} from "@local/agent-fabric-protocol";

const operatorActionMutations = [
  FABRIC_OPERATIONS.operatorActionPreview,
  FABRIC_OPERATIONS.operatorActionCommit,
  FABRIC_OPERATIONS.operatorActionStatus,
  FABRIC_OPERATIONS.operatorActionReconcile,
] as const;

const operatorActionOperationBundles: Readonly<Record<OperatorAction, readonly FabricOperation[]>> = Object.freeze({
  read: [
    FABRIC_OPERATIONS.getRunStatus,
    FABRIC_OPERATIONS.observeEvents,
    FABRIC_OPERATIONS.listTasks,
    FABRIC_OPERATIONS.listAgents,
    FABRIC_OPERATIONS.listReceipts,
    FABRIC_OPERATIONS.projectSessionGet,
    FABRIC_OPERATIONS.operatorAttach,
    FABRIC_OPERATIONS.operatorDetach,
    FABRIC_OPERATIONS.operatorHeartbeat,
    FABRIC_OPERATIONS.intakeRead,
    FABRIC_OPERATIONS.scopedGateRead,
    FABRIC_OPERATIONS.projectDiscover,
    FABRIC_OPERATIONS.projectionSnapshot,
    FABRIC_OPERATIONS.projectionPage,
    FABRIC_OPERATIONS.projectionEvents,
    FABRIC_OPERATIONS.projectionViewPage,
    FABRIC_OPERATIONS.projectionRunPage,
    FABRIC_OPERATIONS.projectionDetailRead,
    FABRIC_OPERATIONS.operatorActionStatus,
    FABRIC_OPERATIONS.messageBodyRead,
    FABRIC_OPERATIONS.operatorRepositoryRead,
    FABRIC_OPERATIONS.operatorArtifactContentRead,
  ],
  decide: [
    FABRIC_OPERATIONS.projectSessionTransition,
    FABRIC_OPERATIONS.projectSessionClose,
    FABRIC_OPERATIONS.membershipBind,
    FABRIC_OPERATIONS.intakeSubmit,
    FABRIC_OPERATIONS.intakeRevise,
    FABRIC_OPERATIONS.scopedGateCreate,
    FABRIC_OPERATIONS.scopedGateResolve,
    ...operatorActionMutations,
  ],
  launch: [
    FABRIC_OPERATIONS.projectSessionCreate,
    FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
    FABRIC_OPERATIONS.projectSessionLaunchPrepare,
    FABRIC_OPERATIONS.intakeDraftCreate,
    FABRIC_OPERATIONS.operatorActionCommit,
    FABRIC_OPERATIONS.operatorActionStatus,
  ],
  takeover: [FABRIC_OPERATIONS.chairTakeover, ...operatorActionMutations],
  steer: operatorActionMutations,
  pause: operatorActionMutations,
  resume: operatorActionMutations,
  cancel: operatorActionMutations,
  drain: operatorActionMutations,
  stop: operatorActionMutations,
  git: operatorActionMutations,
  "git-authorise": operatorActionMutations,
  "git-custody-resolve": operatorActionMutations,
  "agent-lifecycle-recovery-issue": [
    ...operatorActionMutations,
    FABRIC_OPERATIONS.agentLifecycleRecoveryCheckpointValidate,
  ],
  "external-effect": operatorActionMutations,
});

export function operatorOperationsForActions(actions: readonly OperatorAction[]): FabricOperation[] {
  const legal = operationsForPrincipal("operator") as ReadonlySet<FabricOperation>;
  const operations = new Set<FabricOperation>();
  for (const action of actions) {
    for (const operation of operatorActionOperationBundles[action]) {
      if (!isActiveFabricOperation(operation) || !legal.has(operation)) {
        throw new TypeError(`operator action ${action} maps to an illegal operation ${operation}`);
      }
      operations.add(operation);
    }
  }
  return [...operations].sort();
}
