import { randomBytes } from "node:crypto";
import { constants } from "node:fs";
import type { FileHandle } from "node:fs/promises";
import { lstat, mkdir, open, rename, rm } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";

import { flock } from "fs-ext";

const PRIVATE_DIRECTORY_MODE = 0o700;
const PRIVATE_FILE_MODE = 0o600;
const MAX_RECEIPT_BYTES = 64 * 1024;
const MAX_ATTEMPT_BYTES = 8 * 1024;
const MAX_ATTEMPT_JOURNAL_BYTES = 8 * 1024 * 1024;

export const BOOTSTRAP_PHASES = [
  "election-acquired",
  "socket-recheck",
  "spawn",
  "database-owned",
  "migrations-complete",
  "recovery-complete",
  "socket-bound",
  "ready-receipt",
  "handshake",
  "audit-import",
] as const;

export type BootstrapPhase = (typeof BOOTSTRAP_PHASES)[number];
export type BootstrapAttemptStatus = "progress" | "failed" | "ambiguous" | "expired" | "succeeded";

export type BootstrapReadyEvidence = {
  databaseOwned: true;
  migrationsComplete: true;
  recoveryComplete: true;
  socketBound: true;
};

export type BootstrapReadyReceipt = {
  schemaVersion: 1;
  actionId: string;
  electionGeneration: number;
  daemonInstanceGeneration: number;
  socketPath: string;
  protocolVersion: number;
  features: readonly string[];
  runtimeBuildIdentity?: string;
  readyAt: number;
  evidence: BootstrapReadyEvidence;
};

export type BootstrapHeldLease = {
  schemaVersion: 1;
  actionId: string;
  electionGeneration: number;
  status: "held";
  acquiredAt: number;
  expiresAt: number;
};

export type BootstrapTerminalLease = {
  schemaVersion: 1;
  actionId: string;
  electionGeneration: number;
  status: "succeeded" | "failed" | "ambiguous" | "expired";
  acquiredAt: number;
  terminalAt: number;
  code: string;
  message: string;
};

export type BootstrapLeaseReceipt = BootstrapHeldLease | BootstrapTerminalLease;

export type BootstrapAttempt = {
  schemaVersion: 1;
  actionId: string;
  electionGeneration: number;
  phase: BootstrapPhase;
  status: BootstrapAttemptStatus;
  recordedAt: number;
  detail?: string;
};

export type BootstrapGenerationOutcome =
  | { kind: "ready"; receipt: BootstrapReadyReceipt }
  | { kind: "terminal"; receipt: BootstrapTerminalLease };

export type BootstrapElectionInspection =
  | { status: "absent" }
  | { status: "active"; ownership: "kernel-lock" }
  | { status: "ready"; receipt: BootstrapReadyReceipt }
  | { status: "terminal"; receipt: BootstrapTerminalLease };

export class BootstrapElectionError extends Error {
  readonly code: string;

  constructor(code: string, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "BootstrapElectionError";
    this.code = code;
  }
}

export type ElectionLockHandle = {
  release(): Promise<void>;
};

export type ElectionLockProbe =
  | { status: "held" }
  | { status: "acquired"; handle: ElectionLockHandle };

export type ElectionLockPort = {
  tryAcquire(lockPath: string): Promise<ElectionLockHandle | undefined>;
  probe(lockPath: string): Promise<ElectionLockProbe>;
};

export type BootstrapElectionPaths = {
  runtimeDirectory: string;
  lockPath: string;
  leasePath: string;
  readyPath: string;
  attemptsPath: string;
};

export type BootstrapGeneration = {
  readonly actionId: string;
  readonly electionGeneration: number;
  appendAttempt(phase: BootstrapPhase, status: BootstrapAttemptStatus, detail?: string): Promise<void>;
  publishReady(input: Omit<BootstrapReadyReceipt, "schemaVersion" | "actionId" | "electionGeneration" | "readyAt">): Promise<BootstrapReadyReceipt>;
  confirmReady(): Promise<void>;
  recordTerminal(input: {
    status: "failed" | "ambiguous" | "expired";
    code: string;
    message: string;
    phase?: BootstrapPhase;
  }): Promise<BootstrapTerminalLease>;
  waitForOutcome(deadlineAt?: number): Promise<BootstrapGenerationOutcome>;
};

