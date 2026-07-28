import { realpath } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

import {
  AgentSessionFabricBridge,
  type AgentSessionFabricBridgeInput,
} from "./agent-session-continuity.js";
import {
  getSessionInfo,
  query,
  type Options,
  type SDKMessage,
} from "@anthropic-ai/claude-agent-sdk";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import type { McpToolDescriptor } from "@local/agent-fabric-protocol";

import { createProviderAdapter, type ProviderBoundary } from "./adapter.js";
import {
  claudeSubscriptionNoEffectResult,
  consumeClaudeQuery,
  throwClaudeSubscriptionNoEffect,
} from "./claude-agent-sdk-query.js";
import {
  chairLaunchContinuityUnproven,
  createChairLaunchFabricBridge,
  type ChairLaunchFabricBridge,
  type ChairLaunchFabricBridgeInput,
} from "./chair-launch-continuity.js";
import { SqliteAdapterActionJournal } from "./journal.js";
import { journalPathFromArguments, serveAdapter } from "./server.js";
import {
  canonicalJson,
  optionalString,
  isRecord,
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
import {
  parseWorkspaceWriteOfflineProjection,
  WORKSPACE_WRITE_OFFLINE_TOOLS,
} from "./workspace-write-offline.js";
import { verifyProviderConformance } from "../provider-conformance.js";

export type ClaudeAgentSdkBoundary = ProviderBoundary;

const CAPABILITIES: ProviderAdapterCapabilities = {
  protocolVersion: 1,
  adapterId: "claude-agent-sdk",
  operations: [
    "capabilities",
    "status",
    "spawn",
    "attach",
    "send_turn",
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
  answerBearingSpawn: true,
  answerBearingSpawnTurns: "payload-max-turns",
  answerBearingUsageUnits: ["cost:USD", "input_tokens:anthropic", "output_tokens:anthropic"],
  controlModes: ["managed"],
  inboxDeliveryModes: ["structured-push"],
  recoveryOperations: ["resume_reference", "lookup_action"],
  compactInPlace: false,
  idempotencyEvidence: "per-action-fail-closed",
  chairLaunch: {
    schemaVersion: 1,
    method: "launch_chair",
    inputSchemaId: "claude-agent-sdk.chair-launch.v1",
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
      required: ["cwd", "modelFamily", "model", "prompt"],
      properties: {
        cwd: { type: "string", minLength: 1, pattern: "^/" },
        modelFamily: { type: "string", const: "anthropic" },
        model: { type: "string", minLength: 1 },
        prompt: { type: "string", minLength: 1 },
        maxTurns: { type: "integer", minimum: 1 },
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
      nativeAttribution: "claude-sdk-assistant-request-tool-use-v1",
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

const CLAUDE_EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;
const CLAUDE_READ_ONLY_TOOLS = ["Read", "Glob", "Grep"] as const;

function claudeEffort(value: unknown): (typeof CLAUDE_EFFORTS)[number] | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || !CLAUDE_EFFORTS.includes(value as (typeof CLAUDE_EFFORTS)[number])) {
    throw new ProviderAdapterError("INVALID_PARAMS", "effort must be one of low, medium, high, xhigh, max");
  }
  return value as (typeof CLAUDE_EFFORTS)[number];
}

function insideRoot(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (!path.startsWith(`..${sep}`) && path !== ".." && !isAbsolute(path));
}

function boundedPath(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 && Buffer.byteLength(value, "utf8") <= 32_768
    ? value
    : undefined;
}

function safeGlobPattern(value: unknown): boolean {
  if (typeof value !== "string" || value.length === 0 || Buffer.byteLength(value, "utf8") > 4096 || isAbsolute(value)) {
    return false;
  }
  return !value.split(/[\\/]/u).includes("..");
}

function claudeReadOnlyPermission(root: string): NonNullable<Options["canUseTool"]> {
  return async (toolName, input, context) => {
    if (!CLAUDE_READ_ONLY_TOOLS.includes(toolName as (typeof CLAUDE_READ_ONLY_TOOLS)[number])) {
      return { behavior: "deny", message: "provider review permits only path-bounded read tools", toolUseID: context.toolUseID };
    }
    if (toolName === "Glob" && !safeGlobPattern(input.pattern)) {
      return { behavior: "deny", message: "provider review glob is outside the bounded read profile", toolUseID: context.toolUseID };
    }
    const requested = toolName === "Read"
      ? boundedPath(input.file_path)
      : input.path === undefined
        ? root
        : boundedPath(input.path);
    if (requested === undefined) {
      return { behavior: "deny", message: "provider review read path is missing or invalid", toolUseID: context.toolUseID };
    }
    try {
      const [canonicalRoot, canonicalRequested] = await Promise.all([
        realpath(root),
        realpath(isAbsolute(requested) ? requested : resolve(root, requested)),
      ]);
      if (!insideRoot(canonicalRoot, canonicalRequested)) {
        return { behavior: "deny", message: "provider review read path is outside delegated authority", toolUseID: context.toolUseID };
      }
    } catch {
      return { behavior: "deny", message: "provider review read path cannot be verified", toolUseID: context.toolUseID };
    }
    return { behavior: "allow", updatedInput: input, toolUseID: context.toolUseID };
  };
}

function validateClaudeChairLaunchPayload(payload: Record<string, unknown>): Record<string, unknown> {
  const allowed = new Set(["cwd", "modelFamily", "model", "prompt", "maxTurns", "effort"]);
  if (Object.keys(payload).some((key) => !allowed.has(key))) {
    throw new ProviderAdapterError("INVALID_PARAMS", "Claude chair launch payload has unexpected fields");
  }
  const cwd = requiredString(payload.cwd, "cwd");
  if (!isAbsolute(cwd)) throw new ProviderAdapterError("INVALID_PARAMS", "cwd must be absolute");
  const modelFamily = requiredString(payload.modelFamily, "modelFamily");
  if (modelFamily !== "anthropic") {
    throw new ProviderAdapterError("INVALID_PARAMS", "Claude chair launch modelFamily must be anthropic");
  }
  const validated: Record<string, unknown> = {
    cwd,
    modelFamily,
    model: requiredString(payload.model, "model"),
    prompt: requiredString(payload.prompt, "prompt"),
  };
  const maxTurns = positiveInteger(payload.maxTurns, "maxTurns");
  if (maxTurns !== undefined) validated.maxTurns = maxTurns;
  const effort = claudeEffort(payload.effort);
  if (effort !== undefined) validated.effort = effort;
  return validated;
}

function positiveInteger(value: unknown, field: string): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isInteger(value) || typeof value !== "number" || value <= 0) {
    throw new ProviderAdapterError("INVALID_PARAMS", `${field} must be a positive integer`);
  }
  return value;
}

function minimalWriteOfflineEnvironment(): Record<string, string> {
  return {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    TMPDIR: process.env.TMPDIR ?? "/tmp",
    ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
  };
}

export function claudeReadOnlyOptions(
  payload: Record<string, unknown>,
  resume?: string,
  executable?: string,
  environment?: Record<string, string>,
): Options {
  const cwd = optionalString(payload.cwd, "cwd");
  const readOnlyRoot = optionalString(payload.readOnlyRoot, "readOnlyRoot") ?? cwd;
  if ((cwd === undefined) !== (readOnlyRoot === undefined)) {
    throw new ProviderAdapterError("INVALID_PARAMS", "Claude read-only root and cwd must be supplied together");
  }
  if (cwd !== undefined && (readOnlyRoot === undefined || !isAbsolute(cwd) || resolve(readOnlyRoot) !== resolve(cwd))) {
    throw new ProviderAdapterError("INVALID_PARAMS", "Claude read-only root must equal the absolute admitted cwd");
  }
  const model = optionalString(payload.model, "model");
  const maxTurns = positiveInteger(payload.maxTurns, "maxTurns");
  const effort = claudeEffort(payload.effort);
  return {
    ...(cwd === undefined ? {} : { cwd }),
    ...(model === undefined ? {} : { model }),
    ...(maxTurns === undefined ? {} : { maxTurns }),
    ...(effort === undefined ? {} : { effort }),
    ...(resume === undefined ? {} : { resume }),
    ...(executable === undefined ? {} : { pathToClaudeCodeExecutable: executable }),
    ...(environment === undefined ? {} : { env: { ...process.env, ...environment } }),
    tools: cwd === undefined ? [] : [...CLAUDE_READ_ONLY_TOOLS],
    ...(readOnlyRoot === undefined ? {} : { canUseTool: claudeReadOnlyPermission(readOnlyRoot) }),
    permissionMode: "plan",
    ...(readOnlyRoot === undefined ? {} : { settings: {
      permissions: {
        defaultMode: "plan",
        disableBypassPermissionsMode: "disable",
        additionalDirectories: [],
      },
    } }),
    settingSources: [],
    skills: [],
    plugins: [],
  };
}

function claudeWorkspaceWriteOfflineOptions(
  payload: Record<string, unknown>,
  resume?: string,
  executable?: string,
): Options {
  const projection = parseWorkspaceWriteOfflineProjection(payload);
  if (projection === undefined) {
    throw new ProviderAdapterError("INVALID_PARAMS", "write-offline projection is absent");
  }
  const model = optionalString(payload.model, "model");
  const maxTurns = positiveInteger(payload.maxTurns, "maxTurns");
  const effort = claudeEffort(payload.effort);
  const root = projection.writeRoot;
  return {
    cwd: root,
    ...(model === undefined ? {} : { model }),
    ...(maxTurns === undefined ? {} : { maxTurns }),
    ...(effort === undefined ? {} : { effort }),
    ...(resume === undefined ? {} : { resume }),
    ...(executable === undefined ? {} : { pathToClaudeCodeExecutable: executable }),
    env: minimalWriteOfflineEnvironment(),
    tools: [...WORKSPACE_WRITE_OFFLINE_TOOLS],
    permissionMode: "acceptEdits",
    sandbox: {
      enabled: true,
      failIfUnavailable: true,
      autoAllowBashIfSandboxed: true,
      allowUnsandboxedCommands: false,
      filesystem: {
        allowWrite: [root],
        denyWrite: [resolve(root, ".git")],
        allowRead: [root],
        allowManagedReadPathsOnly: true,
      },
      network: {
        allowedDomains: [],
        allowManagedDomainsOnly: true,
        allowUnixSockets: [],
        allowAllUnixSockets: false,
        allowLocalBinding: false,
      },
    },
    settings: {
      permissions: {
        defaultMode: "acceptEdits",
        disableBypassPermissionsMode: "disable",
        additionalDirectories: [],
      },
    },
    settingSources: [],
    skills: [],
    plugins: [],
  };
}

export function claudeProviderOptions(
  payload: Record<string, unknown>,
  resume?: string,
  executable?: string,
  environment?: Record<string, string>,
): Options {
  return payload.executionProfile === undefined
    ? claudeReadOnlyOptions(payload, resume, executable, environment)
    : claudeWorkspaceWriteOfflineOptions(payload, resume, executable);
}

function prompt(payload: Record<string, unknown>): string {
  return requiredString(payload.prompt ?? payload.instruction ?? payload.initialPrompt, "prompt");
}

function claudeResourceUsage(completed: {
  usage: unknown;
  costUsd: number;
  numTurns: unknown;
}): Record<string, number> {
  if (!isRecord(completed.usage)) {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Claude Agent SDK omitted token usage");
  }
  const uncachedInputTokens = completed.usage.input_tokens;
  const outputTokens = completed.usage.output_tokens;
  const cacheCreationInputTokens = completed.usage.cache_creation_input_tokens ?? 0;
  const cacheReadInputTokens = completed.usage.cache_read_input_tokens ?? 0;
  if (
    typeof uncachedInputTokens !== "number" || !Number.isSafeInteger(uncachedInputTokens) || uncachedInputTokens < 0 ||
    typeof cacheCreationInputTokens !== "number" || !Number.isSafeInteger(cacheCreationInputTokens) || cacheCreationInputTokens < 0 ||
    typeof cacheReadInputTokens !== "number" || !Number.isSafeInteger(cacheReadInputTokens) || cacheReadInputTokens < 0 ||
    typeof outputTokens !== "number" || !Number.isSafeInteger(outputTokens) || outputTokens < 0 ||
    typeof completed.numTurns !== "number" || !Number.isSafeInteger(completed.numTurns) || completed.numTurns < 1 ||
    typeof completed.costUsd !== "number" || !Number.isFinite(completed.costUsd) || completed.costUsd < 0
  ) {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Claude Agent SDK returned invalid resource usage");
  }
  const inputTokens = uncachedInputTokens + cacheCreationInputTokens + cacheReadInputTokens;
  const costMicrounits = Math.ceil(completed.costUsd * 1_000_000);
  if (!Number.isSafeInteger(inputTokens) || !Number.isSafeInteger(costMicrounits)) {
    throw new ProviderAdapterError("PROVIDER_RESPONSE_INVALID", "Claude Agent SDK cost exceeds safe microunit range");
  }
  return {
    "cost:USD": costMicrounits,
    "input_tokens:anthropic": inputTokens,
    "output_tokens:anthropic": outputTokens,
    turns: completed.numTurns,
  };
}

type ClaudeChairSession = {
  bridge: ChairLaunchFabricBridge | AgentSessionFabricBridge;
  mcp?: ClaudeChairMcpBridge;
  providerSessionRef?: string;
  providerSessionGeneration: number;
  nativeFabricInvocations: ClaudeNativeFabricInvocation[];
  seenNativeFabricInvocationKeys: Set<string>;
  seenNativeFabricInvocationOrder: string[];
  attested?: boolean;
  busy?: boolean;
  bridgeGeneration?: number;
};

type ClaudeNativeFabricInvocation = {
  toolName: string;
  providerTurnRef: string;
  providerInvocationRef: string;
  input: Record<string, unknown>;
};

export type ClaudeChairMcpBridge = {
  serverName: string;
  server: { type: "sdk"; name: string; instance: McpServer };
  descriptors: readonly McpToolDescriptor[];
  allowedToolNames: readonly string[];
  attestationToolName: string;
  mailboxToolName: string;
  providerToolName(name: string): McpToolDescriptor["name"] | undefined;
  invokeTool(name: McpToolDescriptor["name"], args: Record<string, unknown>): Promise<{
    content: readonly [{ type: "text"; text: string }];
    structuredContent: Record<string, unknown>;
  }>;
};

function boundedClaudeNativeRef(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && Buffer.byteLength(value, "utf8") <= 512;
}

function isFabricToolName(value: string): value is McpToolDescriptor["name"] {
  return /^fabric_[a-z0-9_]+$/u.test(value);
}

function observeClaudeFabricToolUses(
  session: ClaudeChairSession,
  message: SDKMessage,
  mcp: ClaudeChairMcpBridge,
): void {
  if (
    message.type !== "assistant" ||
    !isRecord(message.message) ||
    !Array.isArray(message.message.content)
  ) return;
  const turnRef = typeof message.request_id === "string" && message.request_id.length > 0
    ? message.request_id
    : message.uuid;
  if (
    session.providerSessionRef === undefined ||
    message.session_id !== session.providerSessionRef ||
    !boundedClaudeNativeRef(turnRef)
  ) return;
  for (const block of message.message.content) {
    const providerToolName = isRecord(block) && typeof block.name === "string"
      ? mcp.providerToolName(block.name)
      : undefined;
    if (
      !isRecord(block) ||
      block.type !== "tool_use" ||
      providerToolName === undefined ||
      !boundedClaudeNativeRef(block.id) ||
      !isRecord(block.input)
    ) continue;
    const invocationKey = `${turnRef}\0${block.id}`;
    if (session.seenNativeFabricInvocationKeys.has(invocationKey)) {
      throw new ProviderAdapterError(
        "CHAIR_CONTINUITY_UNPROVEN",
        "Claude replayed a native Fabric tool-use record",
      );
    }
    const serializedInput = canonicalJson(block.input);
    if (Buffer.byteLength(serializedInput, "utf8") > 4_096 || session.nativeFabricInvocations.length >= 32) {
      throw new ProviderAdapterError(
        "CHAIR_CONTINUITY_UNPROVEN",
        "Claude emitted too many or oversized native Fabric tool records",
      );
    }
    session.nativeFabricInvocations.push({
      toolName: providerToolName,
      providerTurnRef: turnRef,
      providerInvocationRef: block.id,
      input: block.input,
    });
    session.seenNativeFabricInvocationKeys.add(invocationKey);
    session.seenNativeFabricInvocationOrder.push(invocationKey);
    if (session.seenNativeFabricInvocationOrder.length > 256) {
      const expired = session.seenNativeFabricInvocationOrder.shift();
      if (expired !== undefined) session.seenNativeFabricInvocationKeys.delete(expired);
    }
  }
}

function consumeClaudeFabricInvocation(
  session: ClaudeChairSession,
  toolName: string,
  input: Record<string, unknown>,
): ClaudeNativeFabricInvocation {
  const index = session.nativeFabricInvocations.findIndex((candidate) => candidate.toolName === toolName);
  if (index === -1) {
    throw new ProviderAdapterError(
      "CHAIR_CONTINUITY_UNPROVEN",
      "Claude MCP invocation lacks a matching native assistant tool-use record",
    );
  }
  const [invocation] = session.nativeFabricInvocations.splice(index, 1);
  if (invocation === undefined || canonicalJson(invocation.input) !== canonicalJson(input)) {
    throw new ProviderAdapterError(
      "CHAIR_CONTINUITY_UNPROVEN",
      "Claude MCP invocation does not match its native assistant tool-use input",
    );
  }
  return invocation;
}

export function createClaudeChairMcpBridge(session: ClaudeChairSession): ClaudeChairMcpBridge {
  const serverName = "agent_fabric_session";
  const descriptors = session.bridge.descriptors;
  const descriptorsByName = new Map(descriptors.map((descriptor) => [descriptor.name, descriptor]));
  const qualifiedByName = new Map(descriptors.map((descriptor) => [
    descriptor.name,
    `mcp__${serverName}__${descriptor.name}`,
  ]));
  const nameByQualified = new Map([...qualifiedByName.entries()].map(([name, qualified]) => [qualified, name]));
  const requiredToolName = session.bridgeGeneration === undefined
    ? (session.bridge as ChairLaunchFabricBridge).challengeToolName
    : (session.bridge as AgentSessionFabricBridge).activationToolName;
  const attestationToolName = qualifiedByName.get(requiredToolName);
  const mailboxToolName = qualifiedByName.get("fabric_mailbox_read");
  if (attestationToolName === undefined || mailboxToolName === undefined) {
    throw new ProviderAdapterError("CAPABILITY_UNAVAILABLE", "Claude chair launch grant lacks required Fabric descriptors");
  }
  const instance = new McpServer(
    { name: serverName, version: "1.0.0" },
    { capabilities: { tools: {} } },
  );
  const bridge: ClaudeChairMcpBridge = {
    serverName,
    server: { type: "sdk", name: serverName, instance },
    descriptors,
    allowedToolNames: [...qualifiedByName.values()],
    attestationToolName,
    mailboxToolName,
    providerToolName(name) {
      return nameByQualified.get(name);
    },
    async invokeTool(name, args) {
      const descriptor = descriptorsByName.get(name);
      if (descriptor === undefined || session.providerSessionRef === undefined) {
        throw new ProviderAdapterError("CHAIR_CONTINUITY_UNPROVEN", "Claude MCP invocation lacks native session, turn or tool-call evidence");
      }
      const invocation = consumeClaudeFabricInvocation(session, name, args);
      const result = await session.bridge.invokeTool(name, args, {
        providerSessionRef: session.providerSessionRef,
        providerSessionGeneration: session.providerSessionGeneration,
        providerTurnRef: invocation.providerTurnRef,
        providerInvocationRef: invocation.providerInvocationRef,
      });
      if (name === requiredToolName) {
        session.attested = true;
        session.nativeFabricInvocations.length = 0;
      }
      return {
        content: [{ type: "text", text: result.receipt }],
        structuredContent: result.structuredContent,
      };
    },
  };
  instance.server.setRequestHandler(ListToolsRequestSchema, () => ({
    tools: descriptors.map(({ name, description, inputSchema, outputSchema }) => ({
      name,
      description,
      inputSchema,
      outputSchema,
      ...(name === requiredToolName
        ? { _meta: { "anthropic/alwaysLoad": true } }
        : {}),
    })),
  }));
  instance.server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const args = request.params.arguments ?? {};
    if (!isRecord(args) || !isFabricToolName(request.params.name)) {
      return { content: [{ type: "text", text: "invalid Fabric tool arguments" }], isError: true };
    }
    try {
      return await bridge.invokeTool(request.params.name, args);
    } catch (error: unknown) {
      return {
        content: [{
          type: "text",
          text: error instanceof ProviderAdapterError ? error.code : "FABRIC_TOOL_FAILED",
        }],
        isError: true,
      };
    }
  });
  return bridge;
}

