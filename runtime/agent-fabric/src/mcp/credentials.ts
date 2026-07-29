import { constants } from "node:fs";
import { lstat, open, realpath } from "node:fs/promises";
import { dirname, isAbsolute, resolve } from "node:path";

import {
  MCP_SEATS,
  projectKey,
  readLegacyBootstrapSeatGeneration,
  resolveSeatPaths,
} from "../cli/seat-store.js";

const CAPABILITY_PATTERN = /^af[bc]_[A-Za-z0-9_-]{43}$/u;
const MCP_SEAT_RENEWAL_WINDOW_MS = 60 * 60 * 1_000;

export class McpSeatNotProvisionedError extends Error {
  readonly code = "MCP_SEAT_NOT_PROVISIONED" as const;

  constructor(message: string) {
    super(message);
    this.name = "McpSeatNotProvisionedError";
  }
}

export class McpSeatRenewalRequiredError extends Error {
  readonly code = "MCP_SEAT_RENEWAL_REQUIRED" as const;

  constructor(message: string) {
    super(message);
    this.name = "McpSeatRenewalRequiredError";
  }
}

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

async function readPrivateRegularFile(path: string): Promise<string> {
  let before: Awaited<ReturnType<typeof lstat>>;
  try {
    before = await lstat(path);
  } catch (error: unknown) {
    throw error;
  }
  if (!before.isFile() || before.isSymbolicLink()) {
    throw new Error("agent fabric MCP capability source must be a regular file");
  }
  if ((before.mode & 0o077) !== 0) {
    throw new Error("agent fabric MCP capability file must be private (0600)");
  }
  if (typeof process.getuid === "function" && before.uid !== process.getuid()) {
    throw new Error("agent fabric MCP capability file must be owned by the current user");
  }
  let handle: Awaited<ReturnType<typeof open>>;
  try {
    handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (error: unknown) {
    if (errorCode(error) === "ELOOP") {
      throw new Error("agent fabric MCP capability source must be a regular file");
    }
    throw error;
  }
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.dev !== before.dev || opened.ino !== before.ino) {
      throw new Error("agent fabric MCP capability file changed while opening");
    }
    if ((opened.mode & 0o077) !== 0) {
      throw new Error("agent fabric MCP capability file must be private (0600)");
    }
    if (typeof process.getuid === "function" && opened.uid !== process.getuid()) {
      throw new Error("agent fabric MCP capability file must be owned by the current user");
    }
    return await handle.readFile("utf8");
  } finally {
    await handle.close();
  }
}

