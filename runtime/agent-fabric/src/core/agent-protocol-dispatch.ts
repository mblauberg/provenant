import {
  FABRIC_OPERATIONS,
  type BaselineOperation,
  type OperationInputMap,
} from "@local/agent-fabric-protocol";

import type { FabricClient } from "./client.js";
import type {
  ProviderActionDispatchRequest,
  ProviderActionResult,
} from "./contracts.js";
import { digest } from "../persistence/row-codec.js";

function providerActionRequestIdentity(value: unknown): Readonly<{
  adapterId: string;
  actionId: string;
}> {
  if (
    typeof value !== "object" ||
    value === null ||
    typeof Reflect.get(value, "adapterId") !== "string" ||
    typeof Reflect.get(value, "actionId") !== "string"
  ) {
    throw new TypeError("provider action request has no canonical action identity");
  }
  return {
    adapterId: Reflect.get(value, "adapterId") as string,
    actionId: Reflect.get(value, "actionId") as string,
  };
}

function isProviderActionPayload(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonReviewProviderActionRequest(
  request: OperationInputMap[typeof FABRIC_OPERATIONS.dispatchProviderAction],
): ProviderActionDispatchRequest {
  if (
    typeof request.adapterId !== "string" ||
    typeof request.actionId !== "string" ||
    typeof request.commandId !== "string"
  ) {
    throw new TypeError("provider action request identity must be text");
  }
  if (!isProviderActionPayload(request.payload)) {
    throw new TypeError("provider action payload must be an object");
  }
  if (request.operation === "spawn") {
    if (typeof request.taskId !== "string" || typeof request.authorityId !== "string") {
      throw new TypeError("provider spawn requires top-level task and authority identities");
    }
    return {
      adapterId: request.adapterId,
      actionId: request.actionId,
      operation: request.operation,
      taskId: request.taskId,
      authorityId: request.authorityId,
      certifyingReview: null,
      payload: request.payload,
      commandId: request.commandId,
    };
  }
  if (
    request.operation !== "send_turn" && request.operation !== "wakeup" &&
    request.operation !== "release" && request.operation !== "steer"
  ) {
    throw new TypeError("invalid provider action operation");
  }
  if (request.authorityId !== undefined && typeof request.authorityId !== "string") {
    throw new TypeError("provider authority identity must be text");
  }
  return {
    adapterId: request.adapterId,
    actionId: request.actionId,
    operation: request.operation,
    certifyingReview: null,
    payload: request.payload,
    commandId: request.commandId,
    ...(request.authorityId === undefined ? {} : { authorityId: request.authorityId }),
  };
}

function publicNonReviewProviderAction(
  action: ProviderActionResult,
  input: Readonly<{ adapterId: string; actionId: string }>,
  includeProviderAnswer: boolean,
): Omit<ProviderActionResult, "actionId" | "result" | "providerAnswer"> & {
  kind: "non-review";
  actionRef: Readonly<{ adapterId: string; actionId: string }>;
  resultDigest?: `sha256:${string}`;
  providerAnswer?: string;
} {
  if (action.actionId !== input.actionId) {
    throw new TypeError("provider action result belongs to another action");
  }
  const { actionId: _actionId, providerAnswer, result, ...metadata } = action;
  return {
    kind: "non-review",
    actionRef: { adapterId: input.adapterId, actionId: input.actionId },
    ...metadata,
    ...(result === undefined ? {} : { resultDigest: digest(result) }),
    ...(includeProviderAnswer && providerAnswer !== undefined ? { providerAnswer } : {}),
  };
}

/**
 * Keep the public baseline protocol closed over the established FabricClient.
 * The wire codec has already validated input; each client method re-authorises
 * the bearer capability at the point of use.
 */
export async function dispatchAgentProtocol(
  client: FabricClient,
  operation: BaselineOperation,
  input: OperationInputMap[BaselineOperation],
): Promise<unknown> {
  switch (operation) {
    case FABRIC_OPERATIONS.delegateAuthority:
      return client.delegateAuthority(input as never);
    case FABRIC_OPERATIONS.registerAgent:
      return client.registerAgent(input as never);
    case FABRIC_OPERATIONS.spawnAgent:
      return client.spawnAgent(input as never);
    case FABRIC_OPERATIONS.attachAgent:
      return client.attachAgent(input as never);
    case FABRIC_OPERATIONS.sendMessage:
      return client.sendMessage(input as never);
    case FABRIC_OPERATIONS.createDiscussionGroup:
      return client.createDiscussionGroup(input as never);
    case FABRIC_OPERATIONS.receiveMessages:
      return { deliveries: await client.receiveMessages(input as never) };
    case FABRIC_OPERATIONS.acknowledgeDelivery:
      await client.acknowledgeDelivery(input as never);
      return { acknowledged: true };
    case FABRIC_OPERATIONS.abandonDelivery:
      return client.abandonDelivery(input as never);
    case FABRIC_OPERATIONS.getMailboxState:
      return client.getMailboxState();
    case FABRIC_OPERATIONS.createTask:
      return client.createTask(input as never);
    case FABRIC_OPERATIONS.claimTask:
      return client.claimTask(input as never);
    case FABRIC_OPERATIONS.refreshTaskReadiness:
      return client.refreshTaskReadiness(input as never);
    case FABRIC_OPERATIONS.recordObjectiveCheck:
      return client.recordObjectiveCheck(input as never);
    case FABRIC_OPERATIONS.acknowledgeTaskHandoff:
      return client.acknowledgeTaskHandoff(input as never);
    case FABRIC_OPERATIONS.getTask:
      return client.getTask(input as never);
    case FABRIC_OPERATIONS.updateTask:
      return client.updateTask(input as never);
    case FABRIC_OPERATIONS.recordTaskOwnerRecoveryProof:
      return client.recordTaskOwnerRecoveryProof(input as never);
    case FABRIC_OPERATIONS.recoverTaskOwner:
      return client.recoverTaskOwner(input as never);
    case FABRIC_OPERATIONS.recordRevocationProof:
      return client.recordRevocationProof(input as never);
    case FABRIC_OPERATIONS.revokeCapability:
      await client.revokeCapability(input as never);
      return null;
    case FABRIC_OPERATIONS.rotateCapability:
      return client.rotateCapability(input as never);
    case FABRIC_OPERATIONS.acquireWriteLease:
      return client.acquireWriteLease(input as never);
    case FABRIC_OPERATIONS.recoverWriteLease:
      return client.recoverWriteLease(input as never);
    case FABRIC_OPERATIONS.renewWriteLease:
      return client.renewWriteLease(input as never);
    case FABRIC_OPERATIONS.getWriteLease:
      return client.getWriteLease(input as never);
    case FABRIC_OPERATIONS.releaseWriteLease:
      return client.releaseWriteLease(input as never);
    case FABRIC_OPERATIONS.requestLifecycle:
      return client.requestLifecycle(input as never);
    case FABRIC_OPERATIONS.getAgentLifecycle:
      return client.getAgentLifecycle(input as never);
    case FABRIC_OPERATIONS.reportProviderState:
      return client.reportProviderState(input as never);
    case FABRIC_OPERATIONS.dispatchProviderAction: {
      const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.dispatchProviderAction];
      if (request.certifyingReview !== null) {
        throw new TypeError("certifying review dispatch requires the review evidence daemon owner");
      }
      return publicNonReviewProviderAction(
        await client.dispatchProviderAction(nonReviewProviderActionRequest(request)),
        providerActionRequestIdentity(request),
        request.operation === "spawn",
      );
    }
    case FABRIC_OPERATIONS.reconcileProviderAction: {
      const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.reconcileProviderAction];
      if (request.expectedActionKind !== "non-review") {
        throw new TypeError("certifying review reconcile requires the review evidence daemon owner");
      }
      return publicNonReviewProviderAction(
        await client.reconcileProviderAction(request as never),
        providerActionRequestIdentity(request),
        true,
      );
    }
    case FABRIC_OPERATIONS.getProviderAction: {
      const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.getProviderAction];
      if (request.expectedActionKind !== "non-review") {
        throw new TypeError("certifying review read requires the review evidence daemon owner");
      }
      return publicNonReviewProviderAction(
        await client.getProviderAction(request as never),
        providerActionRequestIdentity(request),
        true,
      );
    }
    case FABRIC_OPERATIONS.recordOperatorIntervention:
      return client.recordOperatorIntervention(input as never);
    case FABRIC_OPERATIONS.recordVisibilityFailure:
      return client.recordVisibilityFailure(input as never);
    case FABRIC_OPERATIONS.createTeam:
      return client.createTeam(input as never);
    case FABRIC_OPERATIONS.getTeam:
      return client.getTeam(input as never);
    case FABRIC_OPERATIONS.freezeSubtree:
      return client.freezeSubtree(input as never);
    case FABRIC_OPERATIONS.adoptSubtree:
      return client.adoptSubtree(input as never);
    case FABRIC_OPERATIONS.closeSubtreeBarrier:
      return client.closeSubtreeBarrier(input as never);
    case FABRIC_OPERATIONS.reserveBudget:
      return client.reserveBudget(input as never);
    case FABRIC_OPERATIONS.recordBudgetUsage:
      return client.recordBudgetUsage(input as never);
    case FABRIC_OPERATIONS.reconcileBudgetUsage:
      return client.reconcileBudgetUsage(input as never);
    case FABRIC_OPERATIONS.releaseBudget:
      return client.releaseBudget(input as never);
    case FABRIC_OPERATIONS.getBudget:
      return client.getBudget(input as never);
    case FABRIC_OPERATIONS.publishArtifact:
      return client.publishArtifact(input as never);
    case FABRIC_OPERATIONS.closeBarrier:
      return client.closeBarrier(input as never);
    case FABRIC_OPERATIONS.whoami:
      return client.whoami();
    case FABRIC_OPERATIONS.getRunStatus:
      return client.getRunStatus(input as never);
    case FABRIC_OPERATIONS.observeEvents:
      return client.eventsAfter(input as never);
    case FABRIC_OPERATIONS.listTasks:
      return client.listTasks(input as never);
    case FABRIC_OPERATIONS.listAgents:
      return client.listAgents(input as never);
    case FABRIC_OPERATIONS.listReceipts:
      return client.listReceipts(input as never);
    case FABRIC_OPERATIONS.exportReceipt:
      return client.exportReceipt(input as never);
    default: {
      const unreachable: never = operation;
      throw new Error(`unhandled baseline operation: ${String(unreachable)}`);
    }
  }
}