export type HeldBootstrapElection = {
  readonly paths: BootstrapElectionPaths;
  beginGeneration(): Promise<BootstrapGeneration>;
  readCurrentOutcome(): Promise<BootstrapGenerationOutcome | undefined>;
};

export type BootstrapElectionResult<T> =
  | { role: "owner"; value: T }
  | { role: "observer"; outcome: BootstrapGenerationOutcome };

export type BootstrapElectionOptions = {
  runtimeDirectory: string;
  lockPort?: ElectionLockPort;
  clock?: () => number;
  sleep?: (milliseconds: number) => Promise<void>;
  leaseDurationMs?: number;
  waitTimeoutMs?: number;
  pollIntervalMs?: number;
};

function isErrno(value: unknown, ...codes: string[]): value is NodeJS.ErrnoException {
  return value instanceof Error && "code" in value && typeof value.code === "string" && codes.includes(value.code);
}

function positiveInteger(value: number, label: string): number {
  if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${label} must be a positive safe integer`);
  return value;
}

function nonEmptyString(value: unknown, label: string, maximumBytes = 1_024): string {
  if (typeof value !== "string" || value.trim().length === 0 || Buffer.byteLength(value, "utf8") > maximumBytes) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", `${label} is invalid`);
  }
  return value;
}

function receiptRecord(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", `${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(record: Record<string, unknown>, expected: readonly string[], label: string): void {
  const actual = Object.keys(record).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", `${label} fields are invalid`);
  }
}

function parsePositiveInteger(value: unknown, label: string): number {
  if (typeof value !== "number") throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", `${label} is invalid`);
  return positiveInteger(value, label);
}

function parseRuntimeBuildIdentity(value: unknown): string {
  if (typeof value !== "string" || !/^sha256:[a-f0-9]{64}$/u.test(value)) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap ready runtime build identity is invalid");
  }
  return value;
}

function parseLease(value: unknown): BootstrapLeaseReceipt {
  const record = receiptRecord(value, "bootstrap lease");
  const status = record.status;
  if (status === "held") {
    exactKeys(record, ["schemaVersion", "actionId", "electionGeneration", "status", "acquiredAt", "expiresAt"], "bootstrap lease");
    if (record.schemaVersion !== 1) throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap lease schema is invalid");
    const acquiredAt = parsePositiveInteger(record.acquiredAt, "bootstrap lease acquiredAt");
    const expiresAt = parsePositiveInteger(record.expiresAt, "bootstrap lease expiresAt");
    if (expiresAt <= acquiredAt) throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap lease expiry is invalid");
    return {
      schemaVersion: 1,
      actionId: nonEmptyString(record.actionId, "bootstrap lease actionId"),
      electionGeneration: parsePositiveInteger(record.electionGeneration, "bootstrap lease generation"),
      status,
      acquiredAt,
      expiresAt,
    };
  }
  if (status !== "succeeded" && status !== "failed" && status !== "ambiguous" && status !== "expired") {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap lease status is invalid");
  }
  exactKeys(record, ["schemaVersion", "actionId", "electionGeneration", "status", "acquiredAt", "terminalAt", "code", "message"], "bootstrap lease");
  if (record.schemaVersion !== 1) throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap lease schema is invalid");
  return {
    schemaVersion: 1,
    actionId: nonEmptyString(record.actionId, "bootstrap lease actionId"),
    electionGeneration: parsePositiveInteger(record.electionGeneration, "bootstrap lease generation"),
    status,
    acquiredAt: parsePositiveInteger(record.acquiredAt, "bootstrap lease acquiredAt"),
    terminalAt: parsePositiveInteger(record.terminalAt, "bootstrap lease terminalAt"),
    code: nonEmptyString(record.code, "bootstrap lease code"),
    message: nonEmptyString(record.message, "bootstrap lease message", 4_096),
  };
}

