import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

import { AUTHORITY_ACTION_VOCABULARY, connectFabricDaemon, startFabricDaemon } from "../../src/index.ts";
import {
  terminateTrackedTestProcess,
  trackTestProcess,
  untrackTestProcess,
} from "./test-process-registry.ts";
import { createCurrentSessionRun } from "./current-session-testkit.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "./authority-v2-testkit.ts";

export const MCP_ROOT_AUTHORITY = {
  ...TEST_AUTHORITY_V2_FIELDS,
  workspaceRoots: ["."],
  sourcePaths: ["src"],
  artifactPaths: [".agent-run", "project-run/findings", "project-run/paired"],
  actions: [...AUTHORITY_ACTION_VOCABULARY],
  disclosure: { level: "scoped", scopes: ["local"] } as const,
  expiresAt: "2099-01-01T00:00:00.000Z",
  budget: { turns: 128, "cost:USD": 128, descendants: 128 },
};

const PACKAGE_ROOT = fileURLToPath(new URL("../..", import.meta.url));
const MCP_MAIN = fileURLToPath(new URL("../../src/mcp/main.ts", import.meta.url));

export type McpProxy = {
  client: Client;
  pid: number;
  close(): Promise<void>;
};

export async function spawnMcpProxy(options: {
  socketPath: string;
  capability: string;
  label: string;
}): Promise<McpProxy> {
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: ["--import", "tsx", MCP_MAIN],
    cwd: PACKAGE_ROOT,
    env: {
      AGENT_FABRIC_SOCKET_PATH: options.socketPath,
      AGENT_FABRIC_CAPABILITY: options.capability,
      AGENT_FABRIC_CLIENT_LABEL: options.label,
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      TMPDIR: process.env.TMPDIR ?? "/tmp",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
    },
  });
  const client = new Client({ name: options.label, version: "0.1.0" });
  const connecting = client.connect(transport);
  const pid = transport.pid;
  if (pid === null) throw new Error(`MCP proxy ${options.label} did not start`);
  trackTestProcess(pid, `MCP proxy ${options.label}`);
  try {
    await connecting;
  } catch (error: unknown) {
    await transport.close().catch(() => undefined);
    await terminateTrackedTestProcess(pid);
    throw error;
  }
  let closed = false;
  return {
    client,
    pid,
    async close(): Promise<void> {
      if (closed) return;
      closed = true;
      const [clientResult] = await Promise.allSettled([
        client.close(),
        terminateTrackedTestProcess(pid),
      ]);
      if (clientResult?.status === "rejected") throw clientResult.reason;
    },
  };
}

