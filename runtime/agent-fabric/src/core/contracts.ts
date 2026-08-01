import type { AuthorityInput } from "../domain/types.js";
import type {
  LifecycleAcceptedSuspendedV1,
  LifecycleCurrentStateV1,
} from "@local/agent-fabric-protocol";

export type { ProviderActionDispatchRequest } from "../application/provider-action-dispatch-request.js";

export type CurrentMcpSeatBinding = {
  seat: string;
  agentId: string;
  expectedPrincipalGeneration: number;
};

export type CurrentMcpSeatBindingInput = {
  canonicalRoot: string;
  expectedPreviousGeneration: string | null;
  generation: string;
  projectSessionId: string;
  expectedSessionRevision: number;
  expectedSessionGeneration: number;
  runId: string;
  expectedRunRevision: number;
  chairAgentId: string;
  expectedChairGeneration: number;
  chairLeaseId: string;
  expiresAt: string;
  bindings: CurrentMcpSeatBinding[];
};

export type CurrentMcpSeatBindingResult = {
  expectedPreviousGeneration: string | null;
  generation: string;
  projectSessionId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runId: string;
  runRevision: number;
  chairAgentId: string;
  chairGeneration: number;
  chairLeaseId: string;
  expiresAt: string;
  credentials: Array<CurrentMcpSeatBinding & { capability: string }>;
};

export type BootstrapMcpSeatInput = {
  canonicalRoot: string;
  trustRecordDigest: string;
  seat: "claude" | "codex";
  expiresAt: string;
};

export type BootstrapMcpSeatResult = Omit<CurrentMcpSeatBindingResult, "credentials"> & {
  projectId: string;
  canonicalRoot: string;
  bootstrapRunDirectory: string;
  /** True when this call committed new custody rows or a new active generation. */
  custodyMutated: boolean;
  credentials: Array<CurrentMcpSeatBinding & { capability: string; authorityId: string }>;
  droppedSeats?: Array<{
    seat: string;
    agentId: string;
    reason:
      | "AGENT_NOT_LIVE"
      | "STALE_PRINCIPAL_GENERATION"
      | "AUTHORITY_EXPIRES_BEFORE_RENEWAL";
  }>;
};

export type LeaseResult = {
  leaseId: string;
  holderAgentId: string;
  generation: number;
  status: "active" | "quarantined";
  scope: string[];
};

export type AuthorityResult = { authorityId: string };

export type TaskResult = {
  taskId: string;
  ownerAgentId: string | null;
  state: "blocked" | "ready" | "active" | "complete" | "cancelled" | "degraded";
  revision: number;
  ownerLeaseGeneration: number;
  proposedOwnerAgentId: string | null;
  dependencies: string[];
};

export type ReceiptResult = { relativePath: string; schemaVersion: 1 | 2; sha256: string };
export type ObserverEvent = {
  cursor: number;
  eventId: string;
  type: string;
  actorAgentId: string | null;
  createdAt: number;
  summary: string;
};
export type EventsAfterResult = {
  events: ObserverEvent[];
  nextCursor: number;
};
export type ProofResult = { proofId: string };
export type RevocationResult = { revoked: true };
export type CapabilityRotationResult = { agentId: string; principalGeneration: number; capability: string };
export type ArtifactResult = { artifactId: string; relativePath: string; sha256: string };
export type BarrierResult = { scope: "run" | "stage"; closed: true; receipt: ReceiptResult };
export type LifecycleCheckpoint = {
  relativePath: string;
  sha256: string;
  mailboxWatermark: number;
  acknowledgedAboveWatermark: number[];
  inFlightChildren: string[];
  openWork: string[];
  nextAction: string;
  providerResumeReference: string;
};
export type LifecycleResult = LifecycleAcceptedSuspendedV1 | LifecycleCurrentStateV1;
export type ProviderActionResult = {
  actionId: string;
  status: "prepared" | "dispatched" | "accepted" | "terminal" | "ambiguous" | "quarantined";
  history: string[];
  executionCount: number;
  effectCount: number;
  result?: unknown;
  providerAnswer?: string;
};
export type InterventionResult = { interventionId: string };
export type DiscussionGroupInput = { groupId: string; memberAgentIds: string[] };
export type TeamResult = {
  teamId: string;
  parentTeamId: string | null;
  depth: number;
  leaderAgentId: string;
  rootTaskId: string;
  ownedTaskIds: string[];
  memberAgentIds: string[];
  budgetId: string;
  state: "active" | "frozen" | "barrier-closed";
  generation: number;
  successorAgentId: string | null;
  leader?: { agentId: string; authorityId: string };
  rootTask?: TaskResult;
  initialMembers?: Array<{ agentId: string; authorityId: string }>;
  discussionGroups: DiscussionGroupInput[];
  reservedBudget: Record<string, number>;
};
export type BudgetDimensionResult = {
  granted: number;
  reserved: number;
  consumed: number;
  available: number;
  usageUnknown: boolean;
};
export type BudgetResult = {
  budgetId: string;
  parentBudgetId: string | null;
  state: "active" | "usage-unknown" | "released";
  dimensions: Record<string, BudgetDimensionResult>;
  returned: Record<string, number>;
};
export type TeamCreateInput = {
  teamId: string;
  parentTeamId?: string;
  leader: { agentId: string; authority: AuthorityInput };
  rootTask: { taskId: string; objective: string; baseRevision: string };
  initialMembers: Array<{ agentId: string; authority: AuthorityInput }>;
  discussionGroups: DiscussionGroupInput[];
  reservedBudget: Record<string, number>;
  commandId: string;
};
