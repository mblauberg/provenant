import { OPERATOR_ACTIONS, type OperatorAction } from "@local/agent-fabric-protocol";

import type {
  BudgetResult,
  BootstrapMcpSeatInput,
  BootstrapMcpSeatResult,
  CurrentMcpSeatBindingInput,
  CurrentMcpSeatBindingResult,
  EventsAfterResult,
  ProviderActionDispatchRequest,
  TeamResult,
} from "../core/contracts.js";
import type { AuthorityInput, MessageInput } from "../domain/types.js";
import type {
  LocalOperatorConsoleCapabilityInput,
  LocalOperatorConsoleCapabilityResult,
  LocalOperatorConsoleSessionCapabilityResult,
  LocalOperatorPrincipalRotationInput,
  LocalOperatorPrincipalRotationResult,
  LocalOperatorProvisioningInput,
  LocalOperatorProvisioningResult,
  LocalOperatorSessionCapabilityInput,
  LocalOperatorSessionCapabilityResult,
  LocalOperatorTakeoverCapabilityInput,
  LocalOperatorTakeoverCapabilityResult,
} from "../operator/store.js";
import {
  FabricRemoteError,
  TimedNdjsonTransport,
  type TimedNdjsonTransportDependencies,
} from "../transport/ndjson-rpc.js";
import { isRecord, type DaemonInitializeResult } from "./protocol.js";

