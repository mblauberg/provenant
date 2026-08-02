import {
  actionPayload,
  chairLaunchChallengeDigest,
  isRecord,
  parseChairLaunchCapability,
  parseAgentBridgeCapability,
  parseAgentProvisionProviderResult,
  parseChairLaunchContinuityUnprovenEvidence,
  parseChairLaunchProviderResult,
  ProviderAdapterError,
  requiredString,
  type AdapterActionRecord,
  type AgentBridgeHandoff,
  type AgentFabricPrincipalBinding,
  type AgentProvisionBoundaryInput,
  type AgentProvisionProviderResult,
  type AdapterRequestHandler,
  type ChairLaunchBoundaryInput,
  type ChairLaunchHandoff,
  type ChairLaunchProviderResult,
  type ChairRecoveryBoundaryInput,
  type ProviderAdapterCapabilities,
} from "./types.js";
import type { SqliteAdapterActionJournal } from "./journal.js";
import { parseWorkspaceWriteOfflineProjection } from "./workspace-write-offline.js";

export type ProviderBoundary = {
  status(input: { resumeReference?: string }): Promise<Record<string, unknown>>;
  spawn(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
  attach(input: { resumeReference: string; payload: Record<string, unknown> }): Promise<Record<string, unknown>>;
  sendTurn(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
  interrupt(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
  release(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
  steer?(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
  compact?(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
  launchChair?(input: ChairLaunchBoundaryInput): Promise<ChairLaunchProviderResult>;
  recoverChair?(input: ChairRecoveryBoundaryInput): Promise<ChairLaunchProviderResult>;
  provisionAgent?(input: AgentProvisionBoundaryInput): Promise<AgentProvisionProviderResult>;
  hasLiveChairSession?(resumeReference: string, providerSessionGeneration: number): boolean;
  hasLiveAgentSession?(resumeReference: string, providerSessionGeneration: number, bridgeGeneration: number): boolean;
};

type SupportedOperation = "spawn" | "attach" | "send_turn" | "interrupt" | "release" | "steer" | "compact";

function capabilityUnavailable(operation: string): never {
  throw new ProviderAdapterError(
    "CAPABILITY_UNAVAILABLE",
    `adapter does not support ${operation}`,
    { capability: operation },
  );
}

function responseRecord(value: unknown, operation: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", `${operation} returned a non-object response`);
  }
  return value;
}

export function createProviderAdapter(options: {
  capabilities: ProviderAdapterCapabilities;
  boundary: ProviderBoundary;
  journal: SqliteAdapterActionJournal;
  chairLaunch?: {
    handoff?: ChairLaunchHandoff;
    validatePayload(payload: Record<string, unknown>): Record<string, unknown>;
  };
  agentBridge?: { handoff?: AgentBridgeHandoff };
  noEffect?: {
    classify(error: unknown): Record<string, unknown> | undefined;
    throwIfResult(result: Record<string, unknown>): void;
  };
}): AdapterRequestHandler {
  const capabilities: ProviderAdapterCapabilities = {
    ...options.capabilities,
    ...(options.capabilities.chairLaunch === undefined
      ? {}
      : { chairLaunch: parseChairLaunchCapability(options.capabilities.chairLaunch) }),
    ...(options.capabilities.agentBridge === undefined
      ? {}
      : { agentBridge: parseAgentBridgeCapability(options.capabilities.agentBridge) }),
  };
  const supported = new Set(capabilities.operations);
  let chairLaunchHandoff = options.chairLaunch?.handoff;
  let agentBridgeHandoff = options.agentBridge?.handoff;
  const chairLaunchChallengeDigestValue = chairLaunchHandoff === undefined
    ? undefined
    : chairLaunchChallengeDigest(chairLaunchHandoff.attestationChallenge);
  const liveChairLaunchActions = new Set<string>();
  const liveChairActionBySession = new Map<string, string>();
  const liveChairSessionByAction = new Map<string, string>();
  const liveChairBindingByAction = new Map<string, AgentFabricPrincipalBinding & {
    resumeReference: string;
    generation: number;
    bridgeGeneration: number;
  }>();
  const liveAgentSessionByAction = new Map<string, AgentFabricPrincipalBinding & {
    resumeReference: string;
    generation: number;
    bridgeGeneration: number;
  }>();

  async function provisionAgent(params: Record<string, unknown>): Promise<AgentProvisionProviderResult> {
    const bridge = capabilities.agentBridge;
    if (bridge === undefined || options.boundary.provisionAgent === undefined || agentBridgeHandoff === undefined) {
      capabilityUnavailable("provision_agent");
    }
    const allowed = new Set([
      "schemaVersion", "runId", "operation", "actionId", "targetAgentId", "authorityId",
      "bridgeGeneration", "bridgeContractDigest", "payload", "providerSessionRef",
    ]);
    if (Object.keys(params).some((key) => !allowed.has(key)) || params.schemaVersion !== 1) {
      throw new ProviderAdapterError("INVALID_PARAMS", "agent provision request does not match its closed schema");
    }
    const operation = params.operation;
    if ((operation !== "spawn" && operation !== "attach") || !bridge.operations.includes(operation)) {
      capabilityUnavailable(`provision_agent:${String(operation)}`);
    }
    if (!isRecord(params.payload)) throw new ProviderAdapterError("INVALID_PARAMS", "agent provision payload must be an object");
    const actionId = requiredString(params.actionId, "actionId");
    const runId = requiredString(params.runId, "runId");
    const targetAgentId = requiredString(params.targetAgentId, "targetAgentId");
    const authorityId = requiredString(params.authorityId, "authorityId");
    const bridgeGeneration = params.bridgeGeneration;
    if (typeof bridgeGeneration !== "number" || !Number.isSafeInteger(bridgeGeneration) || bridgeGeneration < 1) {
      throw new ProviderAdapterError("INVALID_PARAMS", "bridgeGeneration must be positive");
    }
    const bridgeContractDigest = requiredString(params.bridgeContractDigest, "bridgeContractDigest");
    if (!/^sha256:[0-9a-f]{64}$/u.test(bridgeContractDigest)) {
      throw new ProviderAdapterError("INVALID_PARAMS", "bridgeContractDigest must be sha256");
    }
    if (
      agentBridgeHandoff.expectedPrincipal.agentId !== targetAgentId ||
      agentBridgeHandoff.expectedPrincipal.runId !== runId
    ) {
      throw new ProviderAdapterError(
        "AGENT_BRIDGE_UNPROVEN",
        "agent bridge request does not match the private principal binding",
      );
    }
    const publicPayload = {
      schemaVersion: 1,
      runId,
      operation,
      targetAgentId,
      authorityId,
      bridgeGeneration,
      bridgeContractDigest,
      payload: params.payload,
      ...(typeof params.providerSessionRef === "string" ? { providerSessionRef: params.providerSessionRef } : {}),
    };
    const prepared = options.journal.prepare(actionId, "provision_agent", publicPayload);
    if (!prepared.created) {
      if (prepared.record.status !== "terminal" || !isRecord(prepared.record.result)) {
        throw new ProviderAdapterError("ACTION_RECONCILIATION_REQUIRED", "agent provision action requires lookup");
      }
      const result = parseAgentProvisionProviderResult(prepared.record.result, {
        adapterId: capabilities.adapterId,
        actionId,
        targetAgentId,
        bridgeGeneration,
        bridgeContractDigest,
        ...(agentBridgeHandoff.lifecycleAttestation === undefined ? {} : {
          lifecycleAttestation: agentBridgeHandoff.lifecycleAttestation,
        }),
      });
      const live = liveAgentSessionByAction.get(actionId);
      if (
        live?.agentId !== targetAgentId ||
        live.runId !== runId ||
        live?.resumeReference !== result.providerSessionRef ||
        options.boundary.hasLiveAgentSession?.(
          result.providerSessionRef,
          result.providerSessionGeneration,
          result.bridgeGeneration,
        ) !== true
      ) throw new ProviderAdapterError("AGENT_BRIDGE_LOST", "agent provision result has no retained bridge");
      return result;
    }
    const handoff = agentBridgeHandoff;
    agentBridgeHandoff = undefined;
    const privateHandoffValues = [
      handoff.capability,
      handoff.socketPath,
      ...(handoff.lifecycleAttestation === undefined ? [] : [handoff.lifecycleAttestation.challenge]),
    ];
    if (containsPrivateValue(publicPayload, privateHandoffValues)) {
      throw new ProviderAdapterError("PRIVATE_HANDOFF_DISCLOSED", "agent provision payload contains private handoff material");
    }
    options.journal.markDispatched(actionId);
    try {
      const result = parseAgentProvisionProviderResult(await options.boundary.provisionAgent({
        schemaVersion: 1,
        runId,
        operation,
        actionId,
        targetAgentId,
        authorityId,
        bridgeGeneration,
        bridgeContractDigest,
        payload: params.payload,
        ...(typeof params.providerSessionRef === "string" ? { providerSessionRef: params.providerSessionRef } : {}),
        expectedPrincipal: handoff.expectedPrincipal,
        environment: {
          AGENT_FABRIC_CAPABILITY: handoff.capability,
          AGENT_FABRIC_SOCKET_PATH: handoff.socketPath,
          ...(handoff.lifecycleAttestation === undefined ? {} : {
            AGENT_FABRIC_ATTESTATION_CHALLENGE: handoff.lifecycleAttestation.challenge,
            AGENT_FABRIC_ATTESTATION_CHALLENGE_DIGEST: handoff.lifecycleAttestation.challengeDigest,
            AGENT_FABRIC_LIFECYCLE_CUSTODY_ID: handoff.lifecycleAttestation.custodyId,
            AGENT_FABRIC_LIFECYCLE_CHECKPOINT_DIGEST: handoff.lifecycleAttestation.checkpointDigest,
          }),
        },
      }), {
        adapterId: capabilities.adapterId,
        actionId,
        targetAgentId,
        bridgeGeneration,
        bridgeContractDigest,
        ...(handoff.lifecycleAttestation === undefined ? {} : {
          lifecycleAttestation: handoff.lifecycleAttestation,
        }),
      });
      if (containsPrivateValue(result, privateHandoffValues)) {
        throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "agent provision result contains private handoff material");
      }
      options.journal.markAccepted(actionId);
      options.journal.markTerminal(actionId, result, true);
      liveAgentSessionByAction.set(actionId, {
        ...handoff.expectedPrincipal,
        resumeReference: result.providerSessionRef,
        generation: result.providerSessionGeneration,
        bridgeGeneration: result.bridgeGeneration,
      });
      return result;
    } catch (error: unknown) {
      if (options.journal.get(actionId).status === "dispatched") options.journal.markAmbiguous(actionId);
      if (containsPrivateErrorValue(error, privateHandoffValues)) {
        throw new ProviderAdapterError(
          "PROVIDER_RESPONSE_INVALID",
          "agent provision provider error contained private handoff material",
        );
      }
      throw error;
    }
  }

  function chairLaunchRequest(params: Record<string, unknown>): {
    actionId: string;
    providerContractDigest: string;
    payload: Record<string, unknown>;
  } {
    if (
      Object.keys(params).length !== 4 ||
      !Object.hasOwn(params, "schemaVersion") ||
      !Object.hasOwn(params, "actionId") ||
      !Object.hasOwn(params, "providerContractDigest") ||
      !Object.hasOwn(params, "payload")
    ) {
      throw new ProviderAdapterError("INVALID_PARAMS", "chair launch request does not match its closed schema");
    }
    if (params.schemaVersion !== 1) {
      throw new ProviderAdapterError("INVALID_PARAMS", "chair launch schemaVersion must be 1");
    }
    const providerContractDigest = requiredString(params.providerContractDigest, "providerContractDigest");
    if (!/^sha256:[0-9a-f]{64}$/u.test(providerContractDigest)) {
      throw new ProviderAdapterError("INVALID_PARAMS", "providerContractDigest must be a sha256 digest");
    }
    if (!isRecord(params.payload)) {
      throw new ProviderAdapterError("INVALID_PARAMS", "chair launch payload must be an object");
    }
    if (options.chairLaunch === undefined) capabilityUnavailable("launch_chair");
    return {
      actionId: requiredString(params.actionId, "actionId"),
      providerContractDigest,
      payload: options.chairLaunch.validatePayload(params.payload),
    };
  }

  function containsPrivateValue(value: unknown, privateValues: readonly string[]): boolean {
    if (typeof value === "string") return privateValues.some((candidate) => value.includes(candidate));
    if (Array.isArray(value)) return value.some((entry) => containsPrivateValue(entry, privateValues));
    return isRecord(value) && Object.values(value).some((entry) => containsPrivateValue(entry, privateValues));
  }

  function containsPrivateErrorValue(
    value: unknown,
    privateValues: readonly string[],
    seen = new Set<object>(),
  ): boolean {
    if (typeof value === "string") return privateValues.some((candidate) => value.includes(candidate));
    if (typeof value !== "object" || value === null || seen.has(value)) return false;
    seen.add(value);
    const descriptors = Object.getOwnPropertyDescriptors(value);
    return Reflect.ownKeys(descriptors).some((key) => {
      const descriptor = descriptors[key as keyof typeof descriptors];
      return descriptor !== undefined && "value" in descriptor &&
        containsPrivateErrorValue(descriptor.value, privateValues, seen);
    });
  }

  async function launchChair(params: Record<string, unknown>): Promise<Record<string, unknown>> {
    if (!supported.has("launch_chair") || options.boundary.launchChair === undefined) {
      capabilityUnavailable("launch_chair");
    }
    const request = chairLaunchRequest(params);
    const attestationBinding = (): {
      providerAdapterId: string;
      providerActionId: string;
      providerContractDigest: string;
      challengeDigest: string;
    } => {
      if (chairLaunchChallengeDigestValue === undefined) {
        throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "chair launch challenge is no longer available");
      }
      return {
        providerAdapterId: capabilities.adapterId,
        providerActionId: request.actionId,
        providerContractDigest: request.providerContractDigest,
        challengeDigest: chairLaunchChallengeDigestValue,
      };
    };
    const journalPayload = {
      schemaVersion: 1,
      providerContractDigest: request.providerContractDigest,
      payload: request.payload,
    };
    function replayOrConsumed(record: AdapterActionRecord): ChairLaunchProviderResult {
      if (
        record.status === "terminal" &&
        record.idempotencyProven &&
        liveChairLaunchActions.has(request.actionId)
      ) {
        const result = parseChairLaunchProviderResult(record.result, attestationBinding());
        if (!isLiveChairResult(request.actionId, result)) {
          throw chairBridgeLost(request.actionId);
        }
        return result;
      }
      if (record.status === "terminal" && record.idempotencyProven) {
        throw new ProviderAdapterError(
          "CHAIR_BRIDGE_LOST",
          "chair launch result is durable but its volatile provider bridge is unavailable",
          { actionId: request.actionId },
        );
      }
      throw new ProviderAdapterError(
        "CHAIR_LAUNCH_ALREADY_CONSUMED",
        "chair launch private handoff was already consumed",
        { actionId: request.actionId },
      );
    }
    if (chairLaunchHandoff === undefined) {
      let existing: AdapterActionRecord;
      try {
        existing = options.journal.get(request.actionId);
      } catch (error: unknown) {
        if (error instanceof ProviderAdapterError && error.code === "ACTION_NOT_FOUND") {
          throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "chair launch private handoff is unavailable");
        }
        throw error;
      }
      options.journal.prepare(request.actionId, "launch_chair", journalPayload);
      return replayOrConsumed(existing);
    }
    const handoff = chairLaunchHandoff;
    const credentialValues = [handoff.capability, handoff.socketPath];
    const privateInputValues = [...credentialValues, handoff.attestationChallenge];
    if (containsPrivateValue({ actionId: request.actionId, payload: request.payload }, privateInputValues)) {
      throw new ProviderAdapterError("PRIVATE_HANDOFF_DISCLOSED", "chair launch payload contains private handoff material");
    }
    const prepared = options.journal.prepare(request.actionId, "launch_chair", journalPayload, privateInputValues);
    if (!prepared.created) {
      chairLaunchHandoff = undefined;
      return replayOrConsumed(prepared.record);
    }
    options.journal.markDispatched(request.actionId);
    chairLaunchHandoff = undefined;
    try {
      const result = parseChairLaunchProviderResult(await options.boundary.launchChair({
        ...request,
        providerAdapterId: capabilities.adapterId,
        challengeDigest: attestationBinding().challengeDigest,
        expectedPrincipal: handoff.expectedPrincipal,
        environment: {
          AGENT_FABRIC_CAPABILITY: handoff.capability,
          AGENT_FABRIC_SOCKET_PATH: handoff.socketPath,
          AGENT_FABRIC_ATTESTATION_CHALLENGE: handoff.attestationChallenge,
        },
      }), attestationBinding());
      if (containsPrivateValue(result, privateInputValues)) {
        throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "chair launch result contains private handoff material");
      }
      options.journal.markAccepted(request.actionId);
      options.journal.markTerminal(request.actionId, result, true, privateInputValues);
      liveChairLaunchActions.add(request.actionId);
      liveChairActionBySession.set(result.resumeReference, request.actionId);
      liveChairSessionByAction.set(request.actionId, result.resumeReference);
      liveChairBindingByAction.set(request.actionId, {
        ...handoff.expectedPrincipal,
        resumeReference: result.resumeReference,
        generation: result.providerSessionGeneration,
        bridgeGeneration: 1,
      });
      return result;
    } catch (error: unknown) {
      let current = options.journal.get(request.actionId);
      if (
        current.status === "dispatched" &&
        error instanceof ProviderAdapterError &&
        error.code === "CHAIR_CONTINUITY_UNPROVEN"
      ) {
        try {
          const evidence = parseChairLaunchContinuityUnprovenEvidence(
            error.details,
            request.providerContractDigest,
          );
          if (containsPrivateValue(evidence, credentialValues)) {
            throw new ProviderAdapterError(
              "PROVIDER_RESPONSE_INVALID",
              "chair launch continuity evidence contains private handoff material",
            );
          }
          options.journal.markAccepted(request.actionId);
          options.journal.markAmbiguous(request.actionId, evidence, privateInputValues);
          current = options.journal.get(request.actionId);
        } catch {
          current = options.journal.get(request.actionId);
        }
      }
      if (current.status === "dispatched") options.journal.markAmbiguous(request.actionId);
      throw new ProviderAdapterError(
        "CHAIR_LAUNCH_FAILED",
        "chair launch provider handoff failed",
        { actionId: request.actionId },
      );
    }
  }

  async function recoverChair(params: Record<string, unknown>): Promise<Record<string, unknown>> {
    const allowed = new Set([
      "schemaVersion", "recoveryId", "lossId", "actionId", "providerContractDigest",
      "resumeReference", "expectedProviderSessionGeneration", "nextProviderSessionGeneration",
      "bridgeGeneration", "payload",
    ]);
    if (Object.keys(params).some((key) => !allowed.has(key)) || params.schemaVersion !== 1 || !isRecord(params.payload)) {
      throw new ProviderAdapterError("INVALID_PARAMS", "chair recovery request does not match its closed schema");
    }
    const actionId = requiredString(params.actionId, "actionId");
    const recoveryId = requiredString(params.recoveryId, "recoveryId");
    const lossId = requiredString(params.lossId, "lossId");
    const providerContractDigest = requiredString(params.providerContractDigest, "providerContractDigest");
    const resumeReference = requiredString(params.resumeReference, "resumeReference");
    const expectedProviderSessionGeneration = params.expectedProviderSessionGeneration;
    const nextProviderSessionGeneration = params.nextProviderSessionGeneration;
    const bridgeGeneration = params.bridgeGeneration;
    if (
      !/^sha256:[0-9a-f]{64}$/u.test(providerContractDigest) ||
      !Number.isSafeInteger(expectedProviderSessionGeneration) || Number(expectedProviderSessionGeneration) < 1 ||
      nextProviderSessionGeneration !== Number(expectedProviderSessionGeneration) + 1 ||
      !Number.isSafeInteger(bridgeGeneration) || Number(bridgeGeneration) < 2
    ) throw new ProviderAdapterError("INVALID_PARAMS", "chair recovery generation binding is invalid");
    const publicPayload = {
      schemaVersion: 1,
      recoveryId,
      lossId,
      actionId,
      providerContractDigest,
      resumeReference,
      expectedProviderSessionGeneration,
      nextProviderSessionGeneration,
      bridgeGeneration,
      payload: params.payload,
    };
    const replayOrConsumed = (record: AdapterActionRecord): ChairLaunchProviderResult => {
      if (record.status !== "terminal" || !record.idempotencyProven || !isRecord(record.result)) {
        throw new ProviderAdapterError("ACTION_RECONCILIATION_REQUIRED", "chair recovery action requires lookup");
      }
      const continuity = record.result.fabricContinuity;
      if (!isRecord(continuity) || typeof continuity.challengeDigest !== "string") {
        throw new ProviderAdapterError("JOURNAL_INVALID", "terminal chair recovery binding is malformed");
      }
      const result = parseChairLaunchProviderResult(record.result, {
        providerAdapterId: capabilities.adapterId,
        providerActionId: actionId,
        providerContractDigest,
        challengeDigest: continuity.challengeDigest,
      });
      if (
        result.resumeReference !== resumeReference ||
        result.providerSessionGeneration !== nextProviderSessionGeneration ||
        !isLiveChairResult(actionId, result)
      ) throw chairBridgeLost(actionId);
      return result;
    };
    if (chairLaunchHandoff === undefined) {
      let existing: AdapterActionRecord;
      try {
        existing = options.journal.get(actionId);
      } catch (error: unknown) {
        if (error instanceof ProviderAdapterError && error.code === "ACTION_NOT_FOUND") {
          throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "chair recovery private handoff is unavailable");
        }
        throw error;
      }
      options.journal.prepare(actionId, "recover_chair", publicPayload);
      return replayOrConsumed(existing);
    }
    if (!supported.has("recover_chair") || options.boundary.recoverChair === undefined) {
      capabilityUnavailable("recover_chair");
    }
    const handoff = chairLaunchHandoff;
    const challengeDigest = chairLaunchChallengeDigest(handoff.attestationChallenge);
    const prepared = options.journal.prepare(actionId, "recover_chair", publicPayload, [
      handoff.capability,
      handoff.socketPath,
      handoff.attestationChallenge,
    ]);
    if (!prepared.created) {
      chairLaunchHandoff = undefined;
      return replayOrConsumed(prepared.record);
    }
    options.journal.markDispatched(actionId);
    chairLaunchHandoff = undefined;
    try {
      const result = parseChairLaunchProviderResult(await options.boundary.recoverChair({
        actionId,
        providerAdapterId: capabilities.adapterId,
        providerContractDigest,
        challengeDigest,
        expectedPrincipal: handoff.expectedPrincipal,
        recoveryId,
        lossId,
        resumeReference,
        expectedProviderSessionGeneration: Number(expectedProviderSessionGeneration),
        nextProviderSessionGeneration: Number(nextProviderSessionGeneration),
        bridgeGeneration: Number(bridgeGeneration),
        payload: params.payload,
        environment: {
          AGENT_FABRIC_CAPABILITY: handoff.capability,
          AGENT_FABRIC_SOCKET_PATH: handoff.socketPath,
          AGENT_FABRIC_ATTESTATION_CHALLENGE: handoff.attestationChallenge,
        },
      }), {
        providerAdapterId: capabilities.adapterId,
        providerActionId: actionId,
        providerContractDigest,
        challengeDigest,
      });
      if (
        result.resumeReference !== resumeReference ||
        result.providerSessionGeneration !== nextProviderSessionGeneration
      ) throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "chair recovery provider session binding changed");
      options.journal.markAccepted(actionId);
      options.journal.markTerminal(actionId, result, true, [
        handoff.capability,
        handoff.socketPath,
        handoff.attestationChallenge,
      ]);
      liveChairLaunchActions.add(actionId);
      liveChairActionBySession.set(result.resumeReference, actionId);
      liveChairSessionByAction.set(actionId, result.resumeReference);
      liveChairBindingByAction.set(actionId, {
        ...handoff.expectedPrincipal,
        resumeReference: result.resumeReference,
        generation: result.providerSessionGeneration,
        bridgeGeneration: Number(bridgeGeneration),
      });
      return result;
    } catch {
      if (options.journal.get(actionId).status === "dispatched") options.journal.markAmbiguous(actionId);
      throw new ProviderAdapterError(
        "CHAIR_RECOVERY_FAILED",
        "chair recovery provider handoff failed",
        { actionId },
      );
    }
  }

  async function effect(operation: SupportedOperation, actionId: string, payload: Record<string, unknown>): Promise<{
    record: AdapterActionRecord;
    result: Record<string, unknown>;
  }> {
    if (!supported.has(operation)) capabilityUnavailable(operation);
    parseWorkspaceWriteOfflineProjection(payload);
    const prepared = options.journal.prepare(actionId, operation, payload);
    if (!prepared.created) {
      if (prepared.record.status === "terminal" && isRecord(prepared.record.result)) {
        return { record: prepared.record, result: prepared.record.result };
      }
      return {
        record: prepared.record,
        result: isRecord(prepared.record.result) ? prepared.record.result : {},
      };
    }

    options.journal.markDispatched(actionId);
    try {
      let value: Record<string, unknown>;
      switch (operation) {
        case "spawn":
          value = responseRecord(await options.boundary.spawn(payload), operation);
          break;
        case "attach":
          value = responseRecord(
            await options.boundary.attach({
              resumeReference: requiredString(payload.resumeReference, "resumeReference"),
              payload,
            }),
            operation,
          );
          break;
        case "send_turn":
          value = responseRecord(await options.boundary.sendTurn(payload), operation);
          break;
        case "interrupt":
          value = responseRecord(await options.boundary.interrupt(payload), operation);
          break;
        case "release":
          value = responseRecord(await options.boundary.release(payload), operation);
          if (typeof payload.resumeReference === "string") {
            const launchActionId = liveChairActionBySession.get(payload.resumeReference);
            liveChairActionBySession.delete(payload.resumeReference);
            if (launchActionId !== undefined) {
              liveChairLaunchActions.delete(launchActionId);
              liveChairSessionByAction.delete(launchActionId);
              liveChairBindingByAction.delete(launchActionId);
            }
          }
          break;
        case "steer":
          if (options.boundary.steer === undefined) capabilityUnavailable(operation);
          value = responseRecord(await options.boundary.steer(payload), operation);
          break;
        case "compact":
          if (options.boundary.compact === undefined) capabilityUnavailable(operation);
          value = responseRecord(await options.boundary.compact(payload), operation);
          break;
      }
      options.journal.markAccepted(actionId);
      const record = options.journal.markTerminal(actionId, value, false);
      return { record, result: value };
    } catch (error: unknown) {
      const noEffect = options.noEffect?.classify(error);
      if (noEffect !== undefined) {
        const record = options.journal.markTerminal(actionId, noEffect, true);
        return { record, result: noEffect };
      }
      const current = options.journal.get(actionId);
      if (current.status === "dispatched") options.journal.markAmbiguous(actionId);
      throw error;
    }
  }

  function lookupAction(actionId: string): AdapterActionRecord {
    const record = options.journal.get(actionId);
    if (
      (record.operation !== "launch_chair" && record.operation !== "recover_chair") ||
      record.status !== "terminal"
    ) return record;
    if (!isRecord(record.result) || !isRecord(record.result.fabricContinuity)) {
      throw new ProviderAdapterError("JOURNAL_INVALID", "terminal chair launch journal evidence is malformed");
    }
    const continuity = record.result.fabricContinuity;
    if (typeof continuity.providerContractDigest !== "string" || typeof continuity.challengeDigest !== "string") {
      throw new ProviderAdapterError("JOURNAL_INVALID", "terminal chair launch journal binding is malformed");
    }
    const result = parseChairLaunchProviderResult(record.result, {
      providerAdapterId: capabilities.adapterId,
      providerActionId: record.actionId,
      providerContractDigest: continuity.providerContractDigest,
      challengeDigest: continuity.challengeDigest,
    });
    if (!isLiveChairResult(record.actionId, result)) throw chairBridgeLost(record.actionId);
    return {
      ...record,
      result,
    };
  }

  function isLiveChairResult(actionId: string, result: ChairLaunchProviderResult): boolean {
    return (
      liveChairLaunchActions.has(actionId) &&
      liveChairSessionByAction.get(actionId) === result.resumeReference &&
      liveChairActionBySession.get(result.resumeReference) === actionId &&
      options.boundary.hasLiveChairSession?.(
        result.resumeReference,
        result.providerSessionGeneration,
      ) === true
    );
  }

  function chairBridgeLost(actionId: string): ProviderAdapterError {
    return new ProviderAdapterError(
      "CHAIR_BRIDGE_LOST",
      "terminal chair launch evidence is retained but its exact live provider bridge is unavailable",
      { actionId },
    );
  }

  function retainedBridgeHealth(params: Record<string, unknown>): Record<string, unknown> {
    const keys = [
      "schemaVersion", "kind", "actionId", "agentId", "projectSessionId", "runId",
      "principalGeneration", "providerSessionRef", "providerSessionGeneration", "bridgeGeneration",
    ];
    if (Object.keys(params).length !== keys.length || keys.some((key) => !Object.hasOwn(params, key)) || params.schemaVersion !== 1) {
      throw new ProviderAdapterError("INVALID_PARAMS", "retained bridge health request does not match its closed schema");
    }
    const kind = params.kind;
    if (kind !== "chair" && kind !== "child") {
      throw new ProviderAdapterError("INVALID_PARAMS", "retained bridge health kind is invalid");
    }
    const actionId = requiredString(params.actionId, "actionId");
    const binding = kind === "chair" ? liveChairBindingByAction.get(actionId) : liveAgentSessionByAction.get(actionId);
    const exact = binding !== undefined &&
      binding.agentId === params.agentId &&
      binding.projectSessionId === params.projectSessionId &&
      binding.runId === params.runId &&
      binding.principalGeneration === params.principalGeneration &&
      binding.resumeReference === params.providerSessionRef &&
      binding.generation === params.providerSessionGeneration &&
      binding.bridgeGeneration === params.bridgeGeneration;
    const live = exact && (kind === "chair"
      ? options.boundary.hasLiveChairSession?.(binding.resumeReference, binding.generation) === true
      : options.boundary.hasLiveAgentSession?.(
          binding.resumeReference,
          binding.generation,
          binding.bridgeGeneration,
        ) === true);
    return { schemaVersion: 1, kind, live };
  }

  function promoteRetainedBridge(params: Record<string, unknown>): Record<string, unknown> {
    const keys = [
      "schemaVersion", "actionId", "sourceActionId", "agentId", "projectSessionId", "runId", "principalGeneration",
      "providerSessionRef", "providerSessionGeneration", "sourceBridgeGeneration", "chairBridgeGeneration",
    ];
    if (Object.keys(params).length !== keys.length || keys.some((key) => !Object.hasOwn(params, key)) || params.schemaVersion !== 1) {
      throw new ProviderAdapterError("INVALID_PARAMS", "retained bridge promotion request is invalid");
    }
    const actionId = requiredString(params.actionId, "actionId");
    const sourceActionId = requiredString(params.sourceActionId, "sourceActionId");
    const chairBinding = liveChairBindingByAction.get(actionId);
    const alreadyPromoted = chairBinding !== undefined &&
      chairBinding.agentId === params.agentId && chairBinding.projectSessionId === params.projectSessionId &&
      chairBinding.runId === params.runId && chairBinding.principalGeneration === params.principalGeneration &&
      chairBinding.resumeReference === params.providerSessionRef &&
      chairBinding.generation === params.providerSessionGeneration &&
      chairBinding.bridgeGeneration === params.chairBridgeGeneration &&
      Number.isSafeInteger(params.sourceBridgeGeneration) &&
      Number.isSafeInteger(params.chairBridgeGeneration) &&
      Number(params.chairBridgeGeneration) > Number(params.sourceBridgeGeneration) &&
      options.boundary.hasLiveChairSession?.(chairBinding.resumeReference, chairBinding.generation) === true;
    if (alreadyPromoted) return { schemaVersion: 1, promoted: true };
    const binding = liveAgentSessionByAction.get(sourceActionId);
    const exact = binding !== undefined &&
      binding.agentId === params.agentId && binding.projectSessionId === params.projectSessionId &&
      binding.runId === params.runId && binding.principalGeneration === params.principalGeneration &&
      binding.resumeReference === params.providerSessionRef && binding.generation === params.providerSessionGeneration &&
      binding.bridgeGeneration === params.sourceBridgeGeneration &&
      Number.isSafeInteger(params.chairBridgeGeneration) && Number(params.chairBridgeGeneration) > binding.bridgeGeneration &&
      options.boundary.hasLiveAgentSession?.(binding.resumeReference, binding.generation, binding.bridgeGeneration) === true;
    if (!exact || binding === undefined) return { schemaVersion: 1, promoted: false };
    liveAgentSessionByAction.delete(sourceActionId);
    liveChairLaunchActions.add(actionId);
    liveChairActionBySession.set(binding.resumeReference, actionId);
    liveChairSessionByAction.set(actionId, binding.resumeReference);
    liveChairBindingByAction.set(actionId, {
      ...binding,
      bridgeGeneration: Number(params.chairBridgeGeneration),
    });
    return { schemaVersion: 1, promoted: true };
  }

  return {
    async request(method: string, params: Record<string, unknown>): Promise<unknown> {
      if (method === "capabilities") return capabilities;
      if (method === "retained_bridge_health") return retainedBridgeHealth(params);
      if (method === "promote_retained_bridge") return promoteRetainedBridge(params);
      if (method === "launch_chair") return await launchChair(params);
      if (method === "recover_chair") return await recoverChair(params);
      if (method === "provision_agent") return await provisionAgent(params);
      if (method === "status") {
        return await options.boundary.status({
          ...(typeof params.providerSessionRef === "string"
            ? { resumeReference: params.providerSessionRef }
            : typeof params.resumeReference === "string"
              ? { resumeReference: params.resumeReference }
              : {}),
        });
      }
      if (method === "lookup_action") {
        return lookupAction(requiredString(params.actionId, "actionId"));
      }
      if (method === "cancel_action") {
        return options.journal.cancel(requiredString(params.actionId, "actionId"));
      }
      if (method === "resume_reference") {
        const resumeReference = params.resumeReference ?? params.providerSessionRef;
        return { resumeReference: requiredString(resumeReference, "resumeReference") };
      }
      if (method === "wakeup" || method === "follow_up" || method === "fork") {
        capabilityUnavailable(method);
      }
      if (method === "dispatch") {
        const operation = requiredString(params.operation, "operation");
        if (
          operation !== "send_turn" &&
          operation !== "steer" &&
          operation !== "interrupt" &&
          operation !== "release" &&
          operation !== "compact"
        ) {
          capabilityUnavailable(operation);
        }
        const executed = await effect(
          operation,
          requiredString(params.actionId, "actionId"),
          actionPayload(params),
        );
        return executed.record;
      }
      if (
        method === "spawn" ||
        method === "attach" ||
        method === "interrupt" ||
        method === "release" ||
        method === "compact"
      ) {
        const payload = actionPayload(params);
        const actionId = params.actionId;
        if (method === "release" && actionId === undefined && Object.keys(payload).length === 0) {
          return { released: true, deleted: false };
        }
        const executed = await effect(method, requiredString(actionId, "actionId"), payload);
        if (executed.record.status !== "terminal") {
          throw new ProviderAdapterError(
            "ACTION_RECONCILIATION_REQUIRED",
            `adapter action ${executed.record.actionId} is ${executed.record.status}; lookup is required before retry`,
            { actionId: executed.record.actionId, status: executed.record.status },
          );
        }
        options.noEffect?.throwIfResult(executed.result);
        return executed.result;
      }
      capabilityUnavailable(method);
    },
  };
}
