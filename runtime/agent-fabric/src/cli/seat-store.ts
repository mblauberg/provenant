import { createHash, randomBytes } from "node:crypto";
import { constants } from "node:fs";
import { chmod, lstat, mkdir, open, readdir, realpath, rename, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { flock } from "fs-ext";

export const MCP_SEATS = ["agy", "claude", "codex", "cursor", "kiro"] as const;
export type McpSeat = (typeof MCP_SEATS)[number];

export type SeatMetadata = {
  schemaVersion: 1;
  projectKey: string;
  projectPath: string;
  generation: string;
  previousGeneration: string | null;
  originKind?: "bootstrap" | "provisioned";
  projectSessionId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runId: string;
  runRevision: number;
  chairAgentId: string;
  chairGeneration: number;
  chairLeaseId: string;
  seat: McpSeat;
  agentId: string;
  principalGeneration: number;
  role: "chair" | "peer";
  credentialPath: string;
  expiresAt: string;
};

export type SeatPaths = {
  projectKey: string;
  projectPath: string;
  directory: string;
  generation: string;
  credentialPath: string;
  metadataPath: string;
};

export type SeatProject = Pick<SeatPaths, "projectKey" | "projectPath" | "directory">;

export type SeatGenerationPointer = {
  schemaVersion: 1;
  projectKey: string;
  previousGeneration: string | null;
  generation: string;
};

export class SeatGenerationChangedError extends Error {
  constructor(
    readonly projectPath: string,
    readonly recordedGeneration: string | null,
    readonly recordedPreviousGeneration: string | null,
    readonly expectedGeneration: string | null,
    phase: string,
  ) {
    super(
      `active MCP seat generation changed ${phase}: project ${projectPath}; ` +
      `recorded generation ${recordedGeneration ?? "none"}; ` +
      `recorded previous generation ${recordedPreviousGeneration ?? "none"}; ` +
      `expected generation ${expectedGeneration ?? "none"}`,
    );
    this.name = "SeatGenerationChangedError";
  }
}

type LegacyBootstrapGenerationMarker = {
  schemaVersion: 1;
  projectKey: string;
  generation: string;
};

const GENERATION_PATTERN = /^[0-9a-f]{64}$/u;
const pointerQueues = new Map<string, Promise<void>>();

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

async function assertPrivateDirectory(path: string): Promise<void> {
  const info = await lstat(path);
  if (info.isSymbolicLink()) throw new Error(`private seat directory must not be a symbolic link: ${path}`);
  if (!info.isDirectory()) throw new Error(`private seat path is not a directory: ${path}`);
  await chmod(path, 0o700);
}

async function createPrivateChildDirectory(path: string): Promise<void> {
  try {
    await mkdir(path, { mode: 0o700 });
  } catch (error: unknown) {
    if (errorCode(error) !== "EEXIST") throw error;
  }
  await assertPrivateDirectory(path);
}

async function rejectUnsafeExistingFile(path: string): Promise<void> {
  try {
    const info = await lstat(path);
    if (info.isSymbolicLink()) throw new Error(`private seat file must not be a symbolic link: ${path}`);
    if (!info.isFile()) throw new Error(`private seat path is not a regular file: ${path}`);
  } catch (error: unknown) {
    if (errorCode(error) !== "ENOENT") throw error;
  }
}

async function atomicPrivateWrite(path: string, contents: string): Promise<void> {
  await rejectUnsafeExistingFile(path);
  const temporaryPath = `${path}.tmp-${process.pid}-${randomBytes(12).toString("hex")}`;
  const flags = constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW;
  let handle: Awaited<ReturnType<typeof open>> | undefined;
  try {
    handle = await open(temporaryPath, flags, 0o600);
    await handle.writeFile(contents, "utf8");
    await handle.chmod(0o600);
    await handle.sync();
    await handle.close();
    handle = undefined;
    await rejectUnsafeExistingFile(path);
    await rename(temporaryPath, path);
    const installed = await lstat(path);
    if (!installed.isFile() || installed.isSymbolicLink()) {
      throw new Error(`private seat write did not produce a regular file: ${path}`);
    }
    await chmod(path, 0o600);
  } finally {
    await handle?.close().catch(() => undefined);
    await rm(temporaryPath, { force: true }).catch(() => undefined);
  }
}

async function syncDirectory(path: string): Promise<void> {
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const info = await handle.stat();
    if (!info.isDirectory()) throw new Error(`private seat path is not a directory: ${path}`);
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function flockPromise(fileDescriptor: number, mode: "ex" | "un"): Promise<void> {
  await new Promise<void>((resolvePromise, reject) => {
    flock(fileDescriptor, mode, (error) => error === null ? resolvePromise() : reject(error));
  });
}

async function withPointerLock<T>(directory: string, operation: () => Promise<T>): Promise<T> {
  const lockPath = join(directory, "current.lock");
  const previous = pointerQueues.get(lockPath) ?? Promise.resolve();
  let releaseQueue!: () => void;
  const queued = new Promise<void>((resolveQueue) => { releaseQueue = resolveQueue; });
  const tail = previous.then(() => queued);
  pointerQueues.set(lockPath, tail);
  await previous;
  let handle: Awaited<ReturnType<typeof open>> | undefined;
  try {
    handle = await open(
      lockPath,
      constants.O_RDWR | constants.O_CREAT | constants.O_NOFOLLOW,
      0o600,
    );
    const info = await handle.stat();
    if (!info.isFile() || info.isSymbolicLink() || info.nlink !== 1 || (info.mode & 0o777) !== 0o600) {
      throw new Error(`MCP seat generation lock must be a private regular file: ${lockPath}`);
    }
    await flockPromise(handle.fd, "ex");
    try {
      return await operation();
    } finally {
      await flockPromise(handle.fd, "un");
    }
  } finally {
    await handle?.close().catch(() => undefined);
    releaseQueue();
    if (pointerQueues.get(lockPath) === tail) pointerQueues.delete(lockPath);
  }
}

async function readPrivateFile(path: string): Promise<string> {
  const before = await lstat(path);
  if (!before.isFile() || before.isSymbolicLink() || (before.mode & 0o077) !== 0) {
    throw new Error(`private seat file must be a private regular file: ${path}`);
  }
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.dev !== before.dev || opened.ino !== before.ino || (opened.mode & 0o077) !== 0) {
      throw new Error(`private seat file changed while opening: ${path}`);
    }
    return await handle.readFile("utf8");
  } finally {
    await handle.close();
  }
}

