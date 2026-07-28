import { pathToFileURL } from "node:url";
import { isAbsolute } from "node:path";

import { createProviderAdapter, type ProviderBoundary } from "./adapter.js";
import {
  AgentSessionFabricBridge,
  type AgentSessionFabricBridgeInput,
} from "./agent-session-continuity.js";
import {
  chairLaunchContinuityUnproven,
  createChairLaunchFabricBridge,
  type ChairLaunchFabricBridge,
  type ChairLaunchFabricBridgeInput,
} from "./chair-launch-continuity.js";
import { CodexJsonRpcConnection, openVerifiedCodexJsonRpcConnection } from "./codex-json-rpc.js";
import { SqliteAdapterActionJournal } from "./journal.js";
import { journalPathFromArguments, serveAdapter } from "./server.js";
import {
  isRecord,
  optionalString,
  parseChairLaunchProviderResult,
  ProviderAdapterError,
  requiredString,
  takeChairLaunchHandoff,
  takeAgentBridgeHandoff,
  type AgentBridgeHandoff,
  type AgentProvisionBoundaryInput,
  type AgentProvisionProviderResult,
  type AdapterRequestHandler,
  type ChairLaunchBoundaryInput,
  type ChairRecoveryBoundaryInput,
  type ChairLaunchHandoff,
  type ChairLaunchProviderResult,
  type ProviderAdapterCapabilities,
} from "./types.js";
import type { ProviderSessionToolResult } from "./provider-session-fabric-surface.js";
import { parseWorkspaceWriteOfflineProjection } from "./workspace-write-offline.js";

export type CodexAppServerBoundary = ProviderBoundary & {
  steer(payload: Record<string, unknown>): Promise<Record<string, unknown>>;
};

export function codexAppServerCommand(executable: string): string[] {
  if (!isAbsolute(executable)) throw new TypeError("Codex provider executable must be absolute");
  return [executable, "app-server"];
}

const CAPABILITIES: ProviderAdapterCapabilities = {
  protocolVersion: 1,
  adapterId: "codex-app-server",
  operations: [
    "capabilities",
    "status",
    "spawn",
    "attach",
    "send_turn",
    "steer",
    "interrupt",
    "resume_reference",
    "lookup_action",
    "cancel_action",
    "release",
    "launch_chair",
    "recover_chair",
  ],
  actionJournal: true,
  persistentSession: true,
  ephemeralWorker: true,
  controlModes: ["managed"],
  inboxDeliveryModes: ["structured-push"],
  recoveryOperations: ["resume_reference", "lookup_action"],
  compactInPlace: false,
  idempotencyEvidence: "per-action-fail-closed",
  chairLaunch: {
    schemaVersion: 1,
    method: "launch_chair",
    inputSchemaId: "codex-app-server.chair-launch.v1",
    oneUse: true,
    secretTransport: "private-environment",
    environment: {
      capability: "AGENT_FABRIC_CAPABILITY",
      socketPath: "AGENT_FABRIC_SOCKET_PATH",
      attestationChallenge: "AGENT_FABRIC_ATTESTATION_CHALLENGE",
    },
    publicPayloadSchema: {
      type: "object",
      additionalProperties: false,
      // model is optional: a ChatGPT-subscription Codex account rejects
      // explicit model ids (HTTP 400) and dispatches on the account's
      // default model when the id is omitted (#190).
      required: ["cwd", "modelFamily", "prompt"],
      properties: {
        cwd: { type: "string", minLength: 1, pattern: "^/" },
        modelFamily: { type: "string", const: "openai" },
        model: { type: "string", minLength: 1 },
        prompt: { type: "string", minLength: 1 },
        developerInstructions: { type: "string", minLength: 1 },
        baseInstructions: { type: "string", minLength: 1 },
        serviceTier: { type: "string", minLength: 1 },
        ephemeral: { type: "boolean", const: false },
      },
    },
    noEffectProofSchemas: {},
    attestation: {
      method: "provider-session-random-challenge-v1",
      bridgeContract: "agent-fabric-session-bridge-v1",
      origin: "provider-session-tool-call",
      oneUse: true,
      bridgeLifetime: "provider-session",
      digestAlgorithm: "sha256",
      nativeAttribution: "codex-app-server-thread-turn-call-v1",
    },
  },
  agentBridge: {
    schemaVersion: 1,
    method: "provision_agent",
    operations: ["spawn", "attach"],
    secretTransport: "private-handoff",
    bridgeContract: "agent-fabric-session-bridge-v1",
    generationBound: true,
    providerOriginatedActivation: true,
  },
};