function claudeChairOptions(
  payload: Record<string, unknown>,
  executable: string | undefined,
  resume: string | undefined,
  mcp: ClaudeChairMcpBridge,
): Options {
  return {
    ...claudeProviderOptions(payload, resume, executable),
    mcpServers: { [mcp.serverName]: mcp.server },
    allowedTools: [...mcp.allowedToolNames],
  };
}

type BridgeFactory = (input: ChairLaunchFabricBridgeInput) => Promise<ChairLaunchFabricBridge>;
type AgentBridgeFactory = (input: AgentSessionFabricBridgeInput) => Promise<AgentSessionFabricBridge>;
type ClaudeMcpBridgeFactory = (session: ClaudeChairSession) => ClaudeChairMcpBridge;

export class InstalledClaudeAgentSdkBoundary implements ClaudeAgentSdkBoundary {
  readonly #executable: string | undefined;
  readonly #verifyExecutable: typeof verifyProviderConformance;
  readonly #query: typeof query;
  readonly #bridgeFactory: BridgeFactory;
  readonly #mcpBridgeFactory: ClaudeMcpBridgeFactory;
  readonly #agentBridgeFactory: AgentBridgeFactory;
  readonly #chairSessions = new Map<string, ClaudeChairSession>();

  constructor(options?: string | {
    executable?: string;
    verifyExecutable?: typeof verifyProviderConformance;
    query?: typeof query;
    bridgeFactory?: BridgeFactory;
    mcpBridgeFactory?: ClaudeMcpBridgeFactory;
    agentBridgeFactory?: AgentBridgeFactory;
  }) {
    if (typeof options === "string" || options === undefined) {
      this.#executable = options;
      this.#verifyExecutable = verifyProviderConformance;
      this.#query = query;
      this.#bridgeFactory = createChairLaunchFabricBridge;
      this.#mcpBridgeFactory = createClaudeChairMcpBridge;
      this.#agentBridgeFactory = AgentSessionFabricBridge.create;
    } else {
      this.#executable = options.executable;
      this.#verifyExecutable = options.verifyExecutable ?? verifyProviderConformance;
      this.#query = options.query ?? query;
      this.#bridgeFactory = options.bridgeFactory ?? createChairLaunchFabricBridge;
      this.#mcpBridgeFactory = options.mcpBridgeFactory ?? createClaudeChairMcpBridge;
      this.#agentBridgeFactory = options.agentBridgeFactory ?? AgentSessionFabricBridge.create;
    }
  }