function parseReady(value: unknown): BootstrapReadyReceipt {
  const record = receiptRecord(value, "bootstrap ready receipt");
  const hasRuntimeBuildIdentity = Object.hasOwn(record, "runtimeBuildIdentity");
  exactKeys(record, [
    "schemaVersion", "actionId", "electionGeneration", "daemonInstanceGeneration", "socketPath",
    "protocolVersion", "features", "readyAt", "evidence",
    ...(hasRuntimeBuildIdentity ? ["runtimeBuildIdentity"] : []),
  ], "bootstrap ready receipt");
  if (record.schemaVersion !== 1) throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap ready schema is invalid");
  const evidence = receiptRecord(record.evidence, "bootstrap ready evidence");
  exactKeys(evidence, ["databaseOwned", "migrationsComplete", "recoveryComplete", "socketBound"], "bootstrap ready evidence");
  if (evidence.databaseOwned !== true || evidence.migrationsComplete !== true || evidence.recoveryComplete !== true || evidence.socketBound !== true) {
    throw new BootstrapElectionError("BOOTSTRAP_NOT_READY", "bootstrap ready evidence is incomplete");
  }
  if (!Array.isArray(record.features) || !record.features.every((feature) => typeof feature === "string" && feature.length > 0)) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap ready features are invalid");
  }
  if (new Set(record.features).size !== record.features.length) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap ready features contain duplicates");
  }
  const socketPath = nonEmptyString(record.socketPath, "bootstrap ready socketPath", 4_096);
  if (!isAbsolute(socketPath)) throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap ready socketPath must be absolute");
  return {
    schemaVersion: 1,
    actionId: nonEmptyString(record.actionId, "bootstrap ready actionId"),
    electionGeneration: parsePositiveInteger(record.electionGeneration, "bootstrap ready generation"),
    daemonInstanceGeneration: parsePositiveInteger(record.daemonInstanceGeneration, "bootstrap ready daemon generation"),
    socketPath,
    protocolVersion: parsePositiveInteger(record.protocolVersion, "bootstrap ready protocol version"),
    features: [...record.features],
    ...(hasRuntimeBuildIdentity ? { runtimeBuildIdentity: parseRuntimeBuildIdentity(record.runtimeBuildIdentity) } : {}),
    readyAt: parsePositiveInteger(record.readyAt, "bootstrap ready time"),
    evidence: { databaseOwned: true, migrationsComplete: true, recoveryComplete: true, socketBound: true },
  };
}

async function flockPromise(fileDescriptor: number, mode: "shnb" | "exnb" | "un"): Promise<void> {
  await new Promise<void>((resolvePromise, reject) => {
    flock(fileDescriptor, mode, (error) => error === null ? resolvePromise() : reject(error));
  });
}

async function validatePrivateHandle(handle: FileHandle, path: string): Promise<void> {
  const info = await handle.stat();
  if (!info.isFile() || info.nlink !== 1 || (info.mode & 0o777) !== PRIVATE_FILE_MODE) {
    throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} must be a single-link 0600 regular file`);
  }
  if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
    throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} is not owned by the current user`);
  }
}

export const FLOCK_ELECTION_LOCK_PORT: ElectionLockPort = Object.freeze({
  async tryAcquire(lockPath: string): Promise<ElectionLockHandle | undefined> {
    let handle: FileHandle | undefined;
    try {
      handle = await open(lockPath, constants.O_RDWR | constants.O_CREAT | constants.O_NOFOLLOW, PRIVATE_FILE_MODE);
      await validatePrivateHandle(handle, lockPath);
      try {
        await flockPromise(handle.fd, "exnb");
      } catch (error: unknown) {
        if (isErrno(error, "EAGAIN", "EACCES", "EWOULDBLOCK")) {
          await handle.close();
          return undefined;
        }
        throw error;
      }
      await validatePrivateHandle(handle, lockPath);
      let released = false;
      const lockedHandle = handle;
      return {
        async release(): Promise<void> {
          if (released) return;
          released = true;
          try {
            await flockPromise(lockedHandle.fd, "un");
          } finally {
            await lockedHandle.close();
          }
        },
      };
    } catch (error: unknown) {
      await handle?.close().catch(() => undefined);
      if (isErrno(error, "ELOOP")) {
        throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${lockPath} must not be a symbolic link`, { cause: error });
      }
      throw error;
    }
  },
  async probe(lockPath: string): Promise<ElectionLockProbe> {
    let handle: FileHandle | undefined;
    try {
      // The lock file is a stable private runtime artifact. Creating it here
      // closes the absent-to-created race. Shared inspection locks coexist,
      // while bootstrap's exclusive ownership remains excluded.
      handle = await open(
        lockPath,
        constants.O_RDWR | constants.O_CREAT | constants.O_NOFOLLOW,
        PRIVATE_FILE_MODE,
      );
      await validatePrivateHandle(handle, lockPath);
      try {
        await flockPromise(handle.fd, "shnb");
      } catch (error: unknown) {
        if (isErrno(error, "EAGAIN", "EACCES", "EWOULDBLOCK")) {
          await handle.close();
          return { status: "held" };
        }
        throw error;
      }
      await validatePrivateHandle(handle, lockPath);
      let released = false;
      const lockedHandle = handle;
      return {
        status: "acquired",
        handle: {
          async release(): Promise<void> {
            if (released) return;
            released = true;
            try {
              await flockPromise(lockedHandle.fd, "un");
            } finally {
              await lockedHandle.close();
            }
          },
        },
      };
    } catch (error: unknown) {
      await handle?.close().catch(() => undefined);
      if (isErrno(error, "ELOOP")) {
        throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${lockPath} must not be a symbolic link`, { cause: error });
      }
      throw error;
    }
  },
});

