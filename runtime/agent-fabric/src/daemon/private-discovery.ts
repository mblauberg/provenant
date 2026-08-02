import { createHash, randomBytes } from "node:crypto";
import { constants } from "node:fs";
import type { FileHandle } from "node:fs/promises";
import { lstat, mkdir, open, rename, rm } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";

const PRIVATE_DIRECTORY_MODE = 0o700;
const PRIVATE_FILE_MODE = 0o600;
const MAXIMUM_DISCOVERY_BYTES = 64 * 1024;

export type PrivateDiscoveryCapabilityReceipt = {
  schemaVersion: 1;
  socketPath: string;
  pid: number;
  bootstrapCapability: string;
  lifecycleReceiptAuthorityId: string | null;
  protocolVersion?: number;
  features?: readonly string[];
  runtimeBuildIdentity?: string;
};

export type PrivateDiscoveryOwner = {
  schemaVersion: 1;
  state: "active" | "stopped" | "crashed";
  actionId: string;
  electionGeneration: number;
  daemonInstanceGeneration: number;
  socketPath: string;
  pid: number;
  bootstrapCapabilityHash: string;
  updatedAt: number;
  exitCode: number | null;
  signal: NodeJS.Signals | null;
};

export type PrivateDiscoveryIdentity = Pick<
  PrivateDiscoveryOwner,
  "actionId" | "electionGeneration" | "daemonInstanceGeneration" | "socketPath" | "pid" | "bootstrapCapabilityHash"
>;

export type PrivateDiscoveryState =
  | { status: "absent" }
  | { status: "active"; receipt: PrivateDiscoveryCapabilityReceipt; owner: PrivateDiscoveryOwner }
  | { status: "terminal"; owner: PrivateDiscoveryOwner }
  | { status: "ambiguous"; message: string; owner?: PrivateDiscoveryOwner; receipt?: PrivateDiscoveryCapabilityReceipt };

export type PrivateDiscoveryPaths = {
  runtimeDirectory: string;
  receiptPath: string;
  ownerPath: string;
};

export class PrivateDiscoveryError extends Error {
  readonly code: string;

  constructor(code: string, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "PrivateDiscoveryError";
    this.code = code;
  }
}

function isErrno(value: unknown, ...codes: string[]): value is NodeJS.ErrnoException {
  return value instanceof Error && "code" in value && typeof value.code === "string" && codes.includes(value.code);
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[], label: string): void {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${label} fields are invalid`);
  }
}

function nonEmptyString(value: unknown, label: string, maximumBytes = 4_096): string {
  if (typeof value !== "string" || value.trim().length === 0 || Buffer.byteLength(value, "utf8") > maximumBytes) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${label} is invalid`);
  }
  return value;
}

function positiveInteger(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${label} is invalid`);
  }
  return value;
}

function runtimeBuildIdentity(value: unknown, label: string): string {
  if (typeof value !== "string" || !/^sha256:[a-f0-9]{64}$/u.test(value)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${label} is invalid`);
  }
  return value;
}

export function parseCapabilityReceipt(value: unknown, socketPath: string): PrivateDiscoveryCapabilityReceipt {
  const receipt = record(value, "daemon discovery receipt");
  const hasProtocolVersion = Object.hasOwn(receipt, "protocolVersion");
  const hasFeatures = Object.hasOwn(receipt, "features");
  if (hasProtocolVersion !== hasFeatures) {
    throw new PrivateDiscoveryError(
      "DAEMON_DISCOVERY_INVALID",
      "daemon discovery protocol evidence is incomplete",
    );
  }
  if (receipt.schemaVersion !== 1 || receipt.socketPath !== socketPath) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery receipt does not match the trusted socket");
  }
  const bootstrapCapability = nonEmptyString(receipt.bootstrapCapability, "daemon discovery capability", 256);
  if (!/^afb_[A-Za-z0-9_-]{43}$/u.test(bootstrapCapability)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery capability is invalid");
  }
  let protocolEvidence: Pick<PrivateDiscoveryCapabilityReceipt, "protocolVersion" | "features"> = {};
  if (hasProtocolVersion) {
    if (
      typeof receipt.protocolVersion !== "number" ||
      !Number.isSafeInteger(receipt.protocolVersion) ||
      receipt.protocolVersion < 1 ||
      !Array.isArray(receipt.features) ||
      !receipt.features.every((feature) => typeof feature === "string" && feature.length > 0) ||
      new Set(receipt.features).size !== receipt.features.length
    ) {
      throw new PrivateDiscoveryError(
        "DAEMON_DISCOVERY_INVALID",
        "daemon discovery protocol evidence is invalid",
      );
    }
    protocolEvidence = {
      protocolVersion: receipt.protocolVersion,
      features: [...receipt.features],
    };
  }
  return {
    schemaVersion: 1,
    socketPath,
    pid: positiveInteger(receipt.pid, "daemon discovery pid"),
    bootstrapCapability,
    lifecycleReceiptAuthorityId: receipt.lifecycleReceiptAuthorityId === null
      ? null
      : nonEmptyString(receipt.lifecycleReceiptAuthorityId, "lifecycle receipt authority ID"),
    ...(receipt.runtimeBuildIdentity === undefined
      ? {}
      : { runtimeBuildIdentity: runtimeBuildIdentity(receipt.runtimeBuildIdentity, "daemon runtime build identity") }),
    ...protocolEvidence,
  };
}

