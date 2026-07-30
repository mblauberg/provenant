import type { FabricClient } from "../core/fabric.js";
import type { BootstrapMcpSeatInput, CurrentMcpSeatBindingInput } from "../core/contracts.js";
import type { AuthorityInput, MessageInput, RecoveryEvidence } from "../domain/types.js";
import { isRecord } from "../domain/record.js";
import { isBudgetUnitKey } from "../domain/unit-keys.js";
import {
  OPERATOR_ACTIONS,
  parseAuthorityEnvelopeV2,
  type OperatorAction,
} from "@local/agent-fabric-protocol";
import { fabricCliCommand } from "../domain/fabric-roots.js";

export { isRecord } from "../domain/record.js";
export {
  FABRIC_DAEMON_VERSION,
  FABRIC_PROTOCOL_VERSION,
  daemonInitializeParams,
  daemonInitializeResult,
  isDaemonInitializeResult,
  isDaemonRequest,
  isDaemonResponse,
} from "../transport/daemon-rpc-contract.js";
export type {
  DaemonInitializeParams,
  DaemonInitializeResult,
  DaemonRequest,
  DaemonResponse,
} from "../transport/daemon-rpc-contract.js";

export type ProvisionLocalOperatorInput = {
  canonicalRoot: string;
  trustRecordDigest: string;
  projectAuthorityGeneration: number;
  principalGeneration: number;
  actions: Array<"read" | "launch">;
  expiresAt: string;
};

export type OpenLocalOperatorConsoleCapabilityInput = Omit<
  ProvisionLocalOperatorInput,
  "principalGeneration"
>;

export type IssueLocalOperatorSessionCapabilityInput = {
  projectId: string;
  canonicalRoot: string;
  trustRecordDigest: string;
  projectCapability: { capabilityId: string; token: string };
  projectSessionId: string;
  sessionGeneration: number;
  actions: Array<Exclude<OperatorAction, "takeover">>;
  expiresAt: string;
  launchEnvelopeExpiresAt: string;
};

export type OpenLocalOperatorTakeoverCapabilityInput = {
  projectId: string;
  canonicalRoot: string;
  trustRecordDigest: string;
  projectCapability: { capabilityId: string; token: string };
  projectSessionId: string;
  expiresAt: string;
};

export type RotateLocalOperatorPrincipalInput = {
  projectId: string;
  operatorId: string;
  canonicalRoot: string;
  trustRecordDigest: string;
  projectAuthorityGeneration: number;
  expectedPrincipalGeneration: number;
};

function requiredString(params: Record<string, unknown>, field: string): string {
  const value = params[field];
  if (typeof value !== "string") {
    throw new TypeError(`${field} must be a string`);
  }
  return value;
}

function providerProtocolInvalid(message: string): Error & { code: "PROTOCOL_INVALID" } {
  return Object.assign(new TypeError(message), { code: "PROTOCOL_INVALID" as const });
}

function requiredNumber(params: Record<string, unknown>, field: string): number {
  const value = params[field];
  if (typeof value !== "number") {
    throw new TypeError(`${field} must be a number`);
  }
  return value;
}

function requiredPositiveInteger(params: Record<string, unknown>, field: string): number {
  const value = requiredNumber(params, field);
  if (!Number.isInteger(value) || value < 1) {
    throw new TypeError(`${field} must be a positive integer`);
  }
  return value;
}

function requiredRecord(params: Record<string, unknown>, field: string): Record<string, unknown> {
  const value = params[field];
  if (!isRecord(value)) {
    throw new TypeError(`${field} must be an object`);
  }
  return value;
}

function exactFields(value: Record<string, unknown>, fields: readonly string[], name: string): void {
  const expected = new Set(fields);
  const unknown = Object.keys(value).filter((field) => !expected.has(field));
  const missing = fields.filter((field) => !Object.hasOwn(value, field));
  if (unknown.length > 0 || missing.length > 0) {
    throw new TypeError(`${name} fields are invalid`);
  }
}

function uniqueActions<T extends string>(
  value: unknown,
  allowed: readonly T[],
  field: string,
): T[] {
  const actions = stringArray(value, field);
  if (
    actions.length === 0 ||
    new Set(actions).size !== actions.length ||
    actions.some((action) => !allowed.includes(action as T))
  ) {
    throw new TypeError(`${field} must contain unique allowed actions`);
  }
  return actions as T[];
}

function stringArray(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) {
    throw new TypeError(`${field} must be a string array`);
  }
  return value;
}

