import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";
import { join } from "node:path";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";
import { connectFabricDaemon } from "../daemon/client.js";
import { currentMcpSeatGeneration } from "../core/mcp-seat-generation.js";
import type { FabricPaths } from "./paths.js";
import {
  parseMcpSeat,
  installSeatGeneration,
  readActiveSeatGeneration,
  resolveSeatProject,
  resolveSeatPaths,
  type McpSeat,
  type SeatMetadata,
} from "./seat-store.js";

const MAXIMUM_SEAT_LIFETIME_MS = 31 * 24 * 60 * 60 * 1_000;

export type DiscoveryReceipt = {
  schemaVersion: 1;
  socketPath: string;
  pid: number;
  bootstrapCapability: string;
  lifecycleReceiptAuthorityId: string | null;
};

export type McpProvisionOutput = {
  schemaVersion: 1;
  projectKey: string;
  projectPath: string;
  expectedPreviousGeneration: string | null;
  generation: string;
  projectSessionId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runId: string;
  runRevision: number;
  chairAgentId: string;
  chairGeneration: number;
  chairLeaseId: string;
  chairSeat: McpSeat;
  expiresAt: string;
  seats: Array<{
    seat: McpSeat;
    role: "chair" | "peer";
    agentId: string;
    principalGeneration: number;
    credentialPath: string;
    metadataPath: string;
  }>;
};

type ParsedSeatBinding = {
  seat: McpSeat;
  agentId: string;
  expectedPrincipalGeneration: number;
};

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function assertExactOptions(arguments_: string[], names: readonly string[], command: string): void {
  if (arguments_.length !== names.length * 2) {
    throw new Error(`${command} requires ${names.map((name) => `${name} <value>`).join(" ")}`);
  }
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index];
    const value = arguments_[index + 1];
    if (name === undefined || !names.includes(name)) throw new Error(`${command} received an unknown option`);
    if (value === undefined || value.startsWith("--")) throw new Error(`${command} requires ${name} <value>`);
  }
}

function option(arguments_: string[], name: string, command: string): string {
  const indexes = arguments_.flatMap((value, index) => (value === name ? [index] : []));
  if (indexes.length !== 1) throw new Error(`${command} requires exactly one ${name} option`);
  const index = indexes[0];
  if (index === undefined) throw new Error(`${command} requires ${name} <value>`);
  const value = arguments_[index + 1];
  if (value === undefined || value.startsWith("--")) throw new Error(`${command} requires ${name} <value>`);
  return value;
}

function positiveInteger(value: string, optionName: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || String(parsed) !== value) {
    throw new Error(`mcp provision ${optionName} must be a positive integer`);
  }
  return parsed;
}

function parseSeatBindings(value: string): ParsedSeatBinding[] {
  const values = value.split(",");
  if (values.length === 0 || values.some((binding) => binding.length === 0 || binding.trim() !== binding)) {
    throw new Error("mcp provision --seat-bindings must be a comma-separated seat=agent@generation roster");
  }
  const bindings = values.map((value_) => {
    const equals = value_.indexOf("=");
    const at = value_.lastIndexOf("@");
    if (equals < 1 || at <= equals + 1 || at === value_.length - 1) {
      throw new Error("mcp provision --seat-bindings must use seat=agent@generation entries");
    }
    const seat = parseMcpSeat(value_.slice(0, equals));
    const agentId = value_.slice(equals + 1, at);
    if (agentId.length > 512 || agentId.includes("\0") || agentId.includes(",")) {
      throw new Error("mcp provision --seat-bindings contains an invalid agent ID");
    }
    return {
      seat,
      agentId,
      expectedPrincipalGeneration: positiveInteger(value_.slice(at + 1), "principal generation"),
    };
  });
  if (new Set(bindings.map(({ seat }) => seat)).size !== bindings.length) {
    throw new Error("mcp provision --seat-bindings contains a duplicate seat");
  }
  if (new Set(bindings.map(({ agentId }) => agentId)).size !== bindings.length) {
    throw new Error("mcp provision --seat-bindings contains a duplicate agent");
  }
  return bindings.sort((left, right) => left.seat.localeCompare(right.seat));
}