function validateCodexChairLaunchPayload(payload: Record<string, unknown>): Record<string, unknown> {
  const stringFields = ["developerInstructions", "baseInstructions", "serviceTier"] as const;
  const allowed = new Set(["cwd", "modelFamily", "model", "prompt", "ephemeral", ...stringFields]);
  if (Object.keys(payload).some((key) => !allowed.has(key))) {
    throw new ProviderAdapterError("INVALID_PARAMS", "Codex chair launch payload has unexpected fields");
  }
  const cwd = requiredString(payload.cwd, "cwd");
  if (!isAbsolute(cwd)) throw new ProviderAdapterError("INVALID_PARAMS", "cwd must be absolute");
  const modelFamily = requiredString(payload.modelFamily, "modelFamily");
  if (modelFamily !== "openai") {
    throw new ProviderAdapterError("INVALID_PARAMS", "Codex chair launch modelFamily must be openai");
  }
  const validated: Record<string, unknown> = {
    cwd,
    modelFamily,
    prompt: requiredString(payload.prompt, "prompt"),
  };
  // Optional: omitted for ChatGPT-subscription accounts, which reject
  // explicit model ids and use the account default (#190).
  copyString(payload, "model", validated);
  for (const field of stringFields) copyString(payload, field, validated);
  if (payload.ephemeral === false) validated.ephemeral = false;
  else if (payload.ephemeral !== undefined) {
    throw new ProviderAdapterError("INVALID_PARAMS", "chair launch cannot create an ephemeral Codex thread");
  }
  return validated;
}

function threadId(payload: Record<string, unknown>): string {
  return requiredString(payload.resumeReference ?? payload.threadId, "resumeReference");
}

function textInput(payload: Record<string, unknown>): { type: "text"; text: string }[] {
  return [{ type: "text", text: requiredString(payload.prompt ?? payload.instruction, "prompt") }];
}

function threadFromResponse(value: unknown, operation: string): Record<string, unknown> {
  if (!isRecord(value) || !isRecord(value.thread) || typeof value.thread.id !== "string") {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", `Codex ${operation} returned no thread ID`);
  }
  return value.thread;
}

function turnFromResponse(value: unknown): Record<string, unknown> {
  if (!isRecord(value) || !isRecord(value.turn) || typeof value.turn.id !== "string") {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Codex turn/start returned no turn ID");
  }
  return value.turn;
}

export function codexCompletedTurnResult(turn: Record<string, unknown>): string {
  if (turn.status !== "completed") {
    const detail = isRecord(turn.error) && typeof turn.error.message === "string"
      ? `: ${turn.error.message}`
      : "";
    throw new ProviderAdapterError("PROVIDER_TURN_FAILED", `Codex turn ended with status ${String(turn.status)}${detail}`);
  }
  if (!Array.isArray(turn.items)) {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Codex completed turn returned no item list");
  }
  const messages = turn.items.filter(
    (item): item is Record<string, unknown> => isRecord(item) && item.type === "agentMessage" && typeof item.text === "string",
  );
  const result = messages.at(-1)?.text;
  if (typeof result !== "string" || result.length === 0) {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Codex completed turn returned no agent message");
  }
  return result;
}

function copyString(payload: Record<string, unknown>, key: string, target: Record<string, unknown>): void {
  const value = optionalString(payload[key], key);
  if (value !== undefined) target[key] = value;
}

export function codexThreadConfiguration(
  payload: Record<string, unknown>,
  operation: "start" | "resume" = "start",
): Record<string, unknown> {
  const writeOffline = parseWorkspaceWriteOfflineProjection(payload);
  const configuration: Record<string, unknown> = {
    sandbox: "read-only",
    approvalPolicy: "never",
    ...(writeOffline === undefined ? {} : {
      runtimeWorkspaceRoots: [writeOffline.writeRoot],
      ...(operation === "start" ? { environments: [] } : {}),
    }),
  };
  for (const key of ["cwd", "model", "modelProvider", "developerInstructions", "baseInstructions", "serviceTier"]) {
    copyString(payload, key, configuration);
  }
  if (typeof payload.ephemeral === "boolean") configuration.ephemeral = payload.ephemeral;
  return configuration;
}