function budgetRecord(
  value: unknown,
  field: string,
  allowUnknown: false,
): Record<string, number>;
function budgetRecord(
  value: unknown,
  field: string,
  allowUnknown: true,
): Record<string, number | null>;
function budgetRecord(
  value: unknown,
  field: string,
  allowUnknown: boolean,
): Record<string, number | null> {
  if (!isRecord(value) || Object.keys(value).length === 0) {
    throw new TypeError(`${field} must be a non-empty object`);
  }
  const result: Record<string, number | null> = {};
  for (const [unit, amount] of Object.entries(value)) {
    if (
      !isBudgetUnitKey(unit) ||
      (amount !== null && (typeof amount !== "number" || !Number.isInteger(amount) || amount < 0))
    ) {
      throw new TypeError(`${field}.${unit} must be a non-negative integer`);
    }
    if (amount === null && !allowUnknown) {
      throw new TypeError(`${field}.${unit} must be a non-negative integer`);
    }
    result[unit] = amount;
  }
  return result;
}

function authority(value: unknown): AuthorityInput {
  return parseAuthorityEnvelopeV2(value, "authority");
}

function message(value: Record<string, unknown>): MessageInput {
  const audienceValue = requiredRecord(value, "audience");
  const audience: MessageInput["audience"] =
    audienceValue.kind === "agents"
      ? { kind: "agents", agentIds: stringArray(audienceValue.agentIds, "audience.agentIds") }
      : audienceValue.kind === "team"
        ? { kind: "team", teamId: requiredString(audienceValue, "teamId") }
        : audienceValue.kind === "task"
          ? { kind: "task", taskId: requiredString(audienceValue, "taskId") }
          : (() => {
              throw new TypeError("invalid message audience");
            })();
  const kind = value.kind;
  if (![
    "request",
    "response",
    "event",
    "steer",
    "cancel",
    "escalate",
    "ack",
  ].includes(typeof kind === "string" ? kind : "")) {
    throw new TypeError("invalid message kind");
  }
  if (typeof kind !== "string") {
    throw new TypeError("message kind is required");
  }
  const validKind = kind === "request" || kind === "response" || kind === "event" || kind === "steer" || kind === "cancel" || kind === "escalate" || kind === "ack";
  if (!validKind) {
    throw new TypeError("invalid message kind");
  }
  const requiresAck = value.requiresAck;
  if (typeof requiresAck !== "boolean") {
    throw new TypeError("requiresAck must be a boolean");
  }
  let context: MessageInput["context"];
  if (value.context !== undefined) {
    const contextValue = requiredRecord(value, "context");
    if (contextValue.kind === "direct") context = { kind: "direct" };
    else if (contextValue.kind === "task") context = { kind: "task", taskId: requiredString(contextValue, "taskId") };
    else if (contextValue.kind === "task-dependency") {
      context = {
        kind: "task-dependency",
        fromTaskId: requiredString(contextValue, "fromTaskId"),
        toTaskId: requiredString(contextValue, "toTaskId"),
      };
    } else if (contextValue.kind === "discussion-group") {
      context = { kind: "discussion-group", groupId: requiredString(contextValue, "groupId") };
    } else throw new TypeError("invalid message context");
  }
  return {
    audience,
    kind,
    body: requiredString(value, "body"),
    requiresAck,
    dedupeKey: requiredString(value, "dedupeKey"),
    ...(typeof value.conversationId === "string" ? { conversationId: value.conversationId } : {}),
    ...(typeof value.replyToMessageId === "string" ? { replyToMessageId: value.replyToMessageId } : {}),
    ...(typeof value.taskRevision === "number" ? { taskRevision: value.taskRevision } : {}),
    ...(typeof value.hopCount === "number" ? { hopCount: value.hopCount } : {}),
    ...(typeof value.expiresAt === "string" ? { expiresAt: value.expiresAt } : {}),
    ...(context === undefined ? {} : { context }),
  };
}

function recoveryEvidence(value: unknown): RecoveryEvidence {
  if (!isRecord(value) || typeof value.kind !== "string") {
    throw new TypeError("recovery evidence is invalid");
  }
  if (value.kind === "unproven") {
    return { kind: "unproven" };
  }
  if (value.kind === "predecessor-terminal") {
    return {
      kind: "predecessor-terminal",
      agentId: requiredString(value, "agentId"),
      providerSessionRef: requiredString(value, "providerSessionRef"),
    };
  }
  if (value.kind === "os-isolated") {
    return { kind: "os-isolated", proofRef: requiredString(value, "proofRef") };
  }
  if (value.kind === "patch-only") {
    return { kind: "patch-only", serialApplierRef: requiredString(value, "serialApplierRef") };
  }
  throw new TypeError("recovery evidence kind is invalid");
}