export async function readDiscoveryReceipt(paths: FabricPaths): Promise<DiscoveryReceipt> {
  const discoveryPath = join(paths.runtimeDirectory, "fabric-v1.discovery.json");
  const directory = await lstat(paths.runtimeDirectory);
  if (directory.isSymbolicLink() || !directory.isDirectory() || (directory.mode & 0o777) !== 0o700) {
    throw new Error(`fabric runtime directory must be a private non-symlink directory: ${paths.runtimeDirectory}`);
  }
  const before = await lstat(discoveryPath);
  if (before.isSymbolicLink()) throw new Error(`fabric discovery receipt must not be a symbolic link: ${discoveryPath}`);
  if (!before.isFile()) throw new Error(`fabric discovery receipt is not a regular file: ${discoveryPath}`);
  if ((before.mode & 0o777) !== 0o600) throw new Error(`fabric discovery receipt must have mode 0600: ${discoveryPath}`);

  const handle = await open(discoveryPath, constants.O_RDONLY | constants.O_NOFOLLOW);
  let text: string;
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.dev !== before.dev || opened.ino !== before.ino) {
      throw new Error(`fabric discovery receipt changed while opening: ${discoveryPath}`);
    }
    text = await handle.readFile("utf8");
  } finally {
    await handle.close();
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error(`fabric discovery receipt is not valid JSON: ${discoveryPath}`);
  }
  if (
    !record(value) ||
    Object.keys(value).sort().join(",") !==
      "bootstrapCapability,lifecycleReceiptAuthorityId,pid,schemaVersion,socketPath" ||
    value.schemaVersion !== 1 ||
    value.socketPath !== paths.socketPath ||
    typeof value.pid !== "number" ||
    !Number.isSafeInteger(value.pid) ||
    value.pid <= 0 ||
    typeof value.bootstrapCapability !== "string" ||
    !/^afb_[A-Za-z0-9_-]{43}$/u.test(value.bootstrapCapability) ||
    (value.lifecycleReceiptAuthorityId !== null &&
      (typeof value.lifecycleReceiptAuthorityId !== "string" ||
        value.lifecycleReceiptAuthorityId.trim().length === 0))
  ) {
    throw new Error(`fabric discovery receipt is invalid or does not match the configured socket: ${discoveryPath}`);
  }
  return value as DiscoveryReceipt;
}

function boundedExpiry(value: string): string {
  const expiresAt = Date.parse(value);
  const now = Date.now();
  if (!Number.isFinite(expiresAt) || new Date(expiresAt).toISOString() !== value) {
    throw new Error("mcp provision --expires-at must be an ISO timestamp");
  }
  if (expiresAt <= now || expiresAt - now > MAXIMUM_SEAT_LIFETIME_MS) {
    throw new Error("mcp provision --expires-at must be in the future and no more than 31 days away");
  }
  return value;
}