export function codexTurnContainment(payload: Record<string, unknown>): Record<string, unknown> {
  const writeOffline = parseWorkspaceWriteOfflineProjection(payload);
  if (writeOffline === undefined) {
    return { sandboxPolicy: { type: "readOnly", networkAccess: false } };
  }
  return {
    cwd: writeOffline.writeRoot,
    runtimeWorkspaceRoots: [writeOffline.writeRoot],
    environments: [],
    sandboxPolicy: {
      type: "workspaceWrite",
      writableRoots: [writeOffline.writeRoot],
      networkAccess: false,
      excludeTmpdirEnvVar: true,
      excludeSlashTmp: true,
    },
  };
}

type CodexConnection = Pick<
  CodexJsonRpcConnection,
  "initialize" | "request" | "waitForNotification" | "setServerRequestHandler" | "close" | "closed"
>;

type ConnectionFactory = (environment?: Record<string, string>) => CodexConnection | Promise<CodexConnection>;
type BridgeFactory = (input: ChairLaunchFabricBridgeInput) => Promise<ChairLaunchFabricBridge>;
type AgentBridgeFactory = (input: AgentSessionFabricBridgeInput) => Promise<AgentSessionFabricBridge>;

type CodexChairSession = {
  bridge: ChairLaunchFabricBridge | AgentSessionFabricBridge;
  providerSessionRef: string;
  providerSessionGeneration: number;
  nativeInvocationKeys: Set<string>;
  currentTurnId?: string;
  busy?: boolean;
  bridgeGeneration?: number;
};

function consumeCodexNativeInvocation(
  session: CodexChairSession,
  threadId: string,
  turnId: string,
  callId: string,
): boolean {
  const key = JSON.stringify([threadId, turnId, callId]);
  if (session.nativeInvocationKeys.has(key)) {
    throw new ProviderAdapterError(
      session.bridgeGeneration === undefined ? "CHAIR_CONTINUITY_UNPROVEN" : "AGENT_BRIDGE_UNPROVEN",
      "Codex replayed a native provider Fabric tool-call tuple",
    );
  }
  if (session.nativeInvocationKeys.size >= 256) return false;
  session.nativeInvocationKeys.add(key);
  return true;
}

function codexChairDynamicTools(bridge: ChairLaunchFabricBridge | AgentSessionFabricBridge): Record<string, unknown>[] {
  return bridge.descriptors.map((descriptor) => ({
    type: "function",
    name: descriptor.name,
    description: descriptor.description,
    inputSchema: descriptor.inputSchema,
    deferLoading: false,
  }));
}

function dynamicToolResponse(value: ProviderSessionToolResult): Record<string, unknown> {
  return {
    contentItems: [
      { type: "inputText", text: value.receipt },
      { type: "inputText", text: JSON.stringify(value.structuredContent) },
    ],
    success: true,
  };
}

export class InstalledCodexAppServerBoundary implements CodexAppServerBoundary {
  readonly #connectionFactory: ConnectionFactory;
  readonly #bridgeFactory: BridgeFactory;
  readonly #agentBridgeFactory: AgentBridgeFactory;
  readonly #connections = new Map<string, CodexConnection>();
  readonly #chairSessions = new Map<string, CodexChairSession>();

  constructor(
    connectionFactory: ConnectionFactory,
    bridgeFactory: BridgeFactory = createChairLaunchFabricBridge,
    agentBridgeFactory: AgentBridgeFactory = AgentSessionFabricBridge.create,
  ) {
    this.#connectionFactory = connectionFactory;
    this.#bridgeFactory = bridgeFactory;
    this.#agentBridgeFactory = agentBridgeFactory;
  }