async function ensurePrivateDirectory(path: string): Promise<void> {
  await mkdir(path, { recursive: true, mode: PRIVATE_DIRECTORY_MODE });
  const info = await lstat(path);
  if (!info.isDirectory() || info.isSymbolicLink() || (info.mode & 0o777) !== PRIVATE_DIRECTORY_MODE) {
    throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} must be a 0700 directory`);
  }
  if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
    throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} is not owned by the current user`);
  }
}

async function validateExistingPrivateFile(path: string): Promise<boolean> {
  try {
    const info = await lstat(path);
    if (!info.isFile() || info.isSymbolicLink() || info.nlink !== 1 || (info.mode & 0o777) !== PRIVATE_FILE_MODE) {
      throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} must be a single-link 0600 regular file`);
    }
    if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
      throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} is not owned by the current user`);
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
  await validateExistingPrivateFile(path);
  const serialized = `${JSON.stringify(value)}\n`;
  if (Buffer.byteLength(serialized, "utf8") > MAX_RECEIPT_BYTES) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_TOO_LARGE", "bootstrap receipt is too large");
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
    await validateExistingPrivateFile(path);
    await syncDirectory(resolve(path, ".."));
  } finally {
    await handle?.close().catch(() => undefined);
    await rm(temporaryPath, { force: true });
  }
}

