import type {
  AuthorityEnvelopeV2,
  DisclosurePolicy,
  DisclosureTarget,
} from "@local/agent-fabric-protocol";
import type { ProviderIdentityAssurance } from "../adapters/provider-identity.js";

export type AuthorityInput = AuthorityEnvelopeV2;
export type { DisclosurePolicy, DisclosureTarget };

export type Clock = () => number | Date;

export type FabricOpenOptions = {
  databasePath: string;
  workspaceRoots: string[];
  productRoot?: string;
  clock?: Clock;
  adapters?: Record<string, {
    command: string[];
    environment: Record<string, string>;
    modelPolicy?: { allowedFamilies: string[]; allowedModelPatterns: string[]; requiresExplicitModel: boolean };
    wrapperProvenance?: { repositoryCommit: string; wrapperPath: string };
    npmInstallProductRoot?: string;
    providerAssurance?: ProviderIdentityAssurance;
  }>;
  capabilityKey?: string;
  executionProfile?: string;
  maximumConcurrentProviderTurns?: number;
};

export type AgentAudience = {
  kind: "agents";
  agentIds: string[];
};

export type TeamAudience = {
  kind: "team";
  teamId: string;
};

export type TaskAudience = {
  kind: "task";
  taskId: string;
};

export type MessageAudience = AgentAudience | TeamAudience | TaskAudience;

export type MessageInput = {
  audience: MessageAudience;
  kind: "request" | "response" | "event" | "steer" | "cancel" | "escalate" | "ack";
  body: string;
  requiresAck: boolean;
  dedupeKey: string;
  conversationId?: string;
  replyToMessageId?: string;
  taskRevision?: number;
  hopCount?: number;
  expiresAt?: string;
  context?:
    | { kind: "direct" }
    | { kind: "task"; taskId: string }
    | { kind: "task-dependency"; fromTaskId: string; toTaskId: string }
    | { kind: "discussion-group"; groupId: string };
};

export const MESSAGE_POLICY = {
  maximumInlineBytes: 4096,
  maximumHops: 4,
  maximumUnacknowledgedPerAgent: 100,
} as const;

export type RecoveryEvidence =
  | { kind: "unproven" }
  | { kind: "predecessor-terminal"; agentId: string; providerSessionRef: string }
  | { kind: "os-isolated"; proofRef: string }
  | { kind: "patch-only"; serialApplierRef: string };

export type FabricErrorCode =
  | "AUTHENTICATION_FAILED"
  | "AUTHORITY_WIDENING"
  | "ARTIFACT_DIGEST_INVALID"
  | "ARTIFACT_PATH_FORBIDDEN"
  | "ADAPTER_ARTIFACT_MISSING"
  | "ADAPTER_COMPATIBILITY_INVALID"
  | "ADAPTER_DISABLED"
  | "ADAPTER_IDENTITY_MISMATCH"
  | "ADAPTER_INTERFACE_MISMATCH"
  | "ADAPTER_INTERFACE_PROBE_INCOMPLETE"
  | "ADAPTER_PATH_UNSAFE"
  | "NPM_INSTALL_ATTESTATION_MISMATCH"
  | "ADAPTER_MODEL_REQUIRED"
  | "ADAPTER_FAMILY_FORBIDDEN"
  | "BARRIER_PRECONDITION_FAILED"
  | "BUDGET_EXCEEDED"
  | "CAPABILITY_FORBIDDEN"
  | "CAPABILITY_UNAVAILABLE"
  | "CONFIG_UNTRUSTED_FIELD"
  | "CONFIG_WIDENING_FORBIDDEN"
  | "ACTION_INPUT_CONFLICT"
  | "DEDUPE_CONFLICT"
  | "DELIVERY_ALREADY_RESOLVED"
  | "DELIVERY_REASON_REQUIRED"
  | "LEASE_NOT_EXPIRED"
  | "LEASE_EXPIRED"
  | "LEASE_QUARANTINED"
  | "CHECKPOINT_INCOMPLETE"
  | "CONTEXT_UNRECONCILED"
  | "LIFECYCLE_PRECONDITION_FAILED"
  | "MODEL_REQUIRED"
  | "MODEL_NOT_ALLOWED"
  | "MODEL_FAMILY_NOT_ALLOWED"
  | "MESSAGE_RELATIONSHIP_FORBIDDEN"
  | "MESSAGE_HOP_LIMIT_EXCEEDED"
  | "MESSAGE_QUOTA_EXCEEDED"
  | "NOT_FOUND"
  | "PROVIDER_TURN_ACTIVE"
  | "STALE_LEASE_GENERATION"
  | "STALE_PRINCIPAL_GENERATION"
  | "TASK_NOT_OWNER"
  | "TASK_DEPENDENCY_BLOCKED"
  | "TASK_SUBTREE_CONFLICT"
  | "TASK_REVISION_CONFLICT"
  | "TEAM_DEPTH_EXCEEDED"
  | "STALE_TEAM_GENERATION"
  | "BUDGET_USAGE_UNKNOWN"
  | "WRITE_SCOPE_CONFLICT"
  | "WRITE_SCOPE_RECOVERY_REQUIRED"
  | "WRITE_SCOPE_QUARANTINED";
