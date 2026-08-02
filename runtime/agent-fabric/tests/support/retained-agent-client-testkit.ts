import type { FabricClient } from "../../src/index.ts";

import type { ProviderActionResult } from "./lifecycle-testkit.ts";

export function createRetainedAgentClient(input: {
  chair: {
    dispatchProviderAction(
      value: Parameters<FabricClient["dispatchProviderAction"]>[0],
    ): Promise<ProviderActionResult>;
  };
  adapterId: "fake-lifecycle" | "fake-lifecycle-secondary";
  agentId: "leader" | "child";
}): FabricClient {
  let sequence = 0;
  const retainedCall = async (operation: string, operationInput: Record<string, unknown>): Promise<unknown> => {
    sequence += 1;
    const taskId = operation === "createTask"
      ? `${input.agentId}-task`
      : typeof operationInput.taskId === "string"
      ? operationInput.taskId
      : operation === "receiveMessages"
        ? `${input.agentId}-task`
        : undefined;
    const actionIdentity = operation === "requestLifecycle" && typeof operationInput.commandId === "string"
      ? `lifecycle:${operationInput.commandId}`
      : String(sequence);
    const action = await input.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: input.adapterId,
      actionId: `retained-test:${input.agentId}:${actionIdentity}`,
      operation: "send_turn",
      payload: {
        agentId: input.agentId,
        providerSessionGeneration: 1,
        ...(taskId === undefined ? {} : { taskId }),
        scenario: "retained-test-action",
        retainedAction: { operation, input: operationInput },
      },
      commandId: `retained-test:${input.agentId}:${actionIdentity}:dispatch`,
    });
    if (
      action.status !== "terminal" ||
      typeof action.result !== "object" || action.result === null ||
      !("retainedActionResult" in action.result)
    ) throw new Error(`retained ${input.agentId} ${operation} did not return a terminal result`);
    return (action.result as { retainedActionResult: unknown }).retainedActionResult;
  };
  return {
    acquireWriteLease: async (value: Record<string, unknown>) => await retainedCall(
      "acquireWriteLease",
      { taskId: `${input.agentId}-task`, ...value },
    ),
    attachAgent: async (value: Record<string, unknown>) => await retainedCall("attachAgent", value),
    claimTask: async (value: Record<string, unknown>) => await retainedCall("claimTask", value),
    createTask: async (value: Record<string, unknown>) => await retainedCall("createTask", value),
    delegateAuthority: async (value: Record<string, unknown>) => await retainedCall("delegateAuthority", value),
    receiveMessages: async (value: Record<string, unknown>) => await retainedCall("receiveMessages", value),
    requestLifecycle: async (value: Record<string, unknown>) => await retainedCall("requestLifecycle", value),
  } as unknown as FabricClient;
}