function lifecycleCheckpoint(value: unknown): {
  relativePath: string;
  sha256: string;
  mailboxWatermark: number;
  acknowledgedAboveWatermark: number[];
  inFlightChildren: string[];
  openWork: string[];
  nextAction: string;
  providerResumeReference: string;
} {
  if (!isRecord(value)) throw new TypeError("checkpoint must be an object");
  const acknowledged = value.acknowledgedAboveWatermark;
  if (!Array.isArray(acknowledged) || !acknowledged.every((item) => typeof item === "number")) {
    throw new TypeError("acknowledgedAboveWatermark must be a number array");
  }
  return {
    relativePath: requiredString(value, "relativePath"),
    sha256: requiredString(value, "sha256"),
    mailboxWatermark: requiredNumber(value, "mailboxWatermark"),
    acknowledgedAboveWatermark: acknowledged,
    inFlightChildren: stringArray(value.inFlightChildren, "inFlightChildren"),
    openWork: stringArray(value.openWork, "openWork"),
    nextAction: requiredString(value, "nextAction"),
    providerResumeReference: requiredString(value, "providerResumeReference"),
  };
}

function teamMembers(value: unknown): Array<{ agentId: string; authority: AuthorityInput }> {
  if (!Array.isArray(value)) throw new TypeError("initialMembers must be an array");
  return value.map((item) => {
    if (!isRecord(item)) throw new TypeError("team member must be an object");
    return { agentId: requiredString(item, "agentId"), authority: authority(item.authority) };
  });
}

function discussionGroups(value: unknown): Array<{ groupId: string; memberAgentIds: string[] }> {
  if (!Array.isArray(value)) throw new TypeError("discussionGroups must be an array");
  return value.map((item) => {
    if (!isRecord(item)) throw new TypeError("discussion group must be an object");
    return { groupId: requiredString(item, "groupId"), memberAgentIds: stringArray(item.memberAgentIds, "memberAgentIds") };
  });
}