  async #withConnection<T>(operation: (connection: CodexConnection) => Promise<T>): Promise<T> {
    const connection = await this.#connectionFactory();
    try {
      await connection.initialize();
      return await operation(connection);
    } finally {
      await connection.close();
    }
  }

  async #openConnection(environment?: Record<string, string>): Promise<CodexConnection> {
    const connection = await this.#connectionFactory(environment);
    await connection.initialize();
    return connection;
  }

  async #sessionConnection(resumeReference: string): Promise<CodexConnection> {
    const existing = this.#connections.get(resumeReference);
    if (existing !== undefined) return existing;
    const lostChair = this.#chairSessions.get(resumeReference);
    if (lostChair !== undefined) {
      this.#chairSessions.delete(resumeReference);
      await lostChair.bridge.close();
      throw new ProviderAdapterError(
        lostChair.bridgeGeneration === undefined ? "CHAIR_BRIDGE_LOST" : "AGENT_BRIDGE_LOST",
        "Codex provider connection was lost and cannot be recreated from its thread reference",
      );
    }
    const connection = await this.#openConnection();
    try {
      await connection.request("thread/resume", { threadId: resumeReference });
      this.#connections.set(resumeReference, connection);
      return connection;
    } catch (error: unknown) {
      await connection.close();
      throw error;
    }
  }

  async #completeTurn(
    connection: CodexConnection,
    resumeReference: string,
    payload: Record<string, unknown>,
    chairSession?: CodexChairSession,
    attestationToolName?: string,
  ): Promise<Record<string, unknown>> {
    if (chairSession !== undefined) chairSession.nativeInvocationKeys.clear();
    const invocationArguments = attestationToolName === undefined
      ? "{}"
      : chairSession !== undefined && chairSession.bridgeGeneration === undefined
        ? `{"challengeResponse":"${(chairSession.bridge as ChairLaunchFabricBridge).challengeResponse}"}`
        : chairSession !== undefined && (chairSession.bridge as AgentSessionFabricBridge).challengeResponse !== undefined
          ? `{"challengeResponse":"${(chairSession.bridge as AgentSessionFabricBridge).challengeResponse}"}`
          : "{}";
    const instruction = attestationToolName === undefined
      ? textInput(payload)
      : [{
          type: "text" as const,
          text: `Before continuing, invoke ${attestationToolName} exactly once with ${invocationArguments}. ${
            typeof (payload.prompt ?? payload.instruction) === "string"
              ? String(payload.prompt ?? payload.instruction)
              : "Establish the retained Agent Fabric bridge."
          }`,
        }];
    const response = await connection.request("turn/start", {
      threadId: resumeReference,
      input: instruction,
      ...codexTurnContainment(payload),
      ...(typeof payload.model === "string" ? { model: payload.model } : {}),
      ...(typeof payload.effort === "string" ? { effort: payload.effort } : {}),
    });
    const turn = turnFromResponse(response);
    if (chairSession !== undefined) chairSession.currentTurnId = String(turn.id);
    try {
      const completed = await connection.waitForNotification(
        "turn/completed",
        (params) => params.threadId === resumeReference && isRecord(params.turn) && params.turn.id === turn.id,
      );
      const completedTurn = isRecord(completed.turn) ? completed.turn : turn;
      if (completedTurn.status !== "completed") codexCompletedTurnResult(completedTurn);
      const readResponse = await connection.request("thread/read", { threadId: resumeReference, includeTurns: true });
      const hydratedThread = threadFromResponse(readResponse, "thread/read");
      const hydratedTurn = Array.isArray(hydratedThread.turns)
        ? hydratedThread.turns.find((candidate) => isRecord(candidate) && candidate.id === turn.id)
        : undefined;
      if (!isRecord(hydratedTurn)) {
        throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Codex thread/read returned no completed turn");
      }
      return {
        resumeReference,
        turnId: turn.id,
        status: hydratedTurn.status,
        result: codexCompletedTurnResult(hydratedTurn),
      };
    } finally {
      if (chairSession !== undefined && chairSession.currentTurnId === turn.id) delete chairSession.currentTurnId;
    }
  }

  async closeAll(): Promise<void> {
    const connections = [...this.#connections.values()];
    const bridges = [...this.#chairSessions.values()].map((session) => session.bridge);
    this.#connections.clear();
    this.#chairSessions.clear();
    await Promise.allSettled([
      ...connections.map(async (connection) => await connection.close()),
      ...bridges.map(async (bridge) => await bridge.close()),
    ]);
  }

  async status(input: { resumeReference?: string }): Promise<Record<string, unknown>> {
    if (input.resumeReference === undefined) return { healthy: true, providerSession: "unselected" };
    return await this.#withConnection(async (connection) => {
      const response = await connection.request("thread/read", { threadId: input.resumeReference, includeTurns: false });
      const thread = threadFromResponse(response, "thread/read");
      return { healthy: true, resumeReference: thread.id, status: thread.status ?? "unknown" };
    });
  }

  hasLiveChairSession(resumeReference: string, providerSessionGeneration: number): boolean {
    const session = this.#chairSessions.get(resumeReference);
    const connection = this.#connections.get(resumeReference);
    return (
      session !== undefined &&
      session.providerSessionGeneration === providerSessionGeneration &&
      !session.bridge.closed &&
      connection !== undefined &&
      !connection.closed
    );
  }

  hasLiveAgentSession(
    resumeReference: string,
    providerSessionGeneration: number,
    bridgeGeneration: number,
  ): boolean {
    const session = this.#chairSessions.get(resumeReference);
    const connection = this.#connections.get(resumeReference);
    return session !== undefined && session.bridgeGeneration !== undefined &&
      session.providerSessionGeneration === providerSessionGeneration &&
      session.bridgeGeneration === bridgeGeneration && !session.bridge.closed &&
      connection !== undefined && !connection.closed;
  }

  async spawn(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const connection = await this.#openConnection();
    try {
      const prior = optionalString(payload.priorResumeReference, "priorResumeReference");
      const response = prior === undefined
        ? await connection.request("thread/start", codexThreadConfiguration(payload, "start"))
        : await connection.request("thread/resume", { threadId: prior, ...codexThreadConfiguration(payload, "resume") });
      const thread = threadFromResponse(response, prior === undefined ? "thread/start" : "thread/resume");
      const resumeReference = String(thread.id);
      const completed = await this.#completeTurn(connection, resumeReference, payload);
      this.#connections.set(resumeReference, connection);
      return completed;
    } catch (error: unknown) {
      await connection.close();
      throw error;
    }
  }

  async launchChair(input: ChairLaunchBoundaryInput): Promise<ChairLaunchProviderResult> {
    const bridge = await this.#bridgeFactory({
      providerAdapterId: input.providerAdapterId,
      providerActionId: input.actionId,
      providerContractDigest: input.providerContractDigest,
      challengeDigest: input.challengeDigest,
      capability: input.environment.AGENT_FABRIC_CAPABILITY,
      socketPath: input.environment.AGENT_FABRIC_SOCKET_PATH,
      attestationChallenge: input.environment.AGENT_FABRIC_ATTESTATION_CHALLENGE,
      expectedPrincipal: input.expectedPrincipal,
    });
    let connection: CodexConnection | undefined;
    let evidence: {
      resumeReference: string;
      providerSessionGeneration: number;
      providerContractDigest: string;
    } | undefined;
    try {
      connection = await this.#openConnection();
      let chairSession: CodexChairSession | undefined;
      connection.setServerRequestHandler("item/tool/call", async (params) => {
        if (
          chairSession === undefined ||
          typeof params.threadId !== "string" ||
          params.threadId !== chairSession.providerSessionRef ||
          typeof params.turnId !== "string" ||
          params.turnId !== chairSession.currentTurnId ||
          typeof params.callId !== "string" ||
          params.callId.length === 0 ||
          Buffer.byteLength(params.callId, "utf8") > 512 ||
          typeof params.tool !== "string" ||
          (params.namespace !== undefined && params.namespace !== null)
        ) {
          throw new ProviderAdapterError("CHAIR_CONTINUITY_UNPROVEN", "Codex tool call is not attributable to the active chair turn");
        }
        if (!consumeCodexNativeInvocation(chairSession, params.threadId, params.turnId, params.callId)) {
          await bridge.close();
          await connection?.close();
          throw new ProviderAdapterError(
            "CHAIR_BRIDGE_LOST",
            "Codex native provider tool-call capacity was exceeded",
          );
        }
        if (!isRecord(params.arguments)) {
          throw new ProviderAdapterError("MCP_INPUT_INVALID", "Codex Fabric tool arguments must be an object");
        }
        return dynamicToolResponse(await bridge.invokeTool(params.tool, params.arguments, {
          providerSessionRef: chairSession.providerSessionRef,
          providerSessionGeneration: chairSession.providerSessionGeneration,
          providerTurnRef: params.turnId,
          providerInvocationRef: params.callId,
        }));
      });
      const response = await connection.request("thread/start", {
        ...codexThreadConfiguration(input.payload, "start"),
        dynamicTools: codexChairDynamicTools(bridge),
      });
      const thread = threadFromResponse(response, "thread/start");
      const resumeReference = String(thread.id);
      bridge.bindProviderSession(resumeReference, 1);
      chairSession = {
        bridge,
        providerSessionRef: resumeReference,
        providerSessionGeneration: 1,
        nativeInvocationKeys: new Set(),
      };
      evidence = {
        resumeReference,
        providerSessionGeneration: 1,
        providerContractDigest: input.providerContractDigest,
      };
      await this.#completeTurn(connection, resumeReference, input.payload, chairSession, bridge.challengeToolName);
      const result = parseChairLaunchProviderResult(await bridge.result(), {
        providerAdapterId: input.providerAdapterId,
        providerActionId: input.actionId,
        providerContractDigest: input.providerContractDigest,
        challengeDigest: input.challengeDigest,
      });
      this.#connections.set(resumeReference, connection);
      this.#chairSessions.set(resumeReference, chairSession);
      return result;
    } catch (error: unknown) {
      await connection?.close();
      await bridge.close();
      if (evidence !== undefined) throw chairLaunchContinuityUnproven(evidence);
      throw error;
    }
  }

  async recoverChair(input: ChairRecoveryBoundaryInput): Promise<ChairLaunchProviderResult> {
    const bridge = await this.#bridgeFactory({
      providerAdapterId: input.providerAdapterId,
      providerActionId: input.actionId,
      providerContractDigest: input.providerContractDigest,
      challengeDigest: input.challengeDigest,
      capability: input.environment.AGENT_FABRIC_CAPABILITY,
      socketPath: input.environment.AGENT_FABRIC_SOCKET_PATH,
      attestationChallenge: input.environment.AGENT_FABRIC_ATTESTATION_CHALLENGE,
      expectedPrincipal: input.expectedPrincipal,
    });
    let connection: CodexConnection | undefined;
    try {
      connection = await this.#openConnection();
      let session: CodexChairSession | undefined;
      connection.setServerRequestHandler("item/tool/call", async (params) => {
        if (
          session === undefined || params.threadId !== session.providerSessionRef ||
          params.turnId !== session.currentTurnId || typeof params.callId !== "string" ||
          params.callId.length === 0 || Buffer.byteLength(params.callId, "utf8") > 512 ||
          typeof params.tool !== "string" || (params.namespace !== undefined && params.namespace !== null) ||
          !isRecord(params.arguments)
        ) throw new ProviderAdapterError("CHAIR_CONTINUITY_UNPROVEN", "Codex recovery tool call is not attributable");
        if (!consumeCodexNativeInvocation(session, String(params.threadId), String(params.turnId), params.callId)) {
          throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "Codex recovery invocation capacity exceeded");
        }
        return dynamicToolResponse(await bridge.invokeTool(params.tool, params.arguments, {
          providerSessionRef: session.providerSessionRef,
          providerSessionGeneration: session.providerSessionGeneration,
          providerTurnRef: String(params.turnId),
          providerInvocationRef: params.callId,
        }));
      });
      const response = await connection.request("thread/resume", {
        threadId: input.resumeReference,
        ...codexThreadConfiguration(input.payload, "resume"),
        dynamicTools: codexChairDynamicTools(bridge),
      });
      const thread = threadFromResponse(response, "thread/resume");
      const resumeReference = String(thread.id);
      if (resumeReference !== input.resumeReference) {
        throw new ProviderAdapterError("CHAIR_CONTINUITY_UNPROVEN", "Codex recovery resumed another thread");
      }
      bridge.bindProviderSession(resumeReference, input.nextProviderSessionGeneration);
      session = {
        bridge,
        providerSessionRef: resumeReference,
        providerSessionGeneration: input.nextProviderSessionGeneration,
        nativeInvocationKeys: new Set(),
      };
      await this.#completeTurn(connection, resumeReference, input.payload, session, bridge.challengeToolName);
      const result = parseChairLaunchProviderResult(await bridge.result(), {
        providerAdapterId: input.providerAdapterId,
        providerActionId: input.actionId,
        providerContractDigest: input.providerContractDigest,
        challengeDigest: input.challengeDigest,
      });
      this.#connections.set(resumeReference, connection);
      this.#chairSessions.set(resumeReference, session);
      return result;
    } catch (error: unknown) {
      await connection?.close();
      await bridge.close();
      throw error;
    }
  }

  async provisionAgent(input: AgentProvisionBoundaryInput): Promise<AgentProvisionProviderResult> {
    const providerSessionGeneration = typeof input.payload.generation === "number" &&
      Number.isSafeInteger(input.payload.generation) && input.payload.generation > 0
      ? input.payload.generation : 1;
    const bridge = await this.#agentBridgeFactory({
      providerAdapterId: "codex-app-server",
      providerActionId: input.actionId,
      targetAgentId: input.targetAgentId,
      expectedPrincipal: input.expectedPrincipal,
      bridgeGeneration: input.bridgeGeneration,
      bridgeContractDigest: input.bridgeContractDigest,
      capability: input.environment.AGENT_FABRIC_CAPABILITY,
      socketPath: input.environment.AGENT_FABRIC_SOCKET_PATH,
      ...(input.environment.AGENT_FABRIC_ATTESTATION_CHALLENGE === undefined ? {} : {
        lifecycleAttestation: {
          challenge: input.environment.AGENT_FABRIC_ATTESTATION_CHALLENGE,
          challengeDigest: input.environment.AGENT_FABRIC_ATTESTATION_CHALLENGE_DIGEST as string,
          custodyId: input.environment.AGENT_FABRIC_LIFECYCLE_CUSTODY_ID as string,
          checkpointDigest: input.environment.AGENT_FABRIC_LIFECYCLE_CHECKPOINT_DIGEST as string,
        },
      }),
    });
    let connection: CodexConnection | undefined;
    try {
      connection = await this.#openConnection();
      let session: CodexChairSession | undefined;
      connection.setServerRequestHandler("item/tool/call", async (params) => {
        if (
          session === undefined || typeof params.threadId !== "string" ||
          params.threadId !== session.providerSessionRef || typeof params.turnId !== "string" ||
          params.turnId !== session.currentTurnId || typeof params.callId !== "string" ||
          params.callId.length === 0 || Buffer.byteLength(params.callId, "utf8") > 512 ||
          typeof params.tool !== "string" || (params.namespace !== undefined && params.namespace !== null) ||
          !isRecord(params.arguments)
        ) throw new ProviderAdapterError("AGENT_BRIDGE_UNPROVEN", "Codex child tool call is not attributable");
        if (!consumeCodexNativeInvocation(session, params.threadId, params.turnId, params.callId)) {
          throw new ProviderAdapterError("AGENT_BRIDGE_LOST", "Codex child invocation capacity exceeded");
        }
        return dynamicToolResponse(await bridge.invokeTool(params.tool, params.arguments, {
          providerSessionRef: session.providerSessionRef,
          providerSessionGeneration: session.providerSessionGeneration,
          providerTurnRef: params.turnId,
          providerInvocationRef: params.callId,
        }));
      });
      const response = input.operation === "spawn"
        ? await connection.request("thread/start", {
            ...codexThreadConfiguration(input.payload, "start"),
            dynamicTools: codexChairDynamicTools(bridge),
          })
        : await connection.request("thread/resume", {
            threadId: requiredString(input.providerSessionRef, "providerSessionRef"),
            ...codexThreadConfiguration(input.payload, "resume"),
            dynamicTools: codexChairDynamicTools(bridge),
          });
      const thread = threadFromResponse(response, input.operation === "spawn" ? "thread/start" : "thread/resume");
      const resumeReference = String(thread.id);
      bridge.bindProviderSession(resumeReference, providerSessionGeneration);
      session = {
        bridge,
        providerSessionRef: resumeReference,
        providerSessionGeneration,
        bridgeGeneration: input.bridgeGeneration,
        nativeInvocationKeys: new Set(),
      };
      await this.#completeTurn(connection, resumeReference, input.payload, session, bridge.activationToolName);
      const result = bridge.result();
      this.#connections.set(resumeReference, connection);
      this.#chairSessions.set(resumeReference, session);
      return result;
    } catch (error: unknown) {
      await connection?.close();
      await bridge.close();
      throw error;
    }
  }

  async attach(input: { resumeReference: string; payload: Record<string, unknown> }): Promise<Record<string, unknown>> {
    const connection = await this.#openConnection();
    try {
      const response = await connection.request("thread/resume", {
        threadId: input.resumeReference,
        ...codexThreadConfiguration(input.payload, "resume"),
      });
      const thread = threadFromResponse(response, "thread/resume");
      this.#connections.set(String(thread.id), connection);
      return { resumeReference: thread.id, attached: true };
    } catch (error: unknown) {
      await connection.close();
      throw error;
    }
  }

  async sendTurn(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resumeReference = threadId(payload);
    const connection = await this.#sessionConnection(resumeReference);
    const chairSession = this.#chairSessions.get(resumeReference);
    if (chairSession?.busy === true) {
      throw new ProviderAdapterError("PROVIDER_SESSION_BUSY", "Codex chair session already has an active turn");
    }
    if (chairSession !== undefined) chairSession.busy = true;
    try {
      return await this.#completeTurn(connection, resumeReference, payload, chairSession);
    } finally {
      if (chairSession !== undefined) chairSession.busy = false;
    }
  }

  async steer(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resumeReference = threadId(payload);
    const expectedTurnId = requiredString(payload.expectedTurnId ?? payload.turnId, "expectedTurnId");
    const connection = await this.#sessionConnection(resumeReference);
    return await (async () => {
      const response = await connection.request("turn/steer", {
        threadId: resumeReference,
        expectedTurnId,
        input: textInput(payload),
      });
      if (!isRecord(response) || typeof response.turnId !== "string") {
        throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Codex turn/steer returned no turn ID");
      }
      return { resumeReference, turnId: response.turnId, steered: true };
    })();
  }

  async interrupt(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resumeReference = threadId(payload);
    const turnIdValue = requiredString(payload.turnId, "turnId");
    const connection = await this.#sessionConnection(resumeReference);
    return await (async () => {
      await connection.request("turn/interrupt", { threadId: resumeReference, turnId: turnIdValue });
      return { resumeReference, turnId: turnIdValue, interrupted: true };
    })();
  }

  async release(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resumeReference = optionalString(payload.resumeReference, "resumeReference");
    if (resumeReference !== undefined) {
      const connection = this.#connections.get(resumeReference);
      const chairSession = this.#chairSessions.get(resumeReference);
      this.#connections.delete(resumeReference);
      this.#chairSessions.delete(resumeReference);
      await connection?.close();
      await chairSession?.bridge.close();
    }
    return { released: true, deleted: false, ...(resumeReference === undefined ? {} : { resumeReference }) };
  }
}