async function generationSeatExpiryState(input: {
  directory: string;
  generation: string;
  projectKey: string;
  projectPath: string;
  nowMs: number;
}): Promise<"expired" | "unexpired" | "mixed" | "invalid"> {
  const generationDirectory = join(input.directory, "generations", input.generation);
  try {
    const directoryInfo = await lstat(generationDirectory);
    if (
      !directoryInfo.isDirectory() ||
      directoryInfo.isSymbolicLink() ||
      (directoryInfo.mode & 0o077) !== 0
    ) {
      return "invalid";
    }
    const entries = (await readdir(generationDirectory)).sort();
    const metadataFiles = entries.filter((entry) => entry.endsWith(".json"));
    if (metadataFiles.length === 0) return "invalid";
    const expectedEntries = metadataFiles
      .flatMap((entry) => [entry, `${entry.slice(0, -".json".length)}.cap`])
      .sort();
    if (
      entries.length !== expectedEntries.length ||
      entries.some((entry, index) => entry !== expectedEntries[index])
    ) {
      return "invalid";
    }
    let state: "expired" | "unexpired" | undefined;
    for (const metadataFile of metadataFiles) {
      const seat = metadataFile.slice(0, -".json".length);
      if (!(MCP_SEATS as readonly string[]).includes(seat)) return "invalid";
      const credentialPath = join(generationDirectory, `${seat}.cap`);
      const credentialInfo = await lstat(credentialPath);
      if (
        !credentialInfo.isFile() ||
        credentialInfo.isSymbolicLink() ||
        (credentialInfo.mode & 0o077) !== 0
      ) {
        return "invalid";
      }
      const metadata: unknown = JSON.parse(await readPrivateFile(join(generationDirectory, metadataFile)));
      if (
        typeof metadata !== "object" ||
        metadata === null ||
        Array.isArray(metadata) ||
        !("projectKey" in metadata) ||
        metadata.projectKey !== input.projectKey ||
        !("projectPath" in metadata) ||
        metadata.projectPath !== input.projectPath ||
        !("generation" in metadata) ||
        metadata.generation !== input.generation ||
        !("seat" in metadata) ||
        metadata.seat !== seat ||
        !("credentialPath" in metadata) ||
        metadata.credentialPath !== credentialPath ||
        !("expiresAt" in metadata) ||
        typeof metadata.expiresAt !== "string"
      ) {
        return "invalid";
      }
      const expiresAt = Date.parse(metadata.expiresAt);
      if (
        !Number.isFinite(expiresAt) ||
        new Date(expiresAt).toISOString() !== metadata.expiresAt
      ) {
        return "invalid";
      }
      const seatState = expiresAt <= input.nowMs ? "expired" : "unexpired";
      if (state !== undefined && state !== seatState) return "mixed";
      state = seatState;
    }
    return state ?? "invalid";
  } catch {
    return "invalid";
  }
}