export async function dispatchClientMethod(client: FabricClient, method: string, params: Record<string, unknown>): Promise<unknown> {
  switch (method) {
    case "delegateAuthority": {
      return client.delegateAuthority({
        parentAuthorityId: requiredString(params, "parentAuthorityId"),
        authority: authority(params.authority),
        ...(typeof params.commandId === "string" ? { commandId: params.commandId } : {}),
      });
    }
    case "createTeam": {
      const leader = requiredRecord(params, "leader");
      const rootTask = requiredRecord(params, "rootTask");
      const budgetValue = requiredRecord(params, "reservedBudget");
      const reservedBudget: Record<string, number> = {};
      for (const [unit, amount] of Object.entries(budgetValue)) {
        if (typeof amount !== "number") throw new TypeError(`reservedBudget.${unit} must be a number`);
        reservedBudget[unit] = amount;
      }
      return client.createTeam({
        teamId: requiredString(params, "teamId"),
        ...(typeof params.parentTeamId === "string" ? { parentTeamId: params.parentTeamId } : {}),
        leader: { agentId: requiredString(leader, "agentId"), authority: authority(leader.authority) },
        rootTask: {
          taskId: requiredString(rootTask, "taskId"),
          objective: requiredString(rootTask, "objective"),
          baseRevision: requiredString(rootTask, "baseRevision"),
        },
        initialMembers: teamMembers(params.initialMembers),
        discussionGroups: discussionGroups(params.discussionGroups),
        reservedBudget,
        commandId: requiredString(params, "commandId"),
      });
    }
    case "freezeSubtree":
      return client.freezeSubtree({
        teamId: requiredString(params, "teamId"),
        expectedGeneration: requiredPositiveInteger(params, "expectedGeneration"),
        reason: requiredString(params, "reason"),
        commandId: requiredString(params, "commandId"),
      });
    case "adoptSubtree":
      return client.adoptSubtree({
        teamId: requiredString(params, "teamId"),
        successorAgentId: requiredString(params, "successorAgentId"),
        expectedGeneration: requiredPositiveInteger(params, "expectedGeneration"),
        handoffEvidence: requiredString(params, "handoffEvidence"),
        commandId: requiredString(params, "commandId"),
      });
    case "closeSubtreeBarrier":
      return client.closeSubtreeBarrier({
        teamId: requiredString(params, "teamId"),
        expectedGeneration: requiredPositiveInteger(params, "expectedGeneration"),
        commandId: requiredString(params, "commandId"),
      });
    case "reserveBudget":
      return client.reserveBudget({
        teamId: requiredString(params, "teamId"),
        expectedTeamGeneration: requiredPositiveInteger(params, "expectedTeamGeneration"),
        parentBudgetId: requiredString(params, "parentBudgetId"),
        budgetId: requiredString(params, "budgetId"),
        dimensions: budgetRecord(params.dimensions, "dimensions", false),
        commandId: requiredString(params, "commandId"),
      });
    case "recordBudgetUsage":
      return client.recordBudgetUsage({
        budgetId: requiredString(params, "budgetId"),
        usage: budgetRecord(params.usage, "usage", true),
        commandId: requiredString(params, "commandId"),
      });
    case "reconcileBudgetUsage":
      return client.reconcileBudgetUsage({
        budgetId: requiredString(params, "budgetId"),
        consumed: budgetRecord(params.consumed, "consumed", false),
        commandId: requiredString(params, "commandId"),
      });
    case "releaseBudget":
      return client.releaseBudget({
        budgetId: requiredString(params, "budgetId"),
        commandId: requiredString(params, "commandId"),
      });
    case "getBudget":
      return client.getBudget({ budgetId: requiredString(params, "budgetId") });
    case "acknowledgeTaskHandoff":
      return client.acknowledgeTaskHandoff({
        taskId: requiredString(params, "taskId"),
        taskRevision: requiredPositiveInteger(params, "taskRevision"),
        ownerLeaseGeneration: requiredPositiveInteger(params, "ownerLeaseGeneration"),
        commandId: requiredString(params, "commandId"),
      });
    case "registerAgent": {
      return client.registerAgent({
        agentId: requiredString(params, "agentId"),
        authorityId: requiredString(params, "authorityId"),
        ...(typeof params.providerSessionRef === "string" ? { providerSessionRef: params.providerSessionRef } : {}),
        ...(typeof params.adapterId === "string" ? { adapterId: params.adapterId } : {}),
      });
    }
    case "spawnAgent":
      return client.spawnAgent({
        agentId: requiredString(params, "agentId"),
        authorityId: requiredString(params, "authorityId"),
        adapterId: requiredString(params, "adapterId"),
        actionId: requiredString(params, "actionId"),
        payload: requiredRecord(params, "payload"),
      });
    case "attachAgent":
      return client.attachAgent({
        agentId: requiredString(params, "agentId"),
        authorityId: requiredString(params, "authorityId"),
        adapterId: requiredString(params, "adapterId"),
        actionId: requiredString(params, "actionId"),
        providerSessionRef: requiredString(params, "providerSessionRef"),
      });
    case "dispatchProviderAction": {
      const operation = params.operation;
      if (operation !== "spawn" && operation !== "send_turn" && operation !== "wakeup" && operation !== "release" && operation !== "steer") {
        throw providerProtocolInvalid("invalid provider action operation");
      }
      if (params.certifyingReview !== null) {
        throw providerProtocolInvalid("certifying review dispatch requires the review evidence daemon owner");
      }
      if (params.routeRequest !== undefined) {
        throw providerProtocolInvalid("provider route requests require the review evidence daemon owner");
      }
      const adapterId = requiredString(params, "adapterId");
      const actionId = requiredString(params, "actionId");
      if (adapterId.includes("\0") || actionId.includes("\0")) {
        throw providerProtocolInvalid("provider adapter ID and action ID must not contain NUL");
      }
      if (operation === "spawn") {
        return client.dispatchProviderAction({
          adapterId,
          actionId,
          operation,
          taskId: requiredString(params, "taskId"),
          authorityId: requiredString(params, "authorityId"),
          certifyingReview: null,
          payload: requiredRecord(params, "payload"),
          commandId: requiredString(params, "commandId"),
        });
      }
      if (Object.hasOwn(params, "taskId")) {
        throw providerProtocolInvalid("non-spawn provider action must not carry a top-level task ID");
      }
      if (Object.hasOwn(params, "authorityId") && typeof params.authorityId !== "string") {
        throw providerProtocolInvalid("provider authority ID must be a string when present");
      }
      return client.dispatchProviderAction({
        adapterId, actionId, operation,
        ...(typeof params.authorityId === "string" ? { authorityId: params.authorityId } : {}),
        certifyingReview: null,
        payload: requiredRecord(params, "payload"), commandId: requiredString(params, "commandId"),
      });
    }
    case "requestLifecycle":
    case "releaseAgent": {
      const action = method === "releaseAgent" ? "release" : params.action;
      if (action !== "compact" && action !== "rotate" && action !== "completion-ready" && action !== "release") {
        throw new TypeError("invalid lifecycle action");
      }
      return client.requestLifecycle({
        action,
        agentId: requiredString(params, "agentId"),
        taskId: requiredString(params, "taskId"),
        taskRevision: requiredNumber(params, "taskRevision"),
        checkpoint: lifecycleCheckpoint(params.checkpoint),
        commandId: requiredString(params, "commandId"),
      });
    }
    case "recordOperatorIntervention": {
      const source = params.source;
      const provenance = params.directInputProvenance;
      if (source !== "fabric" && source !== "integration") throw new TypeError("invalid intervention source");
      if (provenance !== "complete" && provenance !== "partial" && provenance !== "unavailable") {
        throw new TypeError("invalid direct-input provenance");
      }
      return client.recordOperatorIntervention({
        source,
        directInputProvenance: provenance,
        taskRevision: requiredNumber(params, "taskRevision"),
        summary: requiredString(params, "summary"),
        commandId: requiredString(params, "commandId"),
      });
    }
    case "sendMessage":
      return client.sendMessage(message(params));
    case "createDiscussionGroup":
      return client.createDiscussionGroup({
        groupId: requiredString(params, "groupId"),
        memberAgentIds: stringArray(params.memberAgentIds, "memberAgentIds"),
        ...(typeof params.teamId === "string" ? { teamId: params.teamId } : {}),
        commandId: requiredString(params, "commandId"),
      });
    case "receiveMessages":
      return client.receiveMessages({
        limit: requiredNumber(params, "limit"),
        visibilityTimeoutMs: requiredNumber(params, "visibilityTimeoutMs"),
      });
    case "acknowledgeDelivery":
      return client.acknowledgeDelivery({ deliveryId: requiredString(params, "deliveryId") });
    case "abandonDelivery":
      return client.abandonDelivery({
        deliveryId: requiredString(params, "deliveryId"),
        reason: requiredString(params, "reason"),
        commandId: requiredString(params, "commandId"),
      });
    case "getMailboxState":
      return client.getMailboxState();
    case "eventsAfter":
      return client.eventsAfter({
        cursor: requiredNumber(params, "cursor"),
        limit: requiredNumber(params, "limit"),
      });
    case "createTask":
      return client.createTask({
        taskId: requiredString(params, "taskId"),
        authorityId: requiredString(params, "authorityId"),
        eligibleAgentIds: stringArray(params.eligibleAgentIds, "eligibleAgentIds"),
        ...(typeof params.proposedOwnerAgentId === "string" ? { proposedOwnerAgentId: params.proposedOwnerAgentId } : {}),
        ...(params.participantAgentIds === undefined ? {} : { participantAgentIds: stringArray(params.participantAgentIds, "participantAgentIds") }),
        ...(params.dependencies === undefined ? {} : { dependencies: stringArray(params.dependencies, "dependencies") }),
        ...(params.expectedArtifacts === undefined ? {} : { expectedArtifacts: stringArray(params.expectedArtifacts, "expectedArtifacts") }),
        ...(params.objectiveChecks === undefined ? {} : { objectiveChecks: stringArray(params.objectiveChecks, "objectiveChecks") }),
        objective: requiredString(params, "objective"),
        baseRevision: requiredString(params, "baseRevision"),
        commandId: requiredString(params, "commandId"),
      });
    case "claimTask":
      return client.claimTask({
        taskId: requiredString(params, "taskId"),
        expectedRevision: requiredNumber(params, "expectedRevision"),
        commandId: requiredString(params, "commandId"),
      });
    case "getTask":
      return client.getTask({ taskId: requiredString(params, "taskId") });
    case "refreshTaskReadiness":
      return client.refreshTaskReadiness({
        taskId: requiredString(params, "taskId"),
        expectedRevision: requiredNumber(params, "expectedRevision"),
        commandId: requiredString(params, "commandId"),
      });
    case "updateTask": {
      const state = params.state;
      if (state !== "complete" && state !== "cancelled" && state !== "degraded") {
        throw new TypeError("invalid terminal task state");
      }
      return client.updateTask({
        taskId: requiredString(params, "taskId"),
        expectedRevision: requiredNumber(params, "expectedRevision"),
        state,
        commandId: requiredString(params, "commandId"),
      });
    }
    case "recordTaskOwnerRecoveryProof": {
      const kind = params.kind;
      if (kind !== "predecessor-terminal" && kind !== "os-isolated" && kind !== "patch-only") throw new TypeError("invalid task-owner recovery proof kind");
      const detailValue = requiredRecord(params, "detail");
      const detail: Record<string, string> = {};
      for (const [key, value] of Object.entries(detailValue)) {
        if (typeof value !== "string") throw new TypeError(`detail.${key} must be a string`);
        detail[key] = value;
      }
      return client.recordTaskOwnerRecoveryProof({ taskId: requiredString(params, "taskId"), ownerLeaseGeneration: requiredNumber(params, "ownerLeaseGeneration"), kind, detail, commandId: requiredString(params, "commandId") });
    }
    case "recoverTaskOwner":
      return client.recoverTaskOwner({
        taskId: requiredString(params, "taskId"), expectedRevision: requiredNumber(params, "expectedRevision"),
        expectedOwnerLeaseGeneration: requiredNumber(params, "expectedOwnerLeaseGeneration"), successorAgentId: requiredString(params, "successorAgentId"),
        proofId: requiredString(params, "proofId"), commandId: requiredString(params, "commandId"),
      });
    case "recordRevocationProof": {
      const kind = params.kind;
      if (kind !== "predecessor-terminal" && kind !== "os-isolated" && kind !== "patch-only") {
        throw new TypeError("invalid revocation proof kind");
      }
      const detailValue = requiredRecord(params, "detail");
      const detail: Record<string, string> = {};
      for (const [key, value] of Object.entries(detailValue)) {
        if (typeof value !== "string") {
          throw new TypeError(`detail.${key} must be a string`);
        }
        detail[key] = value;
      }
      return client.recordRevocationProof({
        leaseId: requiredString(params, "leaseId"),
        generation: requiredNumber(params, "generation"),
        kind,
        detail,
        commandId: requiredString(params, "commandId"),
      });
    }
    case "revokeCapability":
      return client.revokeCapability({
        agentId: requiredString(params, "agentId"),
        commandId: requiredString(params, "commandId"),
      });
    case "rotateCapability":
      return client.rotateCapability({
        agentId: requiredString(params, "agentId"),
        expectedPrincipalGeneration: requiredNumber(params, "expectedPrincipalGeneration"),
        commandId: requiredString(params, "commandId"),
      });
    case "acquireWriteLease":
      return client.acquireWriteLease({
        scope: stringArray(params.scope, "scope"),
        ttlMs: requiredNumber(params, "ttlMs"),
        commandId: requiredString(params, "commandId"),
        ...(params.taskId === undefined ? {} : { taskId: requiredString(params, "taskId") }),
      });
    case "recoverWriteLease":
      return client.recoverWriteLease({
        leaseId: requiredString(params, "leaseId"),
        expectedGeneration: requiredNumber(params, "expectedGeneration"),
        commandId: requiredString(params, "commandId"),
        evidence: recoveryEvidence(params.evidence),
      });
    case "renewWriteLease":
      return client.renewWriteLease({
        leaseId: requiredString(params, "leaseId"),
        expectedGeneration: requiredNumber(params, "expectedGeneration"),
        ttlMs: requiredNumber(params, "ttlMs"),
        commandId: requiredString(params, "commandId"),
      });
    case "getWriteLease":
      return client.getWriteLease({ leaseId: requiredString(params, "leaseId") });
    case "publishArtifact":
      return client.publishArtifact({
        ...(typeof params.taskId === "string" ? { taskId: params.taskId } : {}),
        relativePath: requiredString(params, "relativePath"),
        sha256: requiredString(params, "sha256"),
        commandId: requiredString(params, "commandId"),
      });
    case "closeBarrier": {
      const scope = params.scope;
      if (scope !== "run" && scope !== "stage") {
        throw new TypeError("barrier scope must be run or stage");
      }
      return client.closeBarrier({
        scope,
        ...(typeof params.stageId === "string" ? { stageId: params.stageId } : {}),
        commandId: requiredString(params, "commandId"),
      });
    }
    case "getRunStatus":
      return client.getRunStatus({ runId: requiredString(params, "runId") });
    case "listTasks":
      return client.listTasks({ runId: requiredString(params, "runId") });
    case "listAgents":
      return client.listAgents({ runId: requiredString(params, "runId") });
    case "listReceipts":
      return client.listReceipts({ runId: requiredString(params, "runId") });
    case "exportReceipt":
      return client.exportReceipt({ commandId: requiredString(params, "commandId") });
    default:
      throw new TypeError(`unsupported daemon method ${method}`);
  }
}