export function createCodexAppServerAdapter(options: {
  boundary: CodexAppServerBoundary;
  journal: SqliteAdapterActionJournal;
  chairLaunchHandoff?: ChairLaunchHandoff;
  agentBridgeHandoff?: AgentBridgeHandoff;
}): AdapterRequestHandler {
  return createProviderAdapter({
    capabilities: CAPABILITIES,
    boundary: options.boundary,
    journal: options.journal,
    chairLaunch: {
      ...(options.chairLaunchHandoff === undefined ? {} : { handoff: options.chairLaunchHandoff }),
      validatePayload: validateCodexChairLaunchPayload,
    },
    agentBridge: {
      ...(options.agentBridgeHandoff === undefined ? {} : { handoff: options.agentBridgeHandoff }),
    },
  });
}

export async function runCodexAppServerAdapter(arguments_: string[] = process.argv.slice(2)): Promise<void> {
  const journal = new SqliteAdapterActionJournal(journalPathFromArguments("codex-app-server", arguments_));
  const chairLaunchHandoff = takeChairLaunchHandoff(process.env);
  const agentBridgeHandoff = takeAgentBridgeHandoff(process.env);
  const providerIndex = arguments_.indexOf("--provider-executable");
  const providerExecutable = providerIndex === -1 ? undefined : arguments_[providerIndex + 1];
  if (providerExecutable === undefined) throw new Error("codex-app-server adapter requires --provider-executable");
  const boundary = new InstalledCodexAppServerBoundary(
    async (environment) => await openVerifiedCodexJsonRpcConnection(
      codexAppServerCommand(providerExecutable),
      environment,
    ),
  );
  try {
    await serveAdapter(
      createCodexAppServerAdapter({
        boundary,
        journal,
        ...(chairLaunchHandoff === undefined ? {} : { chairLaunchHandoff }),
        ...(agentBridgeHandoff === undefined ? {} : { agentBridgeHandoff }),
      }),
      { input: process.stdin, output: process.stdout },
    );
  } finally {
    await boundary.closeAll();
    journal.close();
  }
}

const invokedPath = process.argv[1];
if (invokedPath !== undefined && import.meta.url === pathToFileURL(invokedPath).href) {
  await runCodexAppServerAdapter();
}