  async #verifiedQuery(input: Parameters<typeof query>[0]): Promise<ReturnType<typeof query>> {
    if (this.#executable !== undefined) {
      await this.#verifyExecutable({ adapterId: "claude-agent-sdk", executable: this.#executable });
    }
    return this.#query(input);
  }

  async status(input: { resumeReference?: string }): Promise<Record<string, unknown>> {
    if (input.resumeReference === undefined) return { healthy: true, providerSession: "unselected" };
    const info = await getSessionInfo(input.resumeReference);
    return info === undefined
      ? { healthy: false, resumeReference: input.resumeReference, state: "not-found" }
      : {
          healthy: true,
          resumeReference: info.sessionId,
          state: "resumable",
          lastModified: info.lastModified,
          ...(info.cwd === undefined ? {} : { cwd: info.cwd }),
        };
  }

  hasLiveChairSession(resumeReference: string, providerSessionGeneration: number): boolean {
    const session = this.#chairSessions.get(resumeReference);
    return (
      session !== undefined &&
      session.providerSessionGeneration === providerSessionGeneration &&
      !session.bridge.closed
    );
  }

  hasLiveAgentSession(
    resumeReference: string,
    providerSessionGeneration: number,
    bridgeGeneration: number,
  ): boolean {
    const session = this.#chairSessions.get(resumeReference);
    return session !== undefined && session.bridgeGeneration !== undefined &&
      session.providerSessionGeneration === providerSessionGeneration &&
      session.bridgeGeneration === bridgeGeneration && !session.bridge.closed;
  }

  async spawn(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const prior = optionalString(payload.priorResumeReference, "priorResumeReference");
    const completed = await consumeClaudeQuery(await this.#verifiedQuery({
      prompt: prompt(payload),
      options: claudeProviderOptions(payload, prior, this.#executable),
    }));
    return {
      resumeReference: completed.resumeReference,
      result: completed.result,
      resourceUsage: claudeResourceUsage(completed),
    };
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
    const session: ClaudeChairSession = {
      bridge,
      providerSessionGeneration: 1,
      nativeFabricInvocations: [],
      seenNativeFabricInvocationKeys: new Set(),
      seenNativeFabricInvocationOrder: [],
    };
    const mcp = this.#mcpBridgeFactory(session);
    session.mcp = mcp;
    try {
      const completed = await consumeClaudeQuery(await this.#verifiedQuery({
        prompt: `Before continuing, invoke ${mcp.attestationToolName} exactly once with {"challengeResponse":"${bridge.challengeResponse}"}. ${prompt(input.payload)}`,
        options: claudeChairOptions(input.payload, this.#executable, undefined, mcp),
      }), (sessionId) => {
        session.providerSessionRef = sessionId;
        bridge.bindProviderSession(sessionId, session.providerSessionGeneration);
      }, (message) => observeClaudeFabricToolUses(session, message, mcp));
      const evidence = {
        resumeReference: completed.resumeReference,
        providerSessionGeneration: 1,
        providerContractDigest: input.providerContractDigest,
      };
      try {
        const result = parseChairLaunchProviderResult(await bridge.result(), {
          providerAdapterId: input.providerAdapterId,
          providerActionId: input.actionId,
          providerContractDigest: input.providerContractDigest,
          challengeDigest: input.challengeDigest,
        });
        this.#chairSessions.set(completed.resumeReference, session);
        return result;
      } catch {
        throw chairLaunchContinuityUnproven(evidence);
      }
    } catch (error: unknown) {
      await bridge.close();
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
    const session: ClaudeChairSession = {
      bridge,
      providerSessionGeneration: input.nextProviderSessionGeneration,
      nativeFabricInvocations: [],
      seenNativeFabricInvocationKeys: new Set(),
      seenNativeFabricInvocationOrder: [],
    };
    const mcp = this.#mcpBridgeFactory(session);
    session.mcp = mcp;
    try {
      const completed = await consumeClaudeQuery(await this.#verifiedQuery({
        prompt: `Before continuing, invoke ${mcp.attestationToolName} exactly once with {"challengeResponse":"${bridge.challengeResponse}"}. Re-establish the retained Agent Fabric chair bridge.`,
        options: claudeChairOptions(input.payload, this.#executable, input.resumeReference, mcp),
      }), (sessionId) => {
        if (sessionId !== input.resumeReference) {
          throw new ProviderAdapterError("CHAIR_CONTINUITY_UNPROVEN", "Claude recovery resumed another provider session");
        }
        session.providerSessionRef = sessionId;
        bridge.bindProviderSession(sessionId, input.nextProviderSessionGeneration);
      }, (message) => observeClaudeFabricToolUses(session, message, mcp));
      if (completed.resumeReference !== input.resumeReference) {
        throw new ProviderAdapterError("CHAIR_CONTINUITY_UNPROVEN", "Claude recovery returned another provider session");
      }
      const result = parseChairLaunchProviderResult(await bridge.result(), {
        providerAdapterId: input.providerAdapterId,
        providerActionId: input.actionId,
        providerContractDigest: input.providerContractDigest,
        challengeDigest: input.challengeDigest,
      });
      this.#chairSessions.set(input.resumeReference, session);
      return result;
    } catch (error: unknown) {
      await bridge.close();
      throw error;
    }
  }

  async provisionAgent(input: AgentProvisionBoundaryInput): Promise<AgentProvisionProviderResult> {
    const providerSessionGeneration = typeof input.payload.generation === "number" &&
      Number.isSafeInteger(input.payload.generation) && input.payload.generation > 0
      ? input.payload.generation : 1;
    const bridge = await this.#agentBridgeFactory({
      providerAdapterId: "claude-agent-sdk",
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
    const session: ClaudeChairSession = {
      bridge,
      providerSessionGeneration,
      bridgeGeneration: input.bridgeGeneration,
      nativeFabricInvocations: [],
      seenNativeFabricInvocationKeys: new Set(),
      seenNativeFabricInvocationOrder: [],
    };
    const mcp = this.#mcpBridgeFactory(session);
    session.mcp = mcp;
    try {
      const resume = input.operation === "attach" ? input.providerSessionRef : undefined;
      const requestedPrompt = input.payload.prompt ?? input.payload.initialPrompt ?? input.payload.instruction;
      const activationArgs = bridge.challengeResponse === undefined
        ? "{}" : `{"challengeResponse":"${bridge.challengeResponse}"}`;
      const activationPrompt = `Invoke ${mcp.attestationToolName} exactly once with ${activationArgs} before continuing. ${
        typeof requestedPrompt === "string" && requestedPrompt.length > 0
          ? requestedPrompt
          : "Establish the retained Agent Fabric bridge."
      }`;
      const completed = await consumeClaudeQuery(await this.#verifiedQuery({
        prompt: activationPrompt,
        options: claudeChairOptions(input.payload, this.#executable, resume, mcp),
      }), (sessionId) => {
        session.providerSessionRef = sessionId;
        bridge.bindProviderSession(sessionId, providerSessionGeneration);
      }, (message) => observeClaudeFabricToolUses(session, message, mcp));
      const result = bridge.result();
      this.#chairSessions.set(completed.resumeReference, session);
      return result;
    } catch (error: unknown) {
      await bridge.close();
      throw error;
    }
  }

  async attach(input: { resumeReference: string; payload: Record<string, unknown> }): Promise<Record<string, unknown>> {
    const info = await getSessionInfo(input.resumeReference, {
      ...(typeof input.payload.cwd === "string" ? { dir: input.payload.cwd } : {}),
    });
    if (info === undefined) {
      throw new ProviderAdapterError("SESSION_NOT_FOUND", "Claude session cannot be attached", {
        resumeReference: input.resumeReference,
      });
    }
    return { resumeReference: info.sessionId, attached: true };
  }

  async sendTurn(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resumeReference = requiredString(payload.resumeReference, "resumeReference");
    const session = this.#chairSessions.get(resumeReference);
    if (session === undefined) {
      return await consumeClaudeQuery(await this.#verifiedQuery({ prompt: prompt(payload), options: claudeProviderOptions(payload, resumeReference, this.#executable) }));
    }
    const mcp = session.mcp;
    if (mcp === undefined) {
      throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "Claude chair MCP bridge is unavailable");
    }
    if (session.busy === true) {
      throw new ProviderAdapterError("PROVIDER_SESSION_BUSY", "Claude chair session already has an active turn");
    }
    session.busy = true;
    session.nativeFabricInvocations.length = 0;
    try {
      return await consumeClaudeQuery(await this.#verifiedQuery({
        prompt: prompt(payload),
        options: claudeChairOptions(payload, this.#executable, resumeReference, mcp),
      }), (sessionId) => session.bridge.bindProviderSession(sessionId, session.providerSessionGeneration),
      (message) => observeClaudeFabricToolUses(session, message, mcp));
    } catch (error: unknown) {
      if (error instanceof ProviderAdapterError && error.code === "PROVIDER_TURN_FAILED") throw error;
      this.#chairSessions.delete(resumeReference);
      await session.bridge.close();
      throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "Claude chair provider context was lost", undefined, {
        cause: error,
      });
    } finally {
      session.busy = false;
    }
  }

  async interrupt(): Promise<Record<string, unknown>> {
    throw new ProviderAdapterError(
      "CAPABILITY_UNAVAILABLE",
      "Claude SDK interruption requires the owning live Query object; the process-isolated fabric transport cannot prove it",
      { capability: "interrupt" },
    );
  }

  async release(payload: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resumeReference = optionalString(payload.resumeReference, "resumeReference");
    if (resumeReference !== undefined) {
      const session = this.#chairSessions.get(resumeReference);
      this.#chairSessions.delete(resumeReference);
      await session?.bridge.close();
    }
    return { released: true, deleted: false, ...(resumeReference === undefined ? {} : { resumeReference }) };
  }

  async closeAll(): Promise<void> {
    const sessions = [...this.#chairSessions.values()];
    this.#chairSessions.clear();
    await Promise.allSettled(sessions.map(async (session) => await session.bridge.close()));
  }
}