export function bindCurrentMcpSeatsInput(params: Record<string, unknown>): CurrentMcpSeatBindingInput {
  exactFields(params, [
    "canonicalRoot",
    "expectedPreviousGeneration",
    "generation",
    "projectSessionId",
    "expectedSessionRevision",
    "expectedSessionGeneration",
    "runId",
    "expectedRunRevision",
    "chairAgentId",
    "expectedChairGeneration",
    "chairLeaseId",
    "expiresAt",
    "bindings",
  ], "current MCP seat binding");
  if (
    (params.expectedPreviousGeneration !== null &&
      (typeof params.expectedPreviousGeneration !== "string" || !/^[0-9a-f]{64}$/u.test(params.expectedPreviousGeneration))) ||
    typeof params.generation !== "string" ||
    !/^[0-9a-f]{64}$/u.test(params.generation)
  ) {
    throw new TypeError("current MCP seat binding generations are invalid");
  }
  if (!Array.isArray(params.bindings) || params.bindings.length === 0) {
    throw new TypeError("current MCP seat binding requires a non-empty bindings array");
  }
  const bindings = params.bindings.map((value, index) => {
    const binding = requiredRecord({ binding: value }, "binding");
    exactFields(binding, ["seat", "agentId", "expectedPrincipalGeneration"], `current MCP seat binding ${String(index)}`);
    return {
      seat: requiredString(binding, "seat"),
      agentId: requiredString(binding, "agentId"),
      expectedPrincipalGeneration: requiredPositiveInteger(binding, "expectedPrincipalGeneration"),
    };
  });
  return {
    canonicalRoot: requiredString(params, "canonicalRoot"),
    expectedPreviousGeneration: params.expectedPreviousGeneration,
    generation: params.generation,
    projectSessionId: requiredString(params, "projectSessionId"),
    expectedSessionRevision: requiredPositiveInteger(params, "expectedSessionRevision"),
    expectedSessionGeneration: requiredPositiveInteger(params, "expectedSessionGeneration"),
    runId: requiredString(params, "runId"),
    expectedRunRevision: requiredPositiveInteger(params, "expectedRunRevision"),
    chairAgentId: requiredString(params, "chairAgentId"),
    expectedChairGeneration: requiredPositiveInteger(params, "expectedChairGeneration"),
    chairLeaseId: requiredString(params, "chairLeaseId"),
    expiresAt: requiredString(params, "expiresAt"),
    bindings,
  };
}