function parseGenerationPointer(pointer: unknown, pointerPath: string, key: string): SeatGenerationPointer {
  if (
    typeof pointer !== "object" || pointer === null || Array.isArray(pointer) ||
    Object.keys(pointer).sort().join(",") !== "generation,previousGeneration,projectKey,schemaVersion" ||
    !("schemaVersion" in pointer) || pointer.schemaVersion !== 1 ||
    !("projectKey" in pointer) || pointer.projectKey !== key ||
    !("previousGeneration" in pointer) ||
    (pointer.previousGeneration !== null &&
      (typeof pointer.previousGeneration !== "string" || !GENERATION_PATTERN.test(pointer.previousGeneration))) ||
    !("generation" in pointer) || typeof pointer.generation !== "string" ||
    !GENERATION_PATTERN.test(pointer.generation) ||
    pointer.previousGeneration === pointer.generation
  ) {
    throw new Error(`MCP seat generation pointer is invalid: ${pointerPath}`);
  }
  return pointer as SeatGenerationPointer;
}

function parseLegacyBootstrapMarker(marker: unknown, markerPath: string, key: string): LegacyBootstrapGenerationMarker {
  if (
    typeof marker !== "object" || marker === null || Array.isArray(marker) ||
    Object.keys(marker).sort().join(",") !== "generation,projectKey,schemaVersion" ||
    !("schemaVersion" in marker) || marker.schemaVersion !== 1 ||
    !("projectKey" in marker) || marker.projectKey !== key ||
    !("generation" in marker) || typeof marker.generation !== "string" ||
    !GENERATION_PATTERN.test(marker.generation)
  ) {
    throw new Error(`legacy bootstrap generation marker is invalid: ${markerPath}`);
  }
  return marker as LegacyBootstrapGenerationMarker;
}

async function activeGeneration(directory: string, key: string): Promise<string> {
  const pointerPath = join(directory, "current.json");
  return parseGenerationPointer(JSON.parse(await readPrivateFile(pointerPath)), pointerPath, key).generation;
}

export async function readActiveSeatGeneration(input: {
  stateDirectory: string;
  projectPath: string;
}): Promise<SeatGenerationPointer | null> {
  const root = await resolveSeatProject({ stateDirectory: input.stateDirectory, project: input.projectPath });
  const pointerPath = join(root.directory, "current.json");
  try {
    return parseGenerationPointer(JSON.parse(await readPrivateFile(pointerPath)), pointerPath, root.projectKey);
  } catch (error: unknown) {
    if (errorCode(error) === "ENOENT") return null;
    throw error;
  }
}

export async function readLegacyBootstrapSeatGeneration(input: {
  stateDirectory: string;
  projectPath: string;
}): Promise<string | null> {
  const root = await resolveSeatProject({ stateDirectory: input.stateDirectory, project: input.projectPath });
  const markerPath = join(root.directory, "legacy-bootstrap.json");
  try {
    return parseLegacyBootstrapMarker(
      JSON.parse(await readPrivateFile(markerPath)),
      markerPath,
      root.projectKey,
    ).generation;
  } catch (error: unknown) {
    if (errorCode(error) === "ENOENT") return null;
    throw error;
  }
}

export async function markLegacyBootstrapSeatGeneration(input: {
  stateDirectory: string;
  projectPath: string;
  generation: string;
}): Promise<"recorded" | "already-recorded"> {
  if (!GENERATION_PATTERN.test(input.generation)) throw new Error("legacy bootstrap generation is invalid");
  const root = await resolveSeatProject({ stateDirectory: input.stateDirectory, project: input.projectPath });
  return await withPointerLock(root.directory, async () => {
    const active = await readActiveSeatGeneration(input);
    if (active?.generation !== input.generation) {
      throw new SeatGenerationChangedError(
        root.projectPath,
        active?.generation ?? null,
        active?.previousGeneration ?? null,
        input.generation,
        "before legacy bootstrap marking",
      );
    }
    // Replaying an already-marked generation must not rewrite the marker: the
    // lifecycle reports such a replay as unmutated, and an unconditional write
    // would contradict that on every repeat invocation.
    if (await readLegacyBootstrapSeatGeneration(input) === input.generation) return "already-recorded";
    const marker: LegacyBootstrapGenerationMarker = {
      schemaVersion: 1,
      projectKey: root.projectKey,
      generation: input.generation,
    };
    await atomicPrivateWrite(
      join(root.directory, "legacy-bootstrap.json"),
      `${JSON.stringify(marker, null, 2)}\n`,
    );
    await syncDirectory(root.directory);
    return "recorded";
  });
}