function parseOwner(value: unknown, socketPath: string): PrivateDiscoveryOwner {
  const owner = record(value, "daemon discovery owner");
  exactKeys(owner, [
    "schemaVersion",
    "state",
    "actionId",
    "electionGeneration",
    "daemonInstanceGeneration",
    "socketPath",
    "pid",
    "bootstrapCapabilityHash",
    "updatedAt",
    "exitCode",
    "signal",
  ], "daemon discovery owner");
  if (owner.schemaVersion !== 1 || owner.socketPath !== socketPath) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery owner does not match the trusted socket");
  }
  if (owner.state !== "active" && owner.state !== "stopped" && owner.state !== "crashed") {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery owner state is invalid");
  }
  if (typeof owner.bootstrapCapabilityHash !== "string" || !/^[a-f0-9]{64}$/u.test(owner.bootstrapCapabilityHash)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery capability hash is invalid");
  }
  if (owner.exitCode !== null && (typeof owner.exitCode !== "number" || !Number.isSafeInteger(owner.exitCode) || owner.exitCode < 0)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery exit code is invalid");
  }
  if (owner.signal !== null && (typeof owner.signal !== "string" || !/^SIG[A-Z0-9]+$/u.test(owner.signal))) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery signal is invalid");
  }
  if (owner.state === "active" && (owner.exitCode !== null || owner.signal !== null)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "active daemon discovery contains terminal process state");
  }
  if (owner.state === "stopped" && owner.exitCode === null && owner.signal === null) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "stopped daemon discovery has no process outcome");
  }
  return {
    schemaVersion: 1,
    state: owner.state,
    actionId: nonEmptyString(owner.actionId, "daemon discovery action ID", 1_024),
    electionGeneration: positiveInteger(owner.electionGeneration, "daemon discovery election generation"),
    daemonInstanceGeneration: positiveInteger(owner.daemonInstanceGeneration, "daemon discovery daemon generation"),
    socketPath,
    pid: positiveInteger(owner.pid, "daemon discovery owner pid"),
    bootstrapCapabilityHash: owner.bootstrapCapabilityHash,
    updatedAt: positiveInteger(owner.updatedAt, "daemon discovery update time"),
    exitCode: owner.exitCode,
    signal: owner.signal as NodeJS.Signals | null,
  };
}

function capabilityHash(capability: string): string {
  return createHash("sha256").update(capability).digest("hex");
}

function receiptMatchesOwner(receipt: PrivateDiscoveryCapabilityReceipt, owner: PrivateDiscoveryOwner): boolean {
  return receipt.socketPath === owner.socketPath
    && receipt.pid === owner.pid
    && capabilityHash(receipt.bootstrapCapability) === owner.bootstrapCapabilityHash;
}

function identityMatchesOwner(identity: PrivateDiscoveryIdentity, owner: PrivateDiscoveryOwner): boolean {
  return identity.actionId === owner.actionId
    && identity.electionGeneration === owner.electionGeneration
    && identity.daemonInstanceGeneration === owner.daemonInstanceGeneration
    && identity.socketPath === owner.socketPath
    && identity.pid === owner.pid
    && identity.bootstrapCapabilityHash === owner.bootstrapCapabilityHash;
}

export function privateDiscoveryMatchesBootstrapReady(
  owner: Pick<PrivateDiscoveryOwner, "actionId" | "electionGeneration" | "daemonInstanceGeneration" | "socketPath">,
  ready: { actionId: string; electionGeneration: number; daemonInstanceGeneration: number; socketPath: string },
): boolean {
  return ready.actionId === owner.actionId
    && ready.electionGeneration === owner.electionGeneration
    && ready.daemonInstanceGeneration === owner.daemonInstanceGeneration
    && ready.socketPath === owner.socketPath;
}

export function privateDiscoveryPaths(runtimeDirectory: string): PrivateDiscoveryPaths {
  const directory = resolve(runtimeDirectory);
  return {
    runtimeDirectory: directory,
    receiptPath: join(directory, "fabric-v1.discovery.json"),
    ownerPath: join(directory, "fabric-v1.discovery-owner.json"),
  };
}