async function resolveProjectSeatFile(
  environment: NodeJS.ProcessEnv,
  cwd: string,
  warn: (message: string) => void,
): Promise<string> {
  const seat = environment.AGENT_FABRIC_SEAT;
  const stateDirectory = environment.AGENT_FABRIC_STATE_DIRECTORY;
  if (seat === undefined || !(MCP_SEATS as readonly string[]).includes(seat)) {
    throw new Error("agent fabric MCP seat is invalid");
  }
  if (stateDirectory === undefined || !isAbsolute(stateDirectory)) {
    throw new Error("agent fabric MCP state directory must be absolute");
  }
  const configuredProject = environment.AGENT_FABRIC_PROJECT_PATH;
  if (configuredProject !== undefined && !isAbsolute(configuredProject)) {
    throw new Error("agent fabric MCP project path must be absolute");
  }
  let candidate = await realpath(resolve(configuredProject ?? cwd));
  for (;;) {
    try {
      const paths = await resolveSeatPaths({ stateDirectory, project: candidate, seat: seat as (typeof MCP_SEATS)[number] });
      const metadataPath = paths.metadataPath;
      const metadataText = await readPrivateRegularFile(metadataPath);
      const metadata: unknown = JSON.parse(metadataText);
      const credentialPath = paths.credentialPath;
      if (
        typeof metadata !== "object" ||
        metadata === null ||
        !("schemaVersion" in metadata) ||
        metadata.schemaVersion !== 1 ||
        !("projectPath" in metadata) ||
        metadata.projectPath !== candidate ||
        !("projectKey" in metadata) ||
        metadata.projectKey !== projectKey(candidate) ||
        !("generation" in metadata) ||
        metadata.generation !== paths.generation ||
        !("previousGeneration" in metadata) ||
        (metadata.previousGeneration !== null &&
          (typeof metadata.previousGeneration !== "string" || !/^[0-9a-f]{64}$/u.test(metadata.previousGeneration))) ||
        ("originKind" in metadata && metadata.originKind !== "bootstrap" && metadata.originKind !== "provisioned") ||
        !("seat" in metadata) ||
        metadata.seat !== seat ||
        !("credentialPath" in metadata) ||
        metadata.credentialPath !== credentialPath ||
        !("expiresAt" in metadata) ||
        typeof metadata.expiresAt !== "string" ||
        !Number.isFinite(Date.parse(metadata.expiresAt))
      ) {
        throw new Error(`agent fabric MCP seat metadata is invalid for project ${candidate}`);
      }
      const remainingMs = Date.parse(metadata.expiresAt) - Date.now();
      const bootstrapSeat = "originKind" in metadata && metadata.originKind === "bootstrap";
      const verifiedLegacyBootstrapSeat = !("originKind" in metadata) &&
        await readLegacyBootstrapSeatGeneration({ stateDirectory, projectPath: candidate }) === paths.generation;
      if ((bootstrapSeat || verifiedLegacyBootstrapSeat) && remainingMs <= MCP_SEAT_RENEWAL_WINDOW_MS) {
        throw new McpSeatRenewalRequiredError(
          `agent fabric MCP seat ${seat} ${remainingMs <= 0 ? "expired" : "expires"} at ${metadata.expiresAt}`,
        );
      }
      if (remainingMs <= 0) throw new Error(`agent fabric MCP seat ${seat} expired at ${metadata.expiresAt}`);
      if (!bootstrapSeat && !verifiedLegacyBootstrapSeat && remainingMs <= 7 * 24 * 60 * 60 * 1_000) {
        warn(`agent fabric MCP seat ${seat} expires at ${metadata.expiresAt}; coordinate a full-roster renewal`);
      }
      return credentialPath;
    } catch (error: unknown) {
      if (errorCode(error) !== "ENOENT") throw error;
    }
    const parent = dirname(candidate);
    if (parent === candidate) break;
    candidate = parent;
  }
  const detail = `agent fabric MCP seat ${seat} is not provisioned for ${cwd} or an ancestor project`;
  throw new McpSeatNotProvisionedError(
    seat === "claude" || seat === "codex"
      ? detail
      : `${detail}; provision the peer seat with ` +
        `"$HOME/.agents/scripts/agent-fabric" mcp peer-provision --project ${shellQuote(cwd)} --seat ${seat}`,
  );
}

export async function resolveMcpCapability(
  environment: NodeJS.ProcessEnv,
  cwd = process.cwd(),
  warn: (message: string) => void = () => undefined,
): Promise<string> {
  const inline = environment.AGENT_FABRIC_CAPABILITY;
  let file = environment.AGENT_FABRIC_CAPABILITY_FILE;
  const projectSeat = environment.AGENT_FABRIC_SEAT;
  const sourceCount = Number(inline !== undefined) + Number(file !== undefined) + Number(projectSeat !== undefined);
  if (sourceCount !== 1) {
    throw new Error(
      "agent-fabric-mcp requires exactly one capability source; project-seat registries must set " +
      "AGENT_FABRIC_STATE_DIRECTORY, AGENT_FABRIC_SEAT and AGENT_FABRIC_CLIENT_LABEL",
    );
  }
  if (inline !== undefined) {
    if (!CAPABILITY_PATTERN.test(inline)) throw new Error("agent fabric MCP capability is invalid");
    return inline;
  }
  if (projectSeat !== undefined) file = await resolveProjectSeatFile(environment, cwd, warn);
  if (file === undefined || !isAbsolute(file)) {
    throw new Error("agent fabric MCP capability file must be absolute");
  }
  const capability = (await readPrivateRegularFile(file)).trim();
  if (!CAPABILITY_PATTERN.test(capability)) throw new Error("agent fabric MCP capability file is invalid");
  return capability;
}

export async function resolveRenewableMcpCapability(
  environment: NodeJS.ProcessEnv,
  cwd: string,
  renew: () => Promise<unknown>,
  warn: (message: string) => void = () => undefined,
): Promise<string> {
  try {
    return await resolveMcpCapability(environment, cwd, warn);
  } catch (error: unknown) {
    if (!(error instanceof McpSeatRenewalRequiredError)) throw error;
    await renew();
    return await resolveMcpCapability(environment, cwd, warn);
  }
}