function isMessageKind(value: unknown): value is MessageInput["kind"] {
  return value === "request" || value === "response" || value === "event" || value === "steer" || value === "cancel" || value === "escalate" || value === "ack";
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isNumberRecord(value: unknown): value is Record<string, number> {
  return isRecord(value) && Object.values(value).every((item) => typeof item === "number");
}

function isBudgetDimensions(value: unknown): value is BudgetResult["dimensions"] {
  return (
    isRecord(value) &&
    Object.values(value).every(
      (dimension) =>
        isRecord(dimension) &&
        typeof dimension.granted === "number" &&
        typeof dimension.reserved === "number" &&
        typeof dimension.consumed === "number" &&
        typeof dimension.available === "number" &&
        typeof dimension.usageUnknown === "boolean",
    )
  );
}

function teamResult(value: unknown): TeamResult {
  if (
    !isRecord(value) ||
    typeof value.teamId !== "string" ||
    (value.parentTeamId !== null && typeof value.parentTeamId !== "string") ||
    typeof value.depth !== "number" ||
    typeof value.leaderAgentId !== "string" ||
    typeof value.rootTaskId !== "string" ||
    !isStringArray(value.ownedTaskIds) ||
    !isStringArray(value.memberAgentIds) ||
    typeof value.budgetId !== "string" ||
    (value.state !== "active" && value.state !== "frozen" && value.state !== "barrier-closed") ||
    typeof value.generation !== "number" ||
    (value.successorAgentId !== null && typeof value.successorAgentId !== "string") ||
    !Array.isArray(value.discussionGroups) ||
    !value.discussionGroups.every(
      (group) => isRecord(group) && typeof group.groupId === "string" && isStringArray(group.memberAgentIds),
    ) ||
    !isNumberRecord(value.reservedBudget)
  ) {
    throw new Error("daemon returned an invalid team result");
  }
  return {
    teamId: value.teamId,
    parentTeamId: value.parentTeamId,
    depth: value.depth,
    leaderAgentId: value.leaderAgentId,
    rootTaskId: value.rootTaskId,
    ownedTaskIds: value.ownedTaskIds,
    memberAgentIds: value.memberAgentIds,
    budgetId: value.budgetId,
    state: value.state,
    generation: value.generation,
    successorAgentId: value.successorAgentId,
    discussionGroups: value.discussionGroups,
    reservedBudget: value.reservedBudget,
  };
}

function budgetResult(value: unknown): BudgetResult {
  if (
    !isRecord(value) ||
    typeof value.budgetId !== "string" ||
    (value.parentBudgetId !== null && typeof value.parentBudgetId !== "string") ||
    (value.state !== "active" && value.state !== "usage-unknown" && value.state !== "released") ||
    !isBudgetDimensions(value.dimensions) ||
    !isNumberRecord(value.returned)
  ) {
    throw new Error("daemon returned an invalid budget result");
  }
  return {
    budgetId: value.budgetId,
    parentBudgetId: value.parentBudgetId,
    state: value.state,
    dimensions: value.dimensions,
    returned: value.returned,
  };
}

function exactResultFields(value: Record<string, unknown>, fields: readonly string[], name: string): void {
  const expected = new Set(fields);
  if (Object.keys(value).some((field) => !expected.has(field))) {
    throw new Error(`daemon returned an invalid ${name}`);
  }
}

function operatorCredential(value: unknown): { capabilityId: string; token: string } | undefined {
  if (!isRecord(value)) return undefined;
  exactResultFields(value, ["capabilityId", "token"], "operator credential");
  return typeof value.capabilityId === "string" && typeof value.token === "string"
    ? { capabilityId: value.capabilityId, token: value.token }
    : undefined;
}

function isPositiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function localOperatorProvisioningResult(value: unknown): LocalOperatorProvisioningResult {
  if (!isRecord(value)) throw new Error("daemon returned an invalid local operator provisioning result");
  const fields = [
    "projectId", "operatorId", "capabilityId", "projectAuthorityGeneration", "principalGeneration",
    "kind", "actions", "issuedAt", "expiresAt", "issued",
    ...(value.issued === true ? ["credential"] : []),
  ];
  exactResultFields(value, fields, "local operator provisioning result");
  const valid =
    typeof value.projectId === "string" &&
    typeof value.operatorId === "string" &&
    typeof value.capabilityId === "string" &&
    isPositiveInteger(value.projectAuthorityGeneration) &&
    isPositiveInteger(value.principalGeneration) &&
    value.kind === "project-launch" &&
    Array.isArray(value.actions) &&
    value.actions.length > 0 &&
    new Set(value.actions).size === value.actions.length &&
    value.actions.every((action) => action === "read" || action === "launch") &&
    typeof value.issuedAt === "string" &&
    typeof value.expiresAt === "string";
  if (!valid) throw new Error("daemon returned an invalid local operator provisioning result");
  const common = {
    projectId: value.projectId as string,
    operatorId: value.operatorId as string,
    capabilityId: value.capabilityId as string,
    projectAuthorityGeneration: value.projectAuthorityGeneration as number,
    principalGeneration: value.principalGeneration as number,
    kind: "project-launch" as const,
    actions: value.actions as Array<"read" | "launch">,
    issuedAt: value.issuedAt as string,
    expiresAt: value.expiresAt as string,
  };
  if (value.issued === true) {
    const credential = operatorCredential(value.credential);
    if (credential === undefined || credential.capabilityId !== value.capabilityId) {
      throw new Error("daemon returned an invalid local operator provisioning result");
    }
    return { ...common, issued: true, credential };
  }
  if (value.issued !== false || value.credential !== undefined) {
    throw new Error("daemon returned an invalid local operator provisioning result");
  }
  return { ...common, issued: false };
}

function localOperatorSessionCapabilityResult(value: unknown): LocalOperatorSessionCapabilityResult {
  if (!isRecord(value)) throw new Error("daemon returned an invalid local operator session capability result");
  const fields = [
    "projectId", "operatorId", "capabilityId", "projectSessionId", "projectAuthorityGeneration",
    "sessionGeneration", "principalGeneration", "kind", "actions", "issuedAt", "expiresAt", "issued",
    ...(value.issued === true ? ["credential"] : []),
  ];
  exactResultFields(value, fields, "local operator session capability result");
  const actions = Array.isArray(value.actions) &&
    value.actions.length > 0 &&
    new Set(value.actions).size === value.actions.length &&
    value.actions.every((action) => typeof action === "string" && action !== "takeover" && OPERATOR_ACTIONS.includes(action as OperatorAction))
    ? value.actions as Array<Exclude<OperatorAction, "takeover">>
    : undefined;
  const valid =
    typeof value.projectId === "string" &&
    typeof value.operatorId === "string" &&
    typeof value.capabilityId === "string" &&
    typeof value.projectSessionId === "string" &&
    isPositiveInteger(value.projectAuthorityGeneration) &&
    isPositiveInteger(value.sessionGeneration) &&
    isPositiveInteger(value.principalGeneration) &&
    value.kind === "session" &&
    actions !== undefined &&
    typeof value.issuedAt === "string" &&
    typeof value.expiresAt === "string";
  if (!valid) throw new Error("daemon returned an invalid local operator session capability result");
  const common = {
    projectId: value.projectId as string,
    operatorId: value.operatorId as string,
    capabilityId: value.capabilityId as string,
    projectSessionId: value.projectSessionId as string,
    projectAuthorityGeneration: value.projectAuthorityGeneration as number,
    sessionGeneration: value.sessionGeneration as number,
    principalGeneration: value.principalGeneration as number,
    kind: "session" as const,
    actions: actions as Array<Exclude<OperatorAction, "takeover">>,
    issuedAt: value.issuedAt as string,
    expiresAt: value.expiresAt as string,
  };
  if (value.issued === true) {
    const credential = operatorCredential(value.credential);
    if (credential === undefined || credential.capabilityId !== value.capabilityId) {
      throw new Error("daemon returned an invalid local operator session capability result");
    }
    return { ...common, issued: true, credential };
  }
  if (value.issued !== false || value.credential !== undefined) {
    throw new Error("daemon returned an invalid local operator session capability result");
  }
  return { ...common, issued: false };
}

function localOperatorTakeoverCapabilityResult(value: unknown): LocalOperatorTakeoverCapabilityResult {
  if (!isRecord(value)) throw new Error("daemon returned an invalid local operator takeover capability result");
  exactResultFields(value, [
    "projectId", "operatorId", "capabilityId", "projectSessionId", "projectAuthorityGeneration",
    "sessionGeneration", "principalGeneration", "kind", "actions", "issuedAt", "expiresAt",
    "credential", "recoveryIntent",
  ], "local operator takeover capability result");
  const credential = operatorCredential(value.credential);
  const intent = value.recoveryIntent;
  if (
    typeof value.projectId !== "string" ||
    typeof value.operatorId !== "string" ||
    typeof value.capabilityId !== "string" ||
    typeof value.projectSessionId !== "string" ||
    !isPositiveInteger(value.projectAuthorityGeneration) ||
    !isPositiveInteger(value.sessionGeneration) ||
    !isPositiveInteger(value.principalGeneration) ||
    value.kind !== "takeover" ||
    !Array.isArray(value.actions) ||
    value.actions.length !== 2 ||
    value.actions[0] !== "read" ||
    value.actions[1] !== "takeover" ||
    typeof value.issuedAt !== "string" ||
    typeof value.expiresAt !== "string" ||
    credential === undefined ||
    credential.capabilityId !== value.capabilityId ||
    !isRecord(intent) ||
    intent.kind !== "chair-bridge-recovery" ||
    intent.schemaVersion !== 1 ||
    intent.path !== "abandon" ||
    intent.projectSessionId !== value.projectSessionId ||
    typeof intent.coordinationRunId !== "string" ||
    typeof intent.lossId !== "string" ||
    typeof intent.recoveryManifestDigest !== "string" ||
    !isPositiveInteger(intent.expectedSessionRevision) ||
    !isPositiveInteger(intent.expectedSessionGeneration) ||
    !isPositiveInteger(intent.expectedRunRevision) ||
    !isPositiveInteger(intent.expectedChairGeneration) ||
    !isPositiveInteger(intent.expectedPrincipalGeneration) ||
    !isPositiveInteger(intent.expectedBridgeRevision) ||
    !isPositiveInteger(intent.expectedLostBridgeGeneration) ||
    !isPositiveInteger(intent.expectedProviderSessionGeneration) ||
    typeof intent.providerAdapterId !== "string" ||
    typeof intent.providerContractDigest !== "string" ||
    typeof intent.reason !== "string" ||
    intent.reason.length === 0
  ) {
    throw new Error("daemon returned an invalid local operator takeover capability result");
  }
  return value as unknown as LocalOperatorTakeoverCapabilityResult;
}

function localOperatorPrincipalRotationResult(value: unknown): LocalOperatorPrincipalRotationResult {
  if (!isRecord(value)) throw new Error("daemon returned an invalid local operator principal rotation result");
  exactResultFields(value, [
    "projectId", "operatorId", "principalGeneration", "revokedCapabilityCount",
  ], "local operator principal rotation result");
  if (
    typeof value.projectId !== "string" ||
    typeof value.operatorId !== "string" ||
    !isPositiveInteger(value.principalGeneration) ||
    typeof value.revokedCapabilityCount !== "number" ||
    !Number.isSafeInteger(value.revokedCapabilityCount) ||
    value.revokedCapabilityCount < 0
  ) {
    throw new Error("daemon returned an invalid local operator principal rotation result");
  }
  return {
    projectId: value.projectId,
    operatorId: value.operatorId,
    principalGeneration: value.principalGeneration,
    revokedCapabilityCount: value.revokedCapabilityCount,
  };
}

export class FabricDaemonClient {
  readonly #transport: TimedNdjsonTransport;

  private constructor(transport: TimedNdjsonTransport) {
    this.#transport = transport;
  }

  static async connect(
    socketPath: string,
    capability: string,
    requiredCapabilities: readonly string[] = [],
    transportDependencies?: TimedNdjsonTransportDependencies,
  ): Promise<FabricDaemonClient> {
    return new FabricDaemonClient(await TimedNdjsonTransport.connect({
      socketPath,
      capability,
      requiredCapabilities,
    }, transportDependencies));
  }

  get initializeResult(): DaemonInitializeResult {
    return this.#transport.initializeResult;
  }

  async #call(method: string, params: Record<string, unknown>): Promise<unknown> {
    return this.#transport.call(method, params);
  }

  async close(): Promise<void> {
    await this.#transport.close();
  }

  async probeBootstrapContract(): Promise<void> {
    try {
      await this.#call("eventsAfter", { cursor: 0, limit: 1 });
    } catch (error: unknown) {
      if (error instanceof FabricRemoteError && error.code === "BOOTSTRAP_SCOPE_VIOLATION") return;
      throw error;
    }
    throw new FabricRemoteError(
      "DAEMON_PROTOCOL_MISMATCH",
      "daemon bootstrap capability unexpectedly admitted an operational observer call",
    );
  }

  async bindCurrentMcpSeats(input: CurrentMcpSeatBindingInput): Promise<CurrentMcpSeatBindingResult> {
    const result = await this.#call("bindCurrentMcpSeats", input);
    if (
      !isRecord(result) ||
      (result.expectedPreviousGeneration !== null &&
        (typeof result.expectedPreviousGeneration !== "string" || !/^[0-9a-f]{64}$/u.test(result.expectedPreviousGeneration))) ||
      typeof result.generation !== "string" ||
      !/^[0-9a-f]{64}$/u.test(result.generation) ||
      typeof result.projectSessionId !== "string" ||
      !isPositiveInteger(result.sessionRevision) ||
      !isPositiveInteger(result.sessionGeneration) ||
      typeof result.runId !== "string" ||
      !isPositiveInteger(result.runRevision) ||
      typeof result.chairAgentId !== "string" ||
      !isPositiveInteger(result.chairGeneration) ||
      typeof result.chairLeaseId !== "string" ||
      typeof result.expiresAt !== "string" ||
      !Array.isArray(result.credentials) ||
      !result.credentials.every((credential) =>
        isRecord(credential) &&
        typeof credential.seat === "string" &&
        typeof credential.agentId === "string" &&
        isPositiveInteger(credential.expectedPrincipalGeneration) &&
        typeof credential.capability === "string"
      )
    ) {
      throw new Error("daemon returned an invalid current MCP seat binding result");
    }
    return result as CurrentMcpSeatBindingResult;
  }

  async bootstrapMcpSeat(input: BootstrapMcpSeatInput): Promise<BootstrapMcpSeatResult> {
    const result = await this.#call("bootstrapMcpSeat", input);
    if (
      !isRecord(result) ||
      typeof result.projectId !== "string" ||
      result.canonicalRoot !== input.canonicalRoot ||
      typeof result.bootstrapRunDirectory !== "string" ||
      typeof result.generation !== "string" ||
      !Array.isArray(result.credentials) ||
      !result.credentials.every((credential) =>
        isRecord(credential) &&
        typeof credential.seat === "string" &&
        typeof credential.agentId === "string" &&
        typeof credential.authorityId === "string" &&
        isPositiveInteger(credential.expectedPrincipalGeneration) &&
        typeof credential.capability === "string"
      ) ||
      (result.droppedSeats !== undefined && (
        !Array.isArray(result.droppedSeats) ||
        !result.droppedSeats.every((seat) =>
          isRecord(seat) &&
          typeof seat.seat === "string" &&
          typeof seat.agentId === "string" &&
          (
            seat.reason === "AGENT_NOT_LIVE" ||
            seat.reason === "STALE_PRINCIPAL_GENERATION" ||
            seat.reason === "AUTHORITY_EXPIRES_BEFORE_RENEWAL"
          )
        )
      ))
    ) throw new Error("daemon returned an invalid MCP bootstrap result");
    return result as BootstrapMcpSeatResult;
  }

  async provisionLocalOperator(
    input: Omit<LocalOperatorProvisioningInput, "authenticatedSubjectHash">,
  ): Promise<LocalOperatorProvisioningResult> {
    return localOperatorProvisioningResult(await this.#call("provisionLocalOperator", input));
  }

  async openLocalOperatorConsoleCapability(
    input: Omit<LocalOperatorConsoleCapabilityInput, "authenticatedSubjectHash">,
  ): Promise<LocalOperatorConsoleCapabilityResult> {
    const result = localOperatorProvisioningResult(
      await this.#call("openLocalOperatorConsoleCapability", input),
    );
    if (!result.issued) {
      throw new Error("daemon did not issue a fresh local Console capability");
    }
    return result;
  }

  async issueLocalOperatorSessionCapability(
    input: Omit<LocalOperatorSessionCapabilityInput, "authenticatedSubjectHash">,
  ): Promise<LocalOperatorSessionCapabilityResult> {
    return localOperatorSessionCapabilityResult(
      await this.#call("issueLocalOperatorSessionCapability", input),
    );
  }

  async openLocalOperatorConsoleSessionCapability(
    input: Omit<LocalOperatorSessionCapabilityInput, "authenticatedSubjectHash" | "fresh">,
  ): Promise<LocalOperatorConsoleSessionCapabilityResult> {
    const result = localOperatorSessionCapabilityResult(
      await this.#call("openLocalOperatorConsoleSessionCapability", input),
    );
    if (!result.issued) {
      throw new Error("daemon did not issue a fresh local Console session capability");
    }
    return result;
  }

  async openLocalOperatorConsoleTakeoverCapability(
    input: Omit<LocalOperatorTakeoverCapabilityInput, "authenticatedSubjectHash">,
  ): Promise<LocalOperatorTakeoverCapabilityResult> {
    return localOperatorTakeoverCapabilityResult(
      await this.#call("openLocalOperatorConsoleTakeoverCapability", input),
    );
  }

  async rotateLocalOperatorPrincipal(
    input: Omit<LocalOperatorPrincipalRotationInput, "authenticatedSubjectHash">,
  ): Promise<LocalOperatorPrincipalRotationResult> {
    return localOperatorPrincipalRotationResult(
      await this.#call("rotateLocalOperatorPrincipal", input),
    );
  }

  async delegateAuthority(input: {
    parentAuthorityId: string;
    authority: AuthorityInput;
    commandId?: string;
  }): Promise<{ authorityId: string }> {
    const result = await this.#call("delegateAuthority", input);
    if (!isRecord(result) || typeof result.authorityId !== "string") {
      throw new Error("daemon returned an invalid authority result");
    }
    return { authorityId: result.authorityId };
  }

  async registerAgent(input: { agentId: string; authorityId: string; providerSessionRef?: string; adapterId?: string }): Promise<{ capability: string }> {
    const result = await this.#call("registerAgent", input);
    if (!isRecord(result) || typeof result.capability !== "string") {
      throw new Error("daemon returned an invalid registration result");
    }
    return { capability: result.capability };
  }

  async dispatchProviderAction(input: ProviderActionDispatchRequest): Promise<{ actionId: string; status: string; history: string[]; executionCount: number; effectCount: number; result?: unknown }> {
    const result = await this.#call("dispatchProviderAction", input);
    if (!isRecord(result) || typeof result.actionId !== "string" || typeof result.status !== "string" || !Array.isArray(result.history) || !result.history.every((value) => typeof value === "string") || typeof result.executionCount !== "number" || typeof result.effectCount !== "number") {
      throw new Error("daemon returned an invalid provider action result");
    }
    return { actionId: result.actionId, status: result.status, history: result.history, executionCount: result.executionCount, effectCount: result.effectCount, ...(result.result === undefined ? {} : { result: result.result }) };
  }

  async createDiscussionGroup(input: {
    groupId: string;
    memberAgentIds: string[];
    teamId?: string;
    commandId: string;
  }): Promise<{ groupId: string; memberAgentIds: string[] }> {
    const result = await this.#call("createDiscussionGroup", input);
    if (!isRecord(result) || typeof result.groupId !== "string" || !Array.isArray(result.memberAgentIds) || !result.memberAgentIds.every((item) => typeof item === "string")) {
      throw new Error("daemon returned an invalid discussion group");
    }
    return { groupId: result.groupId, memberAgentIds: result.memberAgentIds };
  }

  async freezeSubtree(input: {
    teamId: string;
    expectedGeneration: number;
    reason: string;
    commandId: string;
  }): Promise<TeamResult> {
    return teamResult(await this.#call("freezeSubtree", input));
  }

  async adoptSubtree(input: {
    teamId: string;
    successorAgentId: string;
    expectedGeneration: number;
    handoffEvidence: string;
    commandId: string;
  }): Promise<TeamResult> {
    return teamResult(await this.#call("adoptSubtree", input));
  }

  async closeSubtreeBarrier(input: {
    teamId: string;
    expectedGeneration: number;
    commandId: string;
  }): Promise<{ teamId: string; generation: number; closed: true }> {
    const result = await this.#call("closeSubtreeBarrier", input);
    if (!isRecord(result) || typeof result.teamId !== "string" || typeof result.generation !== "number" || result.closed !== true) {
      throw new Error("daemon returned an invalid subtree barrier result");
    }
    return { teamId: result.teamId, generation: result.generation, closed: true };
  }

  async reserveBudget(input: {
    teamId: string;
    expectedTeamGeneration: number;
    parentBudgetId: string;
    budgetId: string;
    dimensions: Record<string, number>;
    commandId: string;
  }): Promise<BudgetResult> {
    return budgetResult(await this.#call("reserveBudget", input));
  }

  async recordBudgetUsage(input: {
    budgetId: string;
    usage: Record<string, number | null>;
    commandId: string;
  }): Promise<BudgetResult> {
    return budgetResult(await this.#call("recordBudgetUsage", input));
  }

  async reconcileBudgetUsage(input: {
    budgetId: string;
    consumed: Record<string, number>;
    commandId: string;
  }): Promise<BudgetResult> {
    return budgetResult(await this.#call("reconcileBudgetUsage", input));
  }

  async releaseBudget(input: { budgetId: string; commandId: string }): Promise<BudgetResult> {
    return budgetResult(await this.#call("releaseBudget", input));
  }

  async getBudget(input: { budgetId: string }): Promise<BudgetResult> {
    return budgetResult(await this.#call("getBudget", input));
  }

  async acknowledgeTaskHandoff(input: {
    taskId: string;
    taskRevision: number;
    ownerLeaseGeneration: number;
    commandId: string;
  }): Promise<{ acknowledged: true }> {
    const result = await this.#call("acknowledgeTaskHandoff", input);
    if (!isRecord(result) || result.acknowledged !== true) {
      throw new Error("daemon returned an invalid task handoff acknowledgement");
    }
    return { acknowledged: true };
  }

  async sendMessage(input: MessageInput): Promise<{ messageId: string }> {
    const result = await this.#call("sendMessage", input);
    if (!isRecord(result) || typeof result.messageId !== "string") {
      throw new Error("daemon returned an invalid message result");
    }
    return { messageId: result.messageId };
  }

  async receiveMessages(input: { limit: number; visibilityTimeoutMs: number }): Promise<Array<{
    deliveryId: string;
    messageId: string;
    sequence: number;
    body: string;
    attempt: number;
    senderId: string;
    kind: MessageInput["kind"];
    requiresAck: boolean;
  }>> {
    const result = await this.#call("receiveMessages", input);
    if (!Array.isArray(result)) {
      throw new Error("daemon returned invalid deliveries");
    }
    const deliveries: Array<{
      deliveryId: string;
      messageId: string;
      sequence: number;
      body: string;
      attempt: number;
      senderId: string;
      kind: MessageInput["kind"];
      requiresAck: boolean;
    }> = [];
    for (const value of result) {
      if (!isRecord(value) || typeof value.deliveryId !== "string" || typeof value.messageId !== "string" || typeof value.sequence !== "number" || typeof value.body !== "string" || typeof value.attempt !== "number" || typeof value.senderId !== "string" || !isMessageKind(value.kind) || typeof value.requiresAck !== "boolean") {
        throw new Error("daemon returned an invalid delivery");
      }
      deliveries.push({
        deliveryId: value.deliveryId,
        messageId: value.messageId,
        sequence: value.sequence,
        body: value.body,
        attempt: value.attempt,
        senderId: value.senderId,
        kind: value.kind,
        requiresAck: value.requiresAck,
      });
    }
    return deliveries;
  }

  async acknowledgeDelivery(input: { deliveryId: string }): Promise<void> {
    await this.#call("acknowledgeDelivery", input);
  }

  async abandonDelivery(input: { deliveryId: string; reason: string; commandId: string }): Promise<{
    deliveryId: string;
    status: "abandoned";
    reason: string;
  }> {
    const result = await this.#call("abandonDelivery", input);
    if (!isRecord(result) || typeof result.deliveryId !== "string" || result.status !== "abandoned" || typeof result.reason !== "string") {
      throw new Error("daemon returned an invalid delivery abandonment result");
    }
    return { deliveryId: result.deliveryId, status: result.status, reason: result.reason };
  }

  async getMailboxState(): Promise<{ contiguousWatermark: number; acknowledgedAboveWatermark: number[] }> {
    const result = await this.#call("getMailboxState", {});
    if (!isRecord(result) || typeof result.contiguousWatermark !== "number" || !Array.isArray(result.acknowledgedAboveWatermark) || !result.acknowledgedAboveWatermark.every((value) => typeof value === "number")) {
      throw new Error("daemon returned an invalid mailbox state");
    }
    return { contiguousWatermark: result.contiguousWatermark, acknowledgedAboveWatermark: result.acknowledgedAboveWatermark };
  }

  async eventsAfter(input: { cursor: number; limit: number }): Promise<EventsAfterResult> {
    const result = await this.#call("eventsAfter", input);
    if (!isRecord(result) || !Array.isArray(result.events) || typeof result.nextCursor !== "number") {
      throw new Error("daemon returned an invalid event page");
    }
    const events = result.events.map((value) => {
      if (
        !isRecord(value) || typeof value.cursor !== "number" || typeof value.eventId !== "string" ||
        typeof value.type !== "string" || (value.actorAgentId !== null && typeof value.actorAgentId !== "string") ||
        typeof value.createdAt !== "number" || typeof value.summary !== "string"
      ) throw new Error("daemon returned an invalid observer event");
      return {
        cursor: value.cursor,
        eventId: value.eventId,
        type: value.type,
        actorAgentId: value.actorAgentId,
        createdAt: value.createdAt,
        summary: value.summary,
      };
    });
    return { events, nextCursor: result.nextCursor };
  }

  async acquireWriteLease(input: { scope: string[]; ttlMs: number; commandId: string; taskId?: string }): Promise<{
    leaseId: string; holderAgentId: string; generation: number; status: "active" | "quarantined"; scope: string[];
  }> {
    return this.#leaseResult(await this.#call("acquireWriteLease", input));
  }

  async getWriteLease(input: { leaseId: string }): Promise<{
    leaseId: string; holderAgentId: string; generation: number; status: "active" | "quarantined"; scope: string[];
  }> {
    return this.#leaseResult(await this.#call("getWriteLease", input));
  }

  #leaseResult(result: unknown): { leaseId: string; holderAgentId: string; generation: number; status: "active" | "quarantined"; scope: string[] } {
    if (!isRecord(result) || typeof result.leaseId !== "string" || typeof result.holderAgentId !== "string" || typeof result.generation !== "number" || (result.status !== "active" && result.status !== "quarantined") || !Array.isArray(result.scope) || !result.scope.every((value) => typeof value === "string")) {
      throw new Error("daemon returned an invalid write lease result");
    }
    return { leaseId: result.leaseId, holderAgentId: result.holderAgentId, generation: result.generation, status: result.status, scope: result.scope };
  }
}

export async function connectFabricDaemon(options: {
  socketPath: string;
  capability: string;
  requiredCapabilities?: readonly string[];
}): Promise<FabricDaemonClient> {
  return FabricDaemonClient.connect(
    options.socketPath,
    options.capability,
    options.requiredCapabilities,
  );
}