export async function ensurePrivateDirectory(path: string): Promise<void> {
  await mkdir(path, { recursive: true, mode: PRIVATE_DIRECTORY_MODE });
  const info = await lstat(path);
  if (!info.isDirectory() || info.isSymbolicLink() || (info.mode & 0o777) !== PRIVATE_DIRECTORY_MODE) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} must be a private 0700 directory`);
  }
  if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} is not owned by the current user`);
  }
}

async function validatePrivateHandle(handle: FileHandle, path: string): Promise<void> {
  const info = await handle.stat();
  if (!info.isFile() || info.nlink !== 1 || (info.mode & 0o777) !== PRIVATE_FILE_MODE) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} must be a single-link 0600 regular file`);
  }
  if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} is not owned by the current user`);
  }
}

async function readPrivateJson(path: string): Promise<unknown | undefined> {
  for (let attempt = 0; attempt < 16; attempt += 1) {
    let handle: FileHandle;
    try {
      handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    } catch (error: unknown) {
      if (isErrno(error, "ENOENT")) return undefined;
      if (isErrno(error, "ELOOP")) {
        throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} must not be a symbolic link`, { cause: error });
      }
      throw error;
    }
    try {
      const info = await handle.stat();
      if (info.nlink === 0) continue;
      await validatePrivateHandle(handle, path);
      if (info.size > MAXIMUM_DISCOVERY_BYTES) {
        throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${path} exceeds the discovery size limit`);
      }
      try {
        return JSON.parse(await handle.readFile("utf8")) as unknown;
      } catch (error: unknown) {
        throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", `${path} contains invalid JSON`, { cause: error });
      }
    } finally {
      await handle.close();
    }
  }
  throw new PrivateDiscoveryError("DAEMON_DISCOVERY_RACE", `${path} changed too often to read safely`);
}