export async function provisionMcpSeats(arguments_: string[], paths: FabricPaths): Promise<McpProvisionOutput> {
  const command = "mcp provision";
  const optionNames = [
    "--project",
    "--project-session-id",
    "--session-revision",
    "--session-generation",
    "--run-id",
    "--run-revision",
    "--chair-seat",
    "--chair-agent-id",
    "--chair-generation",
    "--chair-lease-id",
    "--seat-bindings",
    "--expires-at",
  ] as const;
  assertExactOptions(arguments_, optionNames, command);
  const project = option(arguments_, "--project", command);
  const projectSessionId = option(arguments_, "--project-session-id", command);
  const sessionRevision = positiveInteger(option(arguments_, "--session-revision", command), "--session-revision");
  const sessionGeneration = positiveInteger(option(arguments_, "--session-generation", command), "--session-generation");
  const runId = option(arguments_, "--run-id", command);
  const runRevision = positiveInteger(option(arguments_, "--run-revision", command), "--run-revision");
  const chairSeat = parseMcpSeat(option(arguments_, "--chair-seat", command));
  const chairAgentId = option(arguments_, "--chair-agent-id", command);
  const chairGeneration = positiveInteger(option(arguments_, "--chair-generation", command), "--chair-generation");
  const chairLeaseId = option(arguments_, "--chair-lease-id", command);
  const bindings = parseSeatBindings(option(arguments_, "--seat-bindings", command));
  const expiresAt = boundedExpiry(option(arguments_, "--expires-at", command));
  if (bindings.length < 2) throw new Error("mcp provision requires at least two distinct seat bindings");
  const chairBinding = bindings.find(({ seat }) => seat === chairSeat);
  if (chairBinding?.agentId !== chairAgentId) {
    throw new Error("mcp provision chair seat must bind the exact supplied chair agent");
  }
  const { projectKey, projectPath } = await resolveSeatProject({
    stateDirectory: paths.stateDirectory,
    project,
  });
  const bindingIdentity = {
    canonicalRoot: projectPath,
    projectSessionId,
    sessionRevision,
    sessionGeneration,
    runId,
    runRevision,
    chairAgentId,
    chairGeneration,
    chairLeaseId,
    bindings,
    expiresAt,
  };
  const { generation } = currentMcpSeatGeneration(bindingIdentity);
  const activeGeneration = await readActiveSeatGeneration({
    stateDirectory: paths.stateDirectory,
    projectPath,
  });
  const expectedPreviousGeneration = activeGeneration?.generation === generation
    ? activeGeneration.previousGeneration
    : activeGeneration?.generation ?? null;
  const discovery = await readDiscoveryReceipt(paths);
  const bootstrap = await connectFabricDaemon({
    socketPath: discovery.socketPath,
    capability: discovery.bootstrapCapability,
    requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
  });
  try {
    const bound = await bootstrap.bindCurrentMcpSeats({
      canonicalRoot: projectPath,
      expectedPreviousGeneration,
      generation,
      projectSessionId,
      expectedSessionRevision: sessionRevision,
      expectedSessionGeneration: sessionGeneration,
      runId,
      expectedRunRevision: runRevision,
      chairAgentId,
      expectedChairGeneration: chairGeneration,
      chairLeaseId,
      expiresAt,
      bindings,
    });
    if (
      bound.generation !== generation ||
      bound.expectedPreviousGeneration !== expectedPreviousGeneration
    ) {
      throw new Error("daemon returned a crossed MCP seat generation");
    }
    const stagedSeats: Array<{ metadata: Omit<SeatMetadata, "credentialPath">; credential: string }> = [];
    for (const binding of bindings) {
      const credential = bound.credentials.find(({ seat }) => seat === binding.seat);
      if (credential === undefined || credential.agentId !== binding.agentId) {
        throw new Error(`daemon did not bind the exact credential for seat ${binding.seat}`);
      }
      const role = binding.seat === chairSeat ? "chair" : "peer";
      const metadata: Omit<SeatMetadata, "credentialPath"> = {
        schemaVersion: 1,
        projectKey,
        projectPath,
        generation,
        previousGeneration: expectedPreviousGeneration,
        originKind: "provisioned",
        projectSessionId,
        sessionRevision,
        sessionGeneration,
        runId,
        runRevision,
        chairAgentId,
        chairGeneration,
        chairLeaseId,
        seat: binding.seat,
        agentId: binding.agentId,
        principalGeneration: binding.expectedPrincipalGeneration,
        role,
        expiresAt,
      };
      stagedSeats.push({ metadata, credential: credential.capability });
    }
    const installed = await installSeatGeneration({
      stateDirectory: paths.stateDirectory,
      projectPath,
      generation,
      expectedPreviousGeneration,
      seats: stagedSeats,
    });
    const outputSeats: McpProvisionOutput["seats"] = [];
    for (const binding of bindings) {
      const written = installed.find((candidate) => candidate.seat === binding.seat);
      if (written === undefined) throw new Error(`seat generation did not install ${binding.seat}`);
      const role = binding.seat === chairSeat ? "chair" : "peer";
      outputSeats.push({
        seat: binding.seat,
        role,
        agentId: binding.agentId,
        principalGeneration: binding.expectedPrincipalGeneration,
        credentialPath: written.credentialPath,
        metadataPath: written.metadataPath,
      });
    }
    return {
      schemaVersion: 1,
      projectKey,
      projectPath,
      expectedPreviousGeneration,
      generation,
      projectSessionId,
      sessionRevision,
      sessionGeneration,
      runId,
      runRevision,
      chairAgentId,
      chairGeneration,
      chairLeaseId,
      chairSeat,
      expiresAt,
      seats: outputSeats,
    };
  } finally {
    await bootstrap.close();
  }
}

export async function mcpSeatPath(arguments_: string[], paths: FabricPaths): Promise<{
  schemaVersion: 1;
  projectKey: string;
  generation: string;
  seat: McpSeat;
  credentialPath: string;
  metadataPath: string;
}> {
  const command = "mcp seat-path";
  assertExactOptions(arguments_, ["--project", "--seat"], command);
  const project = option(arguments_, "--project", command);
  const seat = parseMcpSeat(option(arguments_, "--seat", command));
  const seatPaths = await resolveSeatPaths({ stateDirectory: paths.stateDirectory, project, seat });
  return {
    schemaVersion: 1,
    projectKey: seatPaths.projectKey,
    generation: seatPaths.generation,
    seat,
    credentialPath: seatPaths.credentialPath,
    metadataPath: seatPaths.metadataPath,
  };
}