export function parseMcpSeat(value: string): McpSeat {
  if ((MCP_SEATS as readonly string[]).includes(value)) return value as McpSeat;
  throw new Error(`unsupported MCP seat ${JSON.stringify(value)}; expected one of ${MCP_SEATS.join(",")}`);
}

export async function canonicalProjectPath(project: string): Promise<string> {
  const path = await realpath(resolve(project));
  const info = await lstat(path);
  if (!info.isDirectory()) throw new Error(`MCP project is not a directory: ${path}`);
  return path;
}

export function projectKey(projectPath: string): string {
  return createHash("sha256").update(projectPath).digest("hex").slice(0, 24);
}

export async function resolveSeatProject(input: {
  stateDirectory: string;
  project: string;
  createDirectories?: boolean;
}): Promise<SeatProject> {
  const projectPath = await canonicalProjectPath(input.project);
  const key = projectKey(projectPath);
  const seatsDirectory = join(input.stateDirectory, "seats");
  const directory = join(seatsDirectory, key);
  if (input.createDirectories === true) {
    await assertPrivateDirectory(input.stateDirectory);
    await createPrivateChildDirectory(seatsDirectory);
    await createPrivateChildDirectory(directory);
  }
  return { projectKey: key, projectPath, directory };
}

export async function resolveSeatPaths(input: {
  stateDirectory: string;
  project: string;
  seat: McpSeat;
}): Promise<SeatPaths> {
  const project = await resolveSeatProject(input);
  const generation = await activeGeneration(project.directory, project.projectKey);
  const activeDirectory = join(project.directory, "generations", generation);
  return {
    ...project,
    generation,
    credentialPath: join(activeDirectory, `${input.seat}.cap`),
    metadataPath: join(activeDirectory, `${input.seat}.json`),
  };
}