async function existingPrivateFile(path: string): Promise<boolean> {
  try {
    const info = await lstat(path);
    if (!info.isFile() || info.isSymbolicLink() || info.nlink !== 1 || (info.mode & 0o777) !== PRIVATE_FILE_MODE) {
      throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} must be a single-link 0600 regular file`);
    }
    if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
      throw new PrivateDiscoveryError("DAEMON_DISCOVERY_PATH_UNSAFE", `${path} is not owned by the current user`);
    }
    return true;
  } catch (error: unknown) {
    if (isErrno(error, "ENOENT")) return false;
    throw error;
  }
}

async function syncDirectory(path: string): Promise<void> {
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function atomicPrivateJson(path: string, value: unknown): Promise<void> {
  await existingPrivateFile(path);
  const serialized = `${JSON.stringify(value)}\n`;
  if (Buffer.byteLength(serialized, "utf8") > MAXIMUM_DISCOVERY_BYTES) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery receipt exceeds the size limit");
  }
  const temporaryPath = `${path}.tmp-${process.pid}-${randomBytes(12).toString("hex")}`;
  let handle: FileHandle | undefined;
  try {
    handle = await open(
      temporaryPath,
      constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
      PRIVATE_FILE_MODE,
    );
    await validatePrivateHandle(handle, temporaryPath);
    await handle.writeFile(serialized, "utf8");
    await handle.sync();
    await handle.close();
    handle = undefined;
    await rename(temporaryPath, path);
    await existingPrivateFile(path);
    await syncDirectory(resolve(path, ".."));
  } finally {
    await handle?.close().catch(() => undefined);
    await rm(temporaryPath, { force: true });
  }
}

export async function readPrivateDiscovery(paths: PrivateDiscoveryPaths, socketPath: string): Promise<PrivateDiscoveryState> {
  await ensurePrivateDirectory(paths.runtimeDirectory);
  const [receiptValue, ownerValue] = await Promise.all([
    readPrivateJson(paths.receiptPath),
    readPrivateJson(paths.ownerPath),
  ]);
  const receipt = receiptValue === undefined ? undefined : parseCapabilityReceipt(receiptValue, socketPath);
  const owner = ownerValue === undefined ? undefined : parseOwner(ownerValue, socketPath);
  if (receipt === undefined && owner === undefined) return { status: "absent" };
  if (owner === undefined) {
    return { status: "ambiguous", message: "daemon discovery receipt has no generation-bound owner", ...(receipt === undefined ? {} : { receipt }) };
  }
  if (owner.state !== "active") {
    if (receipt !== undefined && !receiptMatchesOwner(receipt, owner)) {
      return { status: "ambiguous", message: "terminal daemon discovery conflicts with the active receipt", owner, receipt };
    }
    return { status: "terminal", owner };
  }
  if (receipt === undefined) {
    return { status: "ambiguous", message: "active daemon discovery owner has no capability receipt", owner };
  }
  if (!receiptMatchesOwner(receipt, owner)) {
    return { status: "ambiguous", message: "daemon discovery receipt does not match its generation-bound owner", owner, receipt };
  }
  return { status: "active", receipt, owner };
}

export async function readPrivateDiscoveryOwner(
  paths: PrivateDiscoveryPaths,
): Promise<PrivateDiscoveryOwner | undefined> {
  await ensurePrivateDirectory(paths.runtimeDirectory);
  const value = await readPrivateJson(paths.ownerPath);
  if (value === undefined) return undefined;
  const candidate = record(value, "daemon discovery owner");
  if (typeof candidate.socketPath !== "string" || !isAbsolute(candidate.socketPath)) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_INVALID", "daemon discovery owner socket path is invalid");
  }
  return parseOwner(value, candidate.socketPath);
}

export async function publishPrivateDiscovery(input: {
  paths: PrivateDiscoveryPaths;
  actionId: string;
  electionGeneration: number;
  daemonInstanceGeneration: number;
  socketPath: string;
  pid: number;
  bootstrapCapability: string;
  lifecycleReceiptAuthorityId: string | null;
  protocolVersion: number;
  features: readonly string[];
  runtimeBuildIdentity: string;
}): Promise<PrivateDiscoveryIdentity> {
  await ensurePrivateDirectory(input.paths.runtimeDirectory);
  const current = await readPrivateDiscovery(input.paths, input.socketPath);
  if (current.status === "active" || current.status === "ambiguous") {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_OWNED", "daemon discovery cannot be replaced without terminal ownership proof");
  }
  if (current.status === "terminal" && current.owner.electionGeneration >= input.electionGeneration) {
    throw new PrivateDiscoveryError("DAEMON_DISCOVERY_GENERATION_STALE", "daemon discovery generation did not advance");
  }
  const identity: PrivateDiscoveryIdentity = {
    actionId: nonEmptyString(input.actionId, "daemon discovery action ID", 1_024),
    electionGeneration: positiveInteger(input.electionGeneration, "daemon discovery election generation"),
    daemonInstanceGeneration: positiveInteger(input.daemonInstanceGeneration, "daemon discovery daemon generation"),
    socketPath: input.socketPath,
    pid: positiveInteger(input.pid, "daemon discovery pid"),
    bootstrapCapabilityHash: capabilityHash(input.bootstrapCapability),
  };
  const owner: PrivateDiscoveryOwner = {
    schemaVersion: 1,
    state: "active",
    ...identity,
    updatedAt: Date.now(),
    exitCode: null,
    signal: null,
  };
  const receipt: PrivateDiscoveryCapabilityReceipt = {
    schemaVersion: 1,
    socketPath: input.socketPath,
    pid: input.pid,
    bootstrapCapability: input.bootstrapCapability,
    lifecycleReceiptAuthorityId: input.lifecycleReceiptAuthorityId,
    protocolVersion: input.protocolVersion,
    features: [...input.features],
    runtimeBuildIdentity: runtimeBuildIdentity(input.runtimeBuildIdentity, "daemon runtime build identity"),
  };
  await atomicPrivateJson(input.paths.ownerPath, owner);
  await atomicPrivateJson(input.paths.receiptPath, receipt);
  return identity;
}

export async function markPrivateDiscoveryTerminal(input: {
  paths: PrivateDiscoveryPaths;
  expected: PrivateDiscoveryIdentity;
  state: "stopped" | "crashed";
  exitCode: number | null;
  signal: NodeJS.Signals | null;
}): Promise<void> {
  const current = await readPrivateDiscovery(input.paths, input.expected.socketPath);
  if (current.status === "terminal" && identityMatchesOwner(input.expected, current.owner)) return;
  if (current.status !== "active" || !identityMatchesOwner(input.expected, current.owner)) {
    throw new PrivateDiscoveryError(
      "DAEMON_DISCOVERY_OWNERSHIP_MISMATCH",
      "daemon discovery terminal transition does not own the current generation",
    );
  }
  const terminal: PrivateDiscoveryOwner = {
    ...current.owner,
    state: input.state,
    updatedAt: Date.now(),
    exitCode: input.exitCode,
    signal: input.signal,
  };
  const receiptValue = await readPrivateJson(input.paths.receiptPath);
  if (receiptValue !== undefined) {
    const receipt = parseCapabilityReceipt(receiptValue, input.expected.socketPath);
    if (!receiptMatchesOwner(receipt, terminal)) {
      throw new PrivateDiscoveryError(
        "DAEMON_DISCOVERY_OWNERSHIP_MISMATCH",
        "daemon discovery capability receipt changed before terminal cleanup",
      );
    }
    await rm(input.paths.receiptPath);
    await syncDirectory(input.paths.runtimeDirectory);
  }
  // A terminal owner must never coexist with a live bootstrap credential.
  // If the process crashes between these writes, active-without-receipt is
  // deliberately ambiguous and fails closed on the next bootstrap.
  await atomicPrivateJson(input.paths.ownerPath, terminal);
}