export function createClaudeAgentSdkAdapter(options: {
  boundary: ClaudeAgentSdkBoundary;
  journal: SqliteAdapterActionJournal;
  chairLaunchHandoff?: ChairLaunchHandoff;
  agentBridgeHandoff?: AgentBridgeHandoff;
}): AdapterRequestHandler {
  return createProviderAdapter({
    capabilities: CAPABILITIES,
    boundary: options.boundary,
    journal: options.journal,
    noEffect: {
      classify: claudeSubscriptionNoEffectResult,
      throwIfResult: throwClaudeSubscriptionNoEffect,
    },
    chairLaunch: {
      ...(options.chairLaunchHandoff === undefined ? {} : { handoff: options.chairLaunchHandoff }),
      validatePayload: validateClaudeChairLaunchPayload,
    },
    agentBridge: {
      ...(options.agentBridgeHandoff === undefined ? {} : { handoff: options.agentBridgeHandoff }),
    },
  });
}

export async function runClaudeAgentSdkAdapter(arguments_: string[] = process.argv.slice(2)): Promise<void> {
  const journal = new SqliteAdapterActionJournal(journalPathFromArguments("claude-agent-sdk", arguments_));
  const chairLaunchHandoff = takeChairLaunchHandoff(process.env);
  const agentBridgeHandoff = takeAgentBridgeHandoff(process.env);
  const providerIndex = arguments_.indexOf("--provider-executable");
  const providerExecutable = providerIndex === -1 ? undefined : arguments_[providerIndex + 1];
  if (providerExecutable === undefined) throw new Error("claude-agent-sdk adapter requires --provider-executable");
  const boundary = new InstalledClaudeAgentSdkBoundary({
    executable: providerExecutable,
  });
  try {
    await serveAdapter(
      createClaudeAgentSdkAdapter({
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
  await runClaudeAgentSdkAdapter();
}