export async function installSeatGeneration(input: {
  stateDirectory: string;
  projectPath: string;
  generation: string;
  expectedPreviousGeneration: string | null;
  seats: Array<{ metadata: Omit<SeatMetadata, "credentialPath">; credential: string }>;
  allowMissingPreviousGeneration?: boolean;
  allowStaleGenerationReconciliation?: boolean;
  now?: Date;
  beforeActivate?: () => void | Promise<void>;
}): Promise<Array<{ seat: McpSeat; credentialPath: string; metadataPath: string }>> {
  if (!GENERATION_PATTERN.test(input.generation)) throw new Error("MCP seat generation is invalid");
  if (input.expectedPreviousGeneration !== null && !GENERATION_PATTERN.test(input.expectedPreviousGeneration)) {
    throw new Error("expected previous MCP seat generation is invalid");
  }
  if (input.expectedPreviousGeneration === input.generation) {
    throw new Error("MCP seat generation cannot replace itself");
  }
  const fixedNowMs = input.now?.getTime();
  if (fixedNowMs !== undefined && !Number.isFinite(fixedNowMs)) {
    throw new Error("MCP seat generation installation time is invalid");
  }
  if (input.seats.length === 0 || new Set(input.seats.map(({ metadata }) => metadata.seat)).size !== input.seats.length) {
    throw new Error("MCP seat generation must contain a non-empty distinct roster");
  }
  const first = input.seats[0];
  if (first === undefined) throw new Error("MCP seat generation must contain a seat");
  const root = await resolveSeatProject({
    stateDirectory: input.stateDirectory,
    project: input.projectPath,
    createDirectories: true,
  });
  const generationsDirectory = join(root.directory, "generations");
  const generationDirectory = join(generationsDirectory, input.generation);
  await createPrivateChildDirectory(generationsDirectory);
  const stagingDirectory = join(generationsDirectory, `.staging-${input.generation}-${process.pid}-${randomBytes(12).toString("hex")}`);
  await createPrivateChildDirectory(stagingDirectory);
  const written: Array<{ seat: McpSeat; credentialPath: string; metadataPath: string }> = [];
  const expected = new Map<string, string>();
  try {
    for (const { metadata, credential } of input.seats) {
      if (!/^afc_[A-Za-z0-9_-]{43}$/u.test(credential)) throw new Error("daemon returned an invalid seat credential");
      if (metadata.projectKey !== root.projectKey || metadata.projectPath !== root.projectPath) {
        throw new Error(`seat metadata does not match its project-keyed path for ${metadata.seat}`);
      }
      if (
        metadata.generation !== input.generation ||
        metadata.previousGeneration !== input.expectedPreviousGeneration
      ) {
        throw new Error(`seat metadata does not match its generation for ${metadata.seat}`);
      }
      const credentialPath = join(generationDirectory, `${metadata.seat}.cap`);
      const metadataPath = join(generationDirectory, `${metadata.seat}.json`);
      const installedMetadata: SeatMetadata = { ...metadata, credentialPath };
      expected.set(`${metadata.seat}.cap`, credential);
      expected.set(`${metadata.seat}.json`, `${JSON.stringify(installedMetadata, null, 2)}\n`);
      await atomicPrivateWrite(join(stagingDirectory, `${metadata.seat}.cap`), credential);
      await atomicPrivateWrite(join(stagingDirectory, `${metadata.seat}.json`), expected.get(`${metadata.seat}.json`) ?? "");
      written.push({ seat: metadata.seat, credentialPath, metadataPath });
    }
    await input.beforeActivate?.();
    try {
      await rename(stagingDirectory, generationDirectory);
    } catch (error: unknown) {
      if (!(["EEXIST", "ENOTEMPTY"] as Array<string | undefined>).includes(errorCode(error))) throw error;
      const entries = (await readdir(generationDirectory)).sort();
      if (entries.length !== expected.size || entries.some((entry) => !expected.has(entry))) {
        throw new Error(`existing MCP seat generation differs from requested immutable generation: ${input.generation}`);
      }
      for (const [file, contents] of expected) {
        if (await readPrivateFile(join(generationDirectory, file)) !== contents) {
          throw new Error(`existing MCP seat generation differs from requested immutable generation: ${input.generation}`);
        }
      }
    }
    await syncDirectory(generationDirectory);
    await syncDirectory(generationsDirectory);
    await withPointerLock(root.directory, async () => {
      const cutoverNowMs = fixedNowMs ?? Date.now();
      const active = await readActiveSeatGeneration({
        stateDirectory: input.stateDirectory,
        projectPath: input.projectPath,
      });
      if (active?.generation === input.generation) {
        if (active.previousGeneration !== input.expectedPreviousGeneration) {
          const reconciledPredecessor = input.allowStaleGenerationReconciliation === true &&
            active.previousGeneration !== null &&
            await generationSeatExpiryState({
              directory: root.directory,
              generation: active.previousGeneration,
              projectKey: root.projectKey,
              projectPath: root.projectPath,
              nowMs: cutoverNowMs,
            }) === "expired" &&
            await generationSeatExpiryState({
              directory: root.directory,
              generation: active.generation,
              projectKey: root.projectKey,
              projectPath: root.projectPath,
              nowMs: cutoverNowMs,
            }) === "unexpired";
          if (!reconciledPredecessor) {
            throw new SeatGenerationChangedError(
              root.projectPath,
              active.generation,
              active.previousGeneration,
              input.expectedPreviousGeneration,
              "while replaying its predecessor",
            );
          }
        }
        return;
      }
      const crashConvergence = active === null &&
        input.expectedPreviousGeneration !== null &&
        input.allowMissingPreviousGeneration === true;
      const staleGenerationReconciliation = active !== null &&
        input.allowStaleGenerationReconciliation === true &&
        await generationSeatExpiryState({
          directory: root.directory,
          generation: active.generation,
          projectKey: root.projectKey,
          projectPath: root.projectPath,
          nowMs: cutoverNowMs,
        }) === "expired" &&
        await generationSeatExpiryState({
          directory: root.directory,
          generation: input.generation,
          projectKey: root.projectKey,
          projectPath: root.projectPath,
          nowMs: cutoverNowMs,
        }) === "unexpired";
      if (!crashConvergence && (active?.generation ?? null) !== input.expectedPreviousGeneration) {
        if (!staleGenerationReconciliation) {
          throw new SeatGenerationChangedError(
            root.projectPath,
            active?.generation ?? null,
            active?.previousGeneration ?? null,
            input.expectedPreviousGeneration,
            "before filesystem cutover",
          );
        }
      }
      const pointer: SeatGenerationPointer = {
        schemaVersion: 1,
        projectKey: root.projectKey,
        previousGeneration: staleGenerationReconciliation
          ? active.generation
          : input.expectedPreviousGeneration,
        generation: input.generation,
      };
      await atomicPrivateWrite(join(root.directory, "current.json"), `${JSON.stringify(pointer, null, 2)}\n`);
      await syncDirectory(root.directory);
    });
    return written;
  } finally {
    await rm(stagingDirectory, { recursive: true, force: true }).catch(() => undefined);
  }
}