export type ToolCallOutcome = {
  isError: boolean;
  structured: Record<string, unknown>;
  text: string;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function recordArray(value: unknown, field: string): Record<string, unknown>[] {
  if (!Array.isArray(value) || !value.every(isRecord)) {
    throw new TypeError(`${field} must be an array of records`);
  }
  return value;
}

export async function callTool(
  client: Client,
  name: string,
  args: Record<string, unknown>,
): Promise<ToolCallOutcome> {
  const result: unknown = await client.callTool({ name, arguments: args });
  if (!isRecord(result)) {
    throw new TypeError("MCP tool result must be a record");
  }
  let structured: Record<string, unknown> = {};
  const content = Array.isArray(result.content) ? result.content : [];
  const textItem = content.find((item: unknown) => isRecord(item) && item.type === "text");
  const text = isRecord(textItem) && typeof textItem.text === "string" ? textItem.text : "";
  if (isRecord(result.structuredContent)) {
    structured = result.structuredContent;
  } else {
    if (text.length > 0) {
      const parsed: unknown = JSON.parse(text);
      if (isRecord(parsed)) {
        structured = parsed;
      }
    }
  }
  return { isError: result.isError === true, structured, text };
}

export async function createMcpFixture(
  runId = "run-mcp",
  labels: { chair: string; peer: string } = { chair: "claude-chair", peer: "codex-peer" },
  hooks: {
    chairProxyStarted?(input: { daemonPid: number; proxyPid: number; directory: string }): void;
    routeProxies?(input: {
      daemonPid: number;
      directory: string;
      socketPath: string;
    }): Promise<{
      chairSocketPath?: string;
      peerSocketPath?: string;
      close(): Promise<void>;
    }>;
  } = {},
) {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-mcp-"));
  const stateDirectory = join(directory, "state");
  const runtimeDirectory = join(directory, "runtime");
  const projectRunDirectory = join(directory, "project-run");
  const databasePath = join(stateDirectory, "fabric.sqlite3");
  const socketPath = join(runtimeDirectory, "fabric.sock");
  let daemon: Awaited<ReturnType<typeof startFabricDaemon>>;
  try {
    daemon = await startFabricDaemon({ databasePath, stateDirectory, runtimeDirectory, socketPath, workspaceRoots: [directory] });
  } catch (error: unknown) {
    await rm(directory, { recursive: true, force: true });
    throw error;
  }
  trackTestProcess(daemon.pid, `fabric daemon ${directory}`);
  let bootstrap: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let chairDaemon: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let chairProxy: McpProxy | undefined;
  let peerProxy: McpProxy | undefined;
  let proxyRoute: Awaited<ReturnType<NonNullable<typeof hooks.routeProxies>>> | undefined;
  let cleaned = false;
  const cleanup = async (): Promise<void> => {
    if (cleaned) return;
    cleaned = true;
    await Promise.allSettled([
      chairProxy?.close() ?? Promise.resolve(),
      peerProxy?.close() ?? Promise.resolve(),
    ]);
    await proxyRoute?.close().catch(() => undefined);
    await Promise.allSettled([
      chairDaemon?.close() ?? Promise.resolve(),
      bootstrap?.close() ?? Promise.resolve(),
    ]);
    try {
      await daemon.stop();
      untrackTestProcess(daemon.pid);
    } finally {
      await terminateTrackedTestProcess(daemon.pid);
      await rm(directory, { recursive: true, force: true });
    }
  };

  try {
  bootstrap = await connectFabricDaemon({ socketPath, capability: daemon.bootstrapCapability });
  const run = await createCurrentSessionRun({
    databasePath,
    workspaceRoot: directory,
    runId,
    projectRunDirectory,
    chair: { agentId: "chair", authority: MCP_ROOT_AUTHORITY },
  });
  chairDaemon = await connectFabricDaemon({ socketPath, capability: run.chairCapability });
  const peerAuthority = await chairDaemon.delegateAuthority({
    parentAuthorityId: run.chairAuthorityId,
    commandId: `${runId}:peer-authority`,
    authority: {
      ...MCP_ROOT_AUTHORITY,
      sourcePaths: ["src/peer"],
      artifactPaths: [".agent-run/peer", "project-run/paired"],
      actions: [...MCP_ROOT_AUTHORITY.actions],
      budget: { turns: 8, "cost:USD": 8 },
    },
  });
  const peerRegistration = await chairDaemon.registerAgent({
    agentId: "peer",
    authorityId: peerAuthority.authorityId,
  });
  await chairDaemon.createDiscussionGroup({
    groupId: `${runId}:default-group`,
    memberAgentIds: ["chair", "peer"],
    commandId: `${runId}:default-group:create`,
  });
  proxyRoute = await hooks.routeProxies?.({ daemonPid: daemon.pid, directory, socketPath });

  chairProxy = await spawnMcpProxy({
    socketPath: proxyRoute?.chairSocketPath ?? socketPath,
    capability: run.chairCapability,
    label: labels.chair,
  });
  hooks.chairProxyStarted?.({ daemonPid: daemon.pid, proxyPid: chairProxy.pid, directory });
  peerProxy = await spawnMcpProxy({
    socketPath: proxyRoute?.peerSocketPath ?? socketPath,
    capability: peerRegistration.capability,
    label: labels.peer,
  });

  return {
    directory,
    databasePath,
    socketPath,
    projectRunDirectory,
    daemon,
    run,
    peerAuthorityId: peerAuthority.authorityId,
    peerCapability: peerRegistration.capability,
    chairProxy,
    peerProxy,
    cleanup,
  };
  } catch (error: unknown) {
    await cleanup();
    throw error;
  }
}