async function appendPrivateJson(path: string, value: unknown): Promise<void> {
  const serialized = `${JSON.stringify(value)}\n`;
  if (Buffer.byteLength(serialized, "utf8") > MAX_ATTEMPT_BYTES) {
    throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_TOO_LARGE", "bootstrap attempt is too large");
  }
  const handle = await open(
    path,
    constants.O_WRONLY | constants.O_APPEND | constants.O_CREAT | constants.O_NOFOLLOW,
    PRIVATE_FILE_MODE,
  ).catch((error: unknown) => {
    if (isErrno(error, "ELOOP")) {
      throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} must not be a symbolic link`, { cause: error });
    }
    throw error;
  });
  try {
    await validatePrivateHandle(handle, path);
    const info = await handle.stat();
    if (info.size + Buffer.byteLength(serialized, "utf8") > MAX_ATTEMPT_JOURNAL_BYTES) {
      throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_TOO_LARGE", "bootstrap attempt journal reached its safety bound");
    }
    await handle.writeFile(serialized, "utf8");
    await handle.sync();
  } finally {
    await handle.close();
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
        throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} must not be a symbolic link`, { cause: error });
      }
      throw error;
    }
    try {
      const info = await handle.stat();
      // Atomic rename can unlink the complete old inode after open. Retry the
      // pathname instead of misclassifying that expected race as a hard link.
      if (info.nlink === 0) continue;
      if (!info.isFile() || info.nlink !== 1 || (info.mode & 0o777) !== PRIVATE_FILE_MODE) {
        throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} must be a single-link 0600 regular file`);
      }
      if (typeof process.getuid === "function" && info.uid !== process.getuid()) {
        throw new BootstrapElectionError("BOOTSTRAP_PATH_UNSAFE", `${path} is not owned by the current user`);
      }
      if (info.size > MAX_RECEIPT_BYTES) throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_TOO_LARGE", `${path} is too large`);
      try {
        return JSON.parse(await handle.readFile("utf8")) as unknown;
      } catch (error: unknown) {
        if (error instanceof BootstrapElectionError) throw error;
        throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", `${path} contains invalid JSON`, { cause: error });
      }
    } finally {
      await handle.close();
    }
  }
  throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_RACE", `${path} changed too often to read safely`);
}

export class BootstrapElection {
  readonly paths: BootstrapElectionPaths;
  readonly #lockPort: ElectionLockPort;
  readonly #clock: () => number;
  readonly #sleep: (milliseconds: number) => Promise<void>;
  readonly #leaseDurationMs: number;
  readonly #waitTimeoutMs: number;
  readonly #pollIntervalMs: number;

  constructor(options: BootstrapElectionOptions) {
    const runtimeDirectory = resolve(nonEmptyString(options.runtimeDirectory, "runtimeDirectory", 4_096));
    this.paths = Object.freeze({
      runtimeDirectory,
      lockPath: join(runtimeDirectory, "daemon-election.lock"),
      leasePath: join(runtimeDirectory, "daemon-election.lease.json"),
      readyPath: join(runtimeDirectory, "daemon-election.ready.json"),
      attemptsPath: join(runtimeDirectory, "daemon-election.attempts.jsonl"),
    });
    this.#lockPort = options.lockPort ?? FLOCK_ELECTION_LOCK_PORT;
    this.#clock = options.clock ?? Date.now;
    this.#sleep = options.sleep ?? (async (milliseconds) => await new Promise<void>((resolvePromise) => setTimeout(resolvePromise, milliseconds)));
    this.#leaseDurationMs = positiveInteger(options.leaseDurationMs ?? 10_000, "leaseDurationMs");
    this.#waitTimeoutMs = positiveInteger(options.waitTimeoutMs ?? 15_000, "waitTimeoutMs");
    this.#pollIntervalMs = positiveInteger(options.pollIntervalMs ?? 20, "pollIntervalMs");
  }

  async withExclusiveLock<T>(
    actionId: string,
    callback: (held: HeldBootstrapElection) => Promise<T>,
  ): Promise<BootstrapElectionResult<T>> {
    const parsedActionId = nonEmptyString(actionId, "bootstrap actionId");
    await ensurePrivateDirectory(this.paths.runtimeDirectory);
    const deadlineAt = this.#clock() + this.#waitTimeoutMs;
    for (;;) {
      const lock = await this.#lockPort.tryAcquire(this.paths.lockPath);
      if (lock === undefined) {
        const observed = await this.#waitForObservedOutcome(deadlineAt);
        if (observed !== undefined) return { role: "observer", outcome: observed };
        if (this.#clock() >= deadlineAt) {
          throw new BootstrapElectionError("BOOTSTRAP_ELECTION_TIMEOUT", "timed out waiting for the active bootstrap generation");
        }
        continue;
      }
      let lockReleased = false;
      try {
        const currentLease = await this.#readLease();
        if (currentLease?.status === "held" && currentLease.expiresAt > this.#clock()) {
          // Kernel-lock release alone is insufficient for reclaiming a live lease.
          await lock.release();
          lockReleased = true;
          for (;;) {
            const outcome = await this.readGenerationOutcome(currentLease.electionGeneration);
            if (outcome !== undefined) return { role: "observer", outcome };
            if (this.#clock() >= deadlineAt) {
              throw new BootstrapElectionError("BOOTSTRAP_ELECTION_TIMEOUT", "timed out waiting for the active bootstrap generation");
            }
            if (this.#clock() >= currentLease.expiresAt) break;
            await this.#sleep(Math.min(
              this.#pollIntervalMs,
              Math.max(1, Math.min(deadlineAt, currentLease.expiresAt) - this.#clock()),
            ));
          }
          continue;
        }
        if (currentLease?.status === "held") {
          const terminal = await this.#writeTerminal(currentLease, {
            status: "expired",
            code: "BOOTSTRAP_LEASE_EXPIRED",
            message: "bootstrap generation expired after its kernel lock was released",
          });
          await this.#appendAttempt({
            actionId: terminal.actionId,
            electionGeneration: terminal.electionGeneration,
            phase: "election-acquired",
            status: "expired",
            detail: terminal.message,
          });
        }
        let activeGeneration: GenerationContext | undefined;
        const held: HeldBootstrapElection = {
          paths: this.paths,
          beginGeneration: async () => {
            if (activeGeneration !== undefined) {
              throw new BootstrapElectionError("BOOTSTRAP_GENERATION_ACTIVE", "this election already started a generation");
            }
            const generation = Math.max(
              currentLease?.electionGeneration ?? 0,
              (await this.#readReady())?.electionGeneration ?? 0,
            ) + 1;
            activeGeneration = await this.#beginGeneration(parsedActionId, generation);
            return activeGeneration.publicApi;
          },
          readCurrentOutcome: async () => {
            const generation = Math.max(
              (await this.#readLease())?.electionGeneration ?? 0,
              (await this.#readReady())?.electionGeneration ?? 0,
            );
            return generation === 0 ? undefined : await this.readGenerationOutcome(generation);
          },
        };
        try {
          const value = await callback(held);
          if (activeGeneration !== undefined && !activeGeneration.terminal) {
            await activeGeneration.publicApi.recordTerminal({
              status: "ambiguous",
              code: "BOOTSTRAP_INCOMPLETE",
              message: "bootstrap callback returned before recording a terminal outcome",
            });
            throw new BootstrapElectionError("BOOTSTRAP_INCOMPLETE", "bootstrap generation has no terminal outcome");
          }
          return { role: "owner", value };
        } catch (error: unknown) {
          if (activeGeneration !== undefined && !activeGeneration.terminal) {
            await activeGeneration.publicApi.recordTerminal({
              status: "ambiguous",
              code: "BOOTSTRAP_CALLBACK_FAILED",
              message: error instanceof Error ? error.message : String(error),
            }).catch(() => undefined);
          }
          throw error;
        }
      } finally {
        if (!lockReleased) await lock.release();
      }
    }
  }

  async readGenerationOutcome(generation: number): Promise<BootstrapGenerationOutcome | undefined> {
    positiveInteger(generation, "election generation");
    const lease = await this.#readLease();
    if (lease?.electionGeneration === generation && lease.status !== "held" && lease.status !== "succeeded") {
      return { kind: "terminal", receipt: lease };
    }
    if (lease !== undefined && lease.electionGeneration > generation) {
      throw new BootstrapElectionError("BOOTSTRAP_GENERATION_SUPERSEDED", `bootstrap generation ${String(generation)} was superseded`);
    }
    const ready = await this.#readReady();
    if (lease?.electionGeneration === generation && lease.status === "succeeded") {
      if (ready?.electionGeneration !== generation) {
        throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "successful bootstrap lease has no matching ready receipt");
      }
      if (ready.actionId !== lease.actionId) {
        throw new BootstrapElectionError("BOOTSTRAP_RECEIPT_INVALID", "bootstrap ready receipt is not bound to its confirmed lease");
      }
      return { kind: "ready", receipt: ready };
    }
    if (ready !== undefined && ready.electionGeneration > generation) {
      throw new BootstrapElectionError("BOOTSTRAP_GENERATION_SUPERSEDED", `bootstrap generation ${String(generation)} was superseded`);
    }
    return undefined;
  }

  async inspectCurrent(): Promise<BootstrapElectionInspection> {
    return await this.inspectCurrentWith(async (inspection) => inspection);
  }

  /**
   * Inspect without creating the election lock on a never-started runtime.
   *
   * An existing lock still uses the shared inspection fence. When the lock is
   * absent there is no generation to fence, so this reads the receipt snapshot
   * without materialising private state merely to diagnose it.
   */
  async inspectCurrentReadOnlyWith<T>(
    callback: (inspection: BootstrapElectionInspection) => Promise<T>,
  ): Promise<T> {
    if (await validateExistingPrivateFile(this.paths.lockPath)) {
      return await this.inspectCurrentWith(callback);
    }
    let inspection: BootstrapElectionInspection;
    try {
      inspection = await this.#inspectCurrentReceipts();
    } catch (error: unknown) {
      if (await validateExistingPrivateFile(this.paths.lockPath)) {
        return await this.inspectCurrentWith(callback);
      }
      throw error;
    }
    const result = await callback(inspection);
    if (await validateExistingPrivateFile(this.paths.lockPath)) {
      return await this.inspectCurrentWith(callback);
    }
    return result;
  }

  async inspectCurrentWith<T>(
    callback: (inspection: BootstrapElectionInspection) => Promise<T>,
  ): Promise<T> {
    const lock = await this.#lockPort.probe(this.paths.lockPath);
    if (lock.status === "held") {
      return await callback({ status: "active", ownership: "kernel-lock" });
    }
    try {
      return await callback(await this.#inspectCurrentReceipts());
    } finally {
      if (lock.status === "acquired") await lock.handle.release();
    }
  }

  async #inspectCurrentReceipts(): Promise<BootstrapElectionInspection> {
    const [lease, ready] = await Promise.all([this.#readLease(), this.#readReady()]);
    if (lease === undefined && ready === undefined) return { status: "absent" };
    if (lease?.status === "held") {
      throw new BootstrapElectionError(
        "BOOTSTRAP_ELECTION_INCONSISTENT",
        "bootstrap lease is held without kernel election ownership",
      );
    }
    const generation = Math.max(lease?.electionGeneration ?? 0, ready?.electionGeneration ?? 0);
    if (generation === 0) return { status: "absent" };
    const outcome = await this.readGenerationOutcome(generation);
    if (outcome === undefined) {
      throw new BootstrapElectionError("BOOTSTRAP_INCOMPLETE", "bootstrap election artifacts have no current outcome");
    }
    return outcome.kind === "ready"
      ? { status: "ready", receipt: outcome.receipt }
      : { status: "terminal", receipt: outcome.receipt };
  }

  async waitForGenerationOutcome(generation: number, deadlineAt = this.#clock() + this.#waitTimeoutMs): Promise<BootstrapGenerationOutcome> {
    positiveInteger(generation, "election generation");
    positiveInteger(deadlineAt, "bootstrap deadline");
    for (;;) {
      const outcome = await this.readGenerationOutcome(generation);
      if (outcome !== undefined) return outcome;
      if (this.#clock() >= deadlineAt) {
        throw new BootstrapElectionError("BOOTSTRAP_ELECTION_TIMEOUT", `bootstrap generation ${String(generation)} did not become terminal`);
      }
      await this.#sleep(Math.min(this.#pollIntervalMs, Math.max(1, deadlineAt - this.#clock())));
    }
  }

  async #waitForObservedOutcome(deadlineAt: number): Promise<BootstrapGenerationOutcome | undefined> {
    for (;;) {
      const lease = await this.#readLease();
      if (lease !== undefined) {
        // A terminal lease may be historical while the current lock owner is
        // rechecking or shutting down. Only a generation observed as held is
        // eligible for loser polling; otherwise wait for lock turnover.
        if (lease.status === "held") {
          for (;;) {
            const outcome = await this.readGenerationOutcome(lease.electionGeneration);
            if (outcome !== undefined) return outcome;
            if (this.#clock() >= deadlineAt || this.#clock() >= lease.expiresAt) return undefined;
            await this.#sleep(Math.min(
              this.#pollIntervalMs,
              Math.max(1, Math.min(deadlineAt, lease.expiresAt) - this.#clock()),
            ));
          }
        }
      } else {
        const ready = await this.#readReady();
        if (ready !== undefined) {
          throw new BootstrapElectionError(
            "BOOTSTRAP_RECEIPT_INVALID",
            "bootstrap ready receipt has no matching confirmed generation lease",
          );
        }
      }
      if (this.#clock() >= deadlineAt) return undefined;
      await this.#sleep(Math.min(this.#pollIntervalMs, Math.max(1, deadlineAt - this.#clock())));
      // Retry lock acquisition when a winner crashed before publishing its lease.
      return undefined;
    }
  }

  async #beginGeneration(actionId: string, electionGeneration: number): Promise<GenerationContext> {
    const acquiredAt = positiveInteger(this.#clock(), "bootstrap clock");
    const lease: BootstrapHeldLease = {
      schemaVersion: 1,
      actionId,
      electionGeneration,
      status: "held",
      acquiredAt,
      expiresAt: acquiredAt + this.#leaseDurationMs,
    };
    await atomicPrivateJson(this.paths.leasePath, lease);
    await this.#appendAttempt({ actionId, electionGeneration, phase: "election-acquired", status: "progress" });
    let terminal = false;
    let readyReceipt: BootstrapReadyReceipt | undefined;
    const publicApi: BootstrapGeneration = {
      actionId,
      electionGeneration,
      appendAttempt: async (phase, status, detail) => {
        if (terminal) throw new BootstrapElectionError("BOOTSTRAP_GENERATION_TERMINAL", "bootstrap generation is already terminal");
        await this.#appendAttempt({ actionId, electionGeneration, phase, status, ...(detail === undefined ? {} : { detail }) });
      },
      publishReady: async (input) => {
        if (terminal) throw new BootstrapElectionError("BOOTSTRAP_GENERATION_TERMINAL", "bootstrap generation is already terminal");
        const receipt = parseReady({
          schemaVersion: 1,
          actionId,
          electionGeneration,
          ...input,
          readyAt: positiveInteger(this.#clock(), "bootstrap clock"),
        });
        await atomicPrivateJson(this.paths.readyPath, receipt);
        await this.#appendAttempt({ actionId, electionGeneration, phase: "ready-receipt", status: "succeeded" });
        readyReceipt = receipt;
        return receipt;
      },
      confirmReady: async () => {
        if (terminal) throw new BootstrapElectionError("BOOTSTRAP_GENERATION_TERMINAL", "bootstrap generation is already terminal");
        if (readyReceipt === undefined) throw new BootstrapElectionError("BOOTSTRAP_NOT_READY", "bootstrap generation has no ready receipt");
        await this.#writeTerminal(lease, {
          status: "succeeded",
          code: "BOOTSTRAP_READY",
          message: "daemon published a ready receipt and completed authenticated initialization",
        });
        terminal = true;
      },
      recordTerminal: async (input) => {
        if (terminal) throw new BootstrapElectionError("BOOTSTRAP_GENERATION_TERMINAL", "bootstrap generation is already terminal");
        const receipt = await this.#writeTerminal(lease, input);
        await this.#appendAttempt({
          actionId,
          electionGeneration,
          phase: input.phase ?? "handshake",
          status: input.status,
          detail: input.message,
        });
        terminal = true;
        return receipt;
      },
      waitForOutcome: async (deadlineAt) => await this.waitForGenerationOutcome(electionGeneration, deadlineAt),
    };
    return {
      get terminal() { return terminal; },
      publicApi,
    };
  }

  async #writeTerminal(
    held: BootstrapHeldLease,
    input: { status: BootstrapTerminalLease["status"]; code: string; message: string },
  ): Promise<BootstrapTerminalLease> {
    const terminal: BootstrapTerminalLease = {
      schemaVersion: 1,
      actionId: held.actionId,
      electionGeneration: held.electionGeneration,
      status: input.status,
      acquiredAt: held.acquiredAt,
      terminalAt: positiveInteger(this.#clock(), "bootstrap clock"),
      code: nonEmptyString(input.code, "bootstrap terminal code"),
      message: nonEmptyString(input.message, "bootstrap terminal message", 4_096),
    };
    await atomicPrivateJson(this.paths.leasePath, terminal);
    return terminal;
  }

  async #appendAttempt(input: Omit<BootstrapAttempt, "schemaVersion" | "recordedAt">): Promise<void> {
    const attempt: BootstrapAttempt = {
      schemaVersion: 1,
      actionId: nonEmptyString(input.actionId, "bootstrap attempt actionId"),
      electionGeneration: positiveInteger(input.electionGeneration, "bootstrap attempt generation"),
      phase: input.phase,
      status: input.status,
      recordedAt: positiveInteger(this.#clock(), "bootstrap clock"),
      ...(input.detail === undefined ? {} : { detail: nonEmptyString(input.detail, "bootstrap attempt detail", 4_096) }),
    };
    await appendPrivateJson(this.paths.attemptsPath, attempt);
  }

  async #readLease(): Promise<BootstrapLeaseReceipt | undefined> {
    const value = await readPrivateJson(this.paths.leasePath);
    return value === undefined ? undefined : parseLease(value);
  }

  async #readReady(): Promise<BootstrapReadyReceipt | undefined> {
    const value = await readPrivateJson(this.paths.readyPath);
    return value === undefined ? undefined : parseReady(value);
  }
}

type GenerationContext = {
  readonly terminal: boolean;
  readonly publicApi: BootstrapGeneration;
};