export function bootstrapMcpSeatInput(
  params: Record<string, unknown>,
  productRoot: string,
): BootstrapMcpSeatInput {
  exactFields(params, ["canonicalRoot", "trustRecordDigest", "seat", "expiresAt"], "MCP zero-state bootstrap");
  const canonicalRoot = requiredString(params, "canonicalRoot");
  const seat = requiredString(params, "seat");
  if (seat !== "claude" && seat !== "codex") {
    throw new TypeError(
      `MCP bootstrap creates only chair seats claude or codex; run ` +
      `${fabricCliCommand({ productRootFlag: productRoot })} mcp peer-provision ` +
      `--project '${canonicalRoot.replaceAll("'", `'"'"'`)}' --seat ${seat} instead`,
    );
  }
  const trustRecordDigest = requiredString(params, "trustRecordDigest");
  if (!/^sha256:[0-9a-f]{64}$/u.test(trustRecordDigest)) throw new TypeError("MCP bootstrap trust digest is invalid");
  return {
    canonicalRoot,
    trustRecordDigest,
    seat,
    expiresAt: requiredString(params, "expiresAt"),
  };
}

export function provisionLocalOperatorInput(
  params: Record<string, unknown>,
): ProvisionLocalOperatorInput {
  exactFields(params, [
    "canonicalRoot",
    "trustRecordDigest",
    "projectAuthorityGeneration",
    "principalGeneration",
    "actions",
    "expiresAt",
  ], "local operator provisioning");
  return {
    canonicalRoot: requiredString(params, "canonicalRoot"),
    trustRecordDigest: requiredString(params, "trustRecordDigest"),
    projectAuthorityGeneration: requiredPositiveInteger(params, "projectAuthorityGeneration"),
    principalGeneration: requiredPositiveInteger(params, "principalGeneration"),
    actions: uniqueActions(params.actions, ["read", "launch"] as const, "actions"),
    expiresAt: requiredString(params, "expiresAt"),
  };
}

