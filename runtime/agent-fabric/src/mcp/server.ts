import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import {
  CallToolRequestSchema,
  ListResourcesRequestSchema,
  ListResourceTemplatesRequestSchema,
  ListToolsRequestSchema,
  ReadResourceRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import {
  AGENT_RESULT_SHAPE_FEATURES,
  FABRIC_OPERATIONS,
  MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
  NdjsonRpcTransport,
  OPERATION_REGISTRY,
  ProtocolRemoteError,
  ProtocolResultShapeFeatureError,
  ProtocolTransportError,
  buildMcpDescriptorSet,
  isDaemonGrantableOperation,
  operationsForPrincipal,
  parseOperationInputForPrincipal,
  parseOperationResult,
  renderMcpReceipt,
  type FabricOperation,
  type McpResourceDescriptor,
  type McpToolDescriptor,
  type ProtocolPrincipal,
  type ProtocolFeature,
} from "@local/agent-fabric-protocol";

export type FabricMcpServerOptions = {
  socketPath: string;
  capability: string;
  clientLabel?: string;
  refreshCapability?: () => Promise<string>;
  /**
   * Machine-readable receipt for the automatic actions that produced this
   * activation. The proxy only relays it; it never interprets its contents.
   */
  lifecycleReceipt?: Readonly<{ schemaVersion: 1; kind: string; mutated: boolean; healthy: boolean }>;
};

export type FabricMcpServerHandle = {
  server: Server;
  close(): Promise<void>;
};

export function createUnprovisionedMcpServer(options?: {
  bootstrap: () => Promise<FabricMcpServerOptions>;
}): FabricMcpServerHandle {
  const server = new Server(
    { name: "agent-fabric", version: "1.0.0" },
    { capabilities: { tools: { listChanged: true }, resources: {} } },
  );
  let protocolClose: (() => Promise<void>) | undefined;
  const bootstrapTool = {
    name: "fabric_bootstrap",
    description: "Create the exact trusted project's first narrow scoping custody and install this primary seat.",
    inputSchema: { type: "object" as const, additionalProperties: false, properties: {} },
  };
  server.setRequestHandler(ListToolsRequestSchema, () => ({ tools: options === undefined ? [] : [bootstrapTool] }));
  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    if (options === undefined || request.params.name !== "fabric_bootstrap") throw new Error(`unknown tool: ${request.params.name}`);
    const args = request.params.arguments ?? {};
    if (Object.keys(args).length !== 0) throw new TypeError("fabric_bootstrap accepts no arguments");
    try {
      let activated: FabricMcpServerOptions;
      try {
        activated = await options.bootstrap();
      } catch (error: unknown) {
        if (!isRecord(error) || error.code !== "BOOTSTRAP_GENERATION_CHANGED") throw error;
        activated = await options.bootstrap();
      }
      try {
        protocolClose = await configureFabricMcpServer(server, activated);
      } catch (error: unknown) {
        if (!(error instanceof ProtocolRemoteError) || error.code !== "AUTHENTICATION_FAILED") throw error;
        activated = await options.bootstrap();
        protocolClose = await configureFabricMcpServer(server, activated);
      }
      await server.sendToolListChanged();
      return {
        content: [{ type: "text", text: "Agent Fabric bootstrap complete; normal Fabric tools are now active." }],
        structuredContent: {
          bootstrapped: true,
          ...(activated.lifecycleReceipt === undefined ? {} : { receipt: activated.lifecycleReceipt }),
        },
      };
    } catch (error: unknown) {
      const payload = errorPayload(error);
      return { content: [{ type: "text", text: JSON.stringify(payload) }], isError: true };
    }
  });
  server.setRequestHandler(ListResourcesRequestSchema, () => ({ resources: [] }));
  server.setRequestHandler(ListResourceTemplatesRequestSchema, () => ({ resourceTemplates: [] }));
  return {
    server,
    async close(): Promise<void> {
      await Promise.allSettled([server.close(), protocolClose?.() ?? Promise.resolve()]);
    },
  };
}

const agentFeatures = Object.freeze([...new Set(
  [
    ...[...operationsForPrincipal("agent")]
      .filter(isDaemonGrantableOperation)
      .map((operation) => OPERATION_REGISTRY[operation].feature),
    ...AGENT_RESULT_SHAPE_FEATURES,
    MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
  ],
)].sort()) as readonly ProtocolFeature[];

type McpErrorPayload = { code: string; message: string; action?: string };

class ReconnectRequiredError extends Error {
  readonly code = "RECONNECT_REQUIRED";

  constructor(message: string, readonly action = "Retry the Fabric request after the daemon is available.") {
    super(message);
  }
}

export function errorPayload(error: unknown): McpErrorPayload {
  if (error instanceof ProtocolRemoteError) return { code: error.code, message: error.message };
  if (error instanceof ProtocolTransportError) {
    return { code: error.code, message: "Agent Fabric protocol request failed" };
  }
  if (error instanceof ProtocolResultShapeFeatureError) {
    return { code: error.code, message: error.message };
  }
  if (error instanceof TypeError) return { code: "MCP_INPUT_INVALID", message: error.message };
  if (isRecord(error) && typeof error.code === "string" && typeof error.message === "string") {
    return {
      code: error.code,
      message: error.message,
      ...(typeof error.action === "string" ? { action: error.action } : {}),
    };
  }
  return { code: "FABRIC_MCP_REQUEST_FAILED", message: "Agent Fabric MCP request failed" };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasStableReplayIdentity(operation: FabricOperation, input: unknown): boolean {
  if (!isRecord(input)) return false;
  if (typeof input.commandId === "string" && input.commandId.length > 0) return true;
  return operation === FABRIC_OPERATIONS.sendMessage &&
    typeof input.dedupeKey === "string" && input.dedupeKey.length > 0;
}

function isSameAgentPrincipal(left: ProtocolPrincipal, right: ProtocolPrincipal): boolean {
  return left.kind === "agent" && right.kind === "agent" &&
    left.agentId === right.agentId &&
    left.projectSessionId === right.projectSessionId &&
    left.runId === right.runId &&
    left.principalGeneration === right.principalGeneration;
}

function requireSameAgentPrincipal(
  original: ProtocolPrincipal,
  replacement: ProtocolPrincipal,
  operation: FabricOperation,
  input: unknown,
): void {
  if (isSameAgentPrincipal(original, replacement)) return;
  throw new ReconnectRequiredError(
    "Agent Fabric reconnected as a different authenticated principal and did not replay the request",
    ambiguousRetryAction(operation, input),
  );
}

function ambiguousRetryAction(operation: FabricOperation, input: unknown): string {
  if (
    operation === FABRIC_OPERATIONS.receiveMessages &&
    isRecord(input) &&
    typeof input.visibilityTimeoutMs === "number" &&
    Number.isSafeInteger(input.visibilityTimeoutMs) &&
    input.visibilityTimeoutMs > 0
  ) {
    return "The fabric_message_receive outcome is unknown and no delivery was acknowledged. " +
      `Wait at least ${String(input.visibilityTimeoutMs)} ms (the requested visibilityTimeoutMs) before retrying fabric_message_receive.`;
  }
  return "Treat the operation outcome as unknown; reconcile its durable state before an explicit retry.";
}

export async function retryRecoveredProtocolCall<T>(
  call: () => Promise<T>,
  operation?: FabricOperation,
  input?: unknown,
): Promise<T> {
  try {
    return await call();
  } catch (retryError: unknown) {
    if (
      retryError instanceof ProtocolTransportError &&
      (retryError.code === "PROTOCOL_DISCONNECTED" || retryError.code === "PROTOCOL_TIMEOUT")
    ) {
      throw new ReconnectRequiredError(
        "Agent Fabric lost its recovered daemon connection",
        operation === undefined
          ? "Treat the operation outcome as unknown; reconcile its durable state before an explicit retry."
          : ambiguousRetryAction(operation, input),
      );
    }
    throw retryError;
  }
}

export function isRecoverableProtocolInterruption(error: unknown, transportClosed: boolean): boolean {
  return error instanceof ProtocolTransportError &&
    (error.code === "PROTOCOL_DISCONNECTED" || error.code === "PROTOCOL_TIMEOUT") &&
    (transportClosed || (error.code === "PROTOCOL_TIMEOUT" && error.requestState === "queued"));
}

function resourceCall(
  uri: string,
  resources: readonly McpResourceDescriptor[],
): { descriptor: McpResourceDescriptor; runId: string } {
  const match = /^fabric:\/\/runs\/([^/]+)\/(status|tasks|agents|receipts)$/u.exec(uri);
  if (match?.[1] === undefined || match[2] === undefined) throw new TypeError("unknown Fabric resource URI");
  const template = `fabric://runs/{run_id}/${match[2]}`;
  const descriptor = resources.find((candidate) => candidate.uriTemplate === template);
  if (descriptor === undefined) throw new TypeError("Fabric resource is outside the negotiated grant");
  return { descriptor, runId: decodeURIComponent(match[1]) };
}

function advertisedTool(descriptor: McpToolDescriptor): {
  name: string;
  description: string;
  inputSchema: McpToolDescriptor["inputSchema"];
  outputSchema: McpToolDescriptor["outputSchema"];
} {
  return {
    name: descriptor.name,
    description: descriptor.description,
    inputSchema: descriptor.inputSchema,
    outputSchema: descriptor.outputSchema,
  };
}

async function connectAgentProtocol(options: FabricMcpServerOptions, capability: string) {
  if (!/^afc_[A-Za-z0-9_-]{43}$/u.test(capability)) {
    throw new TypeError("MCP requires an Agent Fabric agent capability");
  }
  const protocol = await NdjsonRpcTransport.connect(createConnection(options.socketPath), {
    protocolVersion: 1,
    client: { name: options.clientLabel ?? "agent-fabric-mcp", version: "1.0.0" },
    authentication: {
      scheme: "capability",
      credential: capability,
      clientNonce: `mcp_${randomUUID()}`,
    },
    expectedPrincipalKind: "agent",
    requiredFeatures: ["fabric-core.v1", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
    optionalFeatures: agentFeatures.filter((feature) =>
      feature !== "fabric-core.v1" &&
      feature !== MCP_BOOTSTRAP_CREDENTIALS_FEATURE),
  });
  if (protocol.principal.kind !== "agent") {
    await protocol.close();
    throw new TypeError("MCP protocol credential did not resolve to an agent principal");
  }
  return protocol;
}

async function configureFabricMcpServer(server: Server, options: FabricMcpServerOptions): Promise<() => Promise<void>> {
  let protocol = await connectAgentProtocol(options, options.capability);
  let currentCapability = options.capability;
  let refreshInFlight: Promise<void> | undefined;
  let reconnectInFlight: Promise<void> | undefined;
  const replaceProtocol = async (capability: string): Promise<void> => {
    const replacement = await connectAgentProtocol(options, capability);
    const previous = protocol;
    protocol = replacement;
    currentCapability = capability;
    await previous.close().catch(() => undefined);
  };
  const refreshProtocol = async (): Promise<void> => {
    if (options.refreshCapability === undefined) throw new Error("MCP seat credential refresh is unavailable");
    refreshInFlight ??= (async () => {
      const capability = await options.refreshCapability?.();
      if (capability === undefined) throw new Error("MCP seat credential refresh is unavailable");
      await replaceProtocol(capability);
    })().finally(() => { refreshInFlight = undefined; });
    await refreshInFlight;
  };
  const callWithAuthenticationRefresh = async (operation: FabricOperation, input: unknown): Promise<unknown> => {
    const attemptedPrincipal = protocol.principal;
    try {
      return await protocol.call(operation as never, input as never);
    } catch (error: unknown) {
      if (
        !(error instanceof ProtocolRemoteError) ||
        error.code !== "AUTHENTICATION_FAILED" ||
        options.refreshCapability === undefined
      ) throw error;
      await refreshProtocol();
      requireSameAgentPrincipal(attemptedPrincipal, protocol.principal, operation, input);
      return await protocol.call(operation as never, input as never);
    }
  };
  const call = async (operation: FabricOperation, input: unknown): Promise<unknown> => {
    const attemptedProtocol = protocol;
    const attemptedPrincipal = attemptedProtocol.principal;
    const transportWasClosedBeforeCall = attemptedProtocol.closed;
    try {
      return await callWithAuthenticationRefresh(operation, input);
    } catch (error: unknown) {
      if (!isRecoverableProtocolInterruption(error, attemptedProtocol.closed)) throw error;
      const requestWasNeverSubmitted = transportWasClosedBeforeCall ||
        (error instanceof ProtocolTransportError && error.requestState === "queued");
      try {
        if (protocol === attemptedProtocol) {
          reconnectInFlight ??= (async () => {
            try {
              await replaceProtocol(currentCapability);
            } catch (reconnectError: unknown) {
              if (!(reconnectError instanceof ProtocolRemoteError) || reconnectError.code !== "AUTHENTICATION_FAILED") {
                throw reconnectError;
              }
              await refreshProtocol();
            }
          })().finally(() => { reconnectInFlight = undefined; });
          await reconnectInFlight;
        }
      } catch {
        throw new ReconnectRequiredError("Agent Fabric could not reconnect to the daemon");
      }
      requireSameAgentPrincipal(attemptedPrincipal, protocol.principal, operation, input);
      if (!requestWasNeverSubmitted && !hasStableReplayIdentity(operation, input)) {
        throw new ReconnectRequiredError(
          "Agent Fabric reconnected but did not replay a request without a stable replay identity",
          ambiguousRetryAction(operation, input),
        );
      }
      return await retryRecoveredProtocolCall(
        async () => await callWithAuthenticationRefresh(operation, input),
        operation,
        input,
      );
    }
  };
  const descriptors = buildMcpDescriptorSet(protocol.allowedOperations);
  const toolsByName = new Map(descriptors.tools.map((descriptor) => [descriptor.name, descriptor]));
  server.setRequestHandler(ListToolsRequestSchema, () => ({
    tools: descriptors.tools.map(advertisedTool),
  }));

  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const descriptor = toolsByName.get(request.params.name as `fabric_${string}`);
    if (descriptor === undefined) throw new Error(`unknown tool: ${request.params.name}`);
    const args = request.params.arguments ?? {};
    try {
      const parsedInput = parseOperationInputForPrincipal(descriptor.operation, "agent", args);
      const raw = await call(descriptor.operation, parsedInput);
      const result = parseOperationResult(descriptor.operation, raw);
      if (!isRecord(result)) throw new TypeError("projected Fabric result must be an object");
      return {
        content: [{ type: "text", text: renderMcpReceipt(descriptor, args, result) }],
        structuredContent: result,
      };
    } catch (error: unknown) {
      const payload = errorPayload(error);
      return {
        content: [{ type: "text", text: JSON.stringify(payload) }],
        isError: true,
      };
    }
  });

  server.setRequestHandler(ListResourcesRequestSchema, () => ({ resources: [] }));
  server.setRequestHandler(ListResourceTemplatesRequestSchema, () => ({
    resourceTemplates: descriptors.resources,
  }));
  server.setRequestHandler(ReadResourceRequestSchema, async (request) => {
    const { descriptor, runId } = resourceCall(request.params.uri, descriptors.resources);
    const operation = descriptor.operation as FabricOperation;
    const input = parseOperationInputForPrincipal(operation, "agent", { runId });
    const raw = await call(operation, input);
    const result = parseOperationResult(operation, raw);
    return {
      contents: [{
        uri: request.params.uri,
        mimeType: descriptor.mimeType,
        text: JSON.stringify(result),
      }],
    };
  });

  return async () => protocol.close();
}

export async function createFabricMcpServer(options: FabricMcpServerOptions): Promise<FabricMcpServerHandle> {
  const server = new Server(
    { name: "agent-fabric", version: "1.0.0" },
    { capabilities: { tools: {}, resources: {} } },
  );
  const closeProtocol = await configureFabricMcpServer(server, options);
  let closed = false;
  return {
    server,
    async close(): Promise<void> {
      if (closed) return;
      closed = true;
      await Promise.allSettled([server.close(), closeProtocol()]);
    },
  };
}