export function openLocalOperatorConsoleCapabilityInput(
  params: Record<string, unknown>,
): OpenLocalOperatorConsoleCapabilityInput {
  exactFields(params, [
    "canonicalRoot",
    "trustRecordDigest",
    "projectAuthorityGeneration",
    "actions",
    "expiresAt",
  ], "local Console operator capability");
  return {
    canonicalRoot: requiredString(params, "canonicalRoot"),
    trustRecordDigest: requiredString(params, "trustRecordDigest"),
    projectAuthorityGeneration: requiredPositiveInteger(params, "projectAuthorityGeneration"),
    actions: uniqueActions(params.actions, ["read", "launch"] as const, "actions"),
    expiresAt: requiredString(params, "expiresAt"),
  };
}

export function issueLocalOperatorSessionCapabilityInput(
  params: Record<string, unknown>,
): IssueLocalOperatorSessionCapabilityInput {
  exactFields(params, [
    "projectId",
    "canonicalRoot",
    "trustRecordDigest",
    "projectCapability",
    "projectSessionId",
    "sessionGeneration",
    "actions",
    "expiresAt",
    "launchEnvelopeExpiresAt",
  ], "local operator session capability");
  const projectCapability = requiredRecord(params, "projectCapability");
  exactFields(projectCapability, ["capabilityId", "token"], "project capability credential");
  const allowed = OPERATOR_ACTIONS.filter((action): action is Exclude<OperatorAction, "takeover"> => action !== "takeover");
  return {
    projectId: requiredString(params, "projectId"),
    canonicalRoot: requiredString(params, "canonicalRoot"),
    trustRecordDigest: requiredString(params, "trustRecordDigest"),
    projectCapability: {
      capabilityId: requiredString(projectCapability, "capabilityId"),
      token: requiredString(projectCapability, "token"),
    },
    projectSessionId: requiredString(params, "projectSessionId"),
    sessionGeneration: requiredPositiveInteger(params, "sessionGeneration"),
    actions: uniqueActions(params.actions, allowed, "actions"),
    expiresAt: requiredString(params, "expiresAt"),
    launchEnvelopeExpiresAt: requiredString(params, "launchEnvelopeExpiresAt"),
  };
}

export function openLocalOperatorTakeoverCapabilityInput(
  params: Record<string, unknown>,
): OpenLocalOperatorTakeoverCapabilityInput {
  exactFields(params, [
    "projectId",
    "canonicalRoot",
    "trustRecordDigest",
    "projectCapability",
    "projectSessionId",
    "expiresAt",
  ], "local operator takeover capability");
  const projectCapability = requiredRecord(params, "projectCapability");
  exactFields(projectCapability, ["capabilityId", "token"], "project capability credential");
  return {
    projectId: requiredString(params, "projectId"),
    canonicalRoot: requiredString(params, "canonicalRoot"),
    trustRecordDigest: requiredString(params, "trustRecordDigest"),
    projectCapability: {
      capabilityId: requiredString(projectCapability, "capabilityId"),
      token: requiredString(projectCapability, "token"),
    },
    projectSessionId: requiredString(params, "projectSessionId"),
    expiresAt: requiredString(params, "expiresAt"),
  };
}

export function rotateLocalOperatorPrincipalInput(
  params: Record<string, unknown>,
): RotateLocalOperatorPrincipalInput {
  exactFields(params, [
    "projectId",
    "operatorId",
    "canonicalRoot",
    "trustRecordDigest",
    "projectAuthorityGeneration",
    "expectedPrincipalGeneration",
  ], "local operator principal rotation");
  return {
    projectId: requiredString(params, "projectId"),
    operatorId: requiredString(params, "operatorId"),
    canonicalRoot: requiredString(params, "canonicalRoot"),
    trustRecordDigest: requiredString(params, "trustRecordDigest"),
    projectAuthorityGeneration: requiredPositiveInteger(params, "projectAuthorityGeneration"),
    expectedPrincipalGeneration: requiredPositiveInteger(params, "expectedPrincipalGeneration"),
  };
}
