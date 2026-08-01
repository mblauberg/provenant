import { createHash, randomBytes } from "node:crypto";
import { existsSync, lstatSync, realpathSync, statSync } from "node:fs";
import { chmod, open, readFile, rename, rm, rmdir } from "node:fs/promises";
import { basename, dirname, join, normalize, resolve } from "node:path";

import Database from "better-sqlite3";
import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

import type { FabricOpenOptions } from "../domain/types.js";
import { resolveFabricRoots } from "../domain/fabric-roots.js";
import {
  inspectFabricDatabase,
  type DatabaseInspectionHooks,
} from "../core/migrations.js";
import { FabricRemoteError } from "../transport/ndjson-rpc.js";
import { attachOrStartDaemon, type DaemonHandshakeResult } from "./bootstrap-client.js";
import { BootstrapElection, type BootstrapReadyReceipt } from "./bootstrap-election.js";
import {
  holdDaemonChildBeforeDiscovery,
  spawnDaemonChild,
  type ChildExit,
} from "./daemon-child.js";
import {
  ensurePrivateDirectory,
  markPrivateDiscoveryTerminal,
  privateDiscoveryPaths,
  publishPrivateDiscovery,
  readPrivateDiscovery,
  type PrivateDiscoveryCapabilityReceipt,
  type PrivateDiscoveryIdentity,
  type PrivateDiscoveryOwner,
  type PrivateDiscoveryPaths,
} from "./private-discovery.js";
import { FABRIC_PROTOCOL_VERSION, isRecord } from "./protocol.js";
import { preflightProtocolBuild } from "./protocol-build-preflight.js";
import { FabricDaemonClient } from "./rpc-client.js";
import { composeDaemonConfiguration } from "./composition.js";
import type { HerdrDaemonProcessConfiguration } from "./herdr-composition.js";
import type { OptionalGitHubHostedChecksConfiguration } from "../operator/github-hosted-checks.js";
import type { TrustedGitConfiguration } from "../operator/trusted-git-registry.js";
import { verifyReviewProfileCatalogueDigest } from "../review/profile/catalogue-digest.js";

export { FabricRemoteError } from "../transport/ndjson-rpc.js";
export { connectFabricDaemon, FabricDaemonClient } from "./rpc-client.js";

export type DaemonStartOptions = {
  databasePath: string;
  stateDirectory: string;
  runtimeDirectory: string;
  socketPath: string;
  lifecycleReceiptAuthorityId?: string;
  adapters?: NonNullable<FabricOpenOptions["adapters"]>;
  executionProfile?: string;
  maximumConcurrentProviderTurns?: number;
  workspaceRoots?: string[];
  githubHostedChecks?: OptionalGitHubHostedChecksConfiguration;
  trustedGitConfiguration?: TrustedGitConfiguration;
  herdr?: HerdrDaemonProcessConfiguration;
  /** Parent-process test seam; never crosses into the daemon child. */
  inspectionHooks?: DatabaseInspectionHooks;
  configuration?: {
    globalConfigPath: string;
    localConfigPath?: string;
    projectConfigPath?: string;
    runConfigPath?: string;
    compatibilityPath: string;
    compatibilitySchemaPath: string;
    agentsHome: string;
  };
};

export type FabricDaemonHandle = {
  bootstrapCapability: string;
  address: { transport: "unix"; path: string };
  pid: number;
  ownsProcess: boolean;
  /** Releases only this caller's process handles. It never stops the daemon. */
  release(): void;
  stop(): Promise<void>;
  waitForExit(): Promise<void>;
};

function canonicalDaemonPath(path: string): string {
  let cursor = resolve(path);
  const suffix: string[] = [];
  while (!existsSync(cursor)) {
    const parent = dirname(cursor);
    if (parent === cursor) throw new TypeError(`workspace root has no resolvable ancestor: ${path}`);
    suffix.unshift(basename(cursor));
    cursor = parent;
  }
  return normalize(resolve(realpathSync(cursor), ...suffix));
}

function safeDatabasePath(path: string): string {
  const requested = resolve(path);
  if (existsSync(requested)) {
    const link = lstatSync(requested);
    if (link.isSymbolicLink()) throw new FabricRemoteError("DAEMON_DATABASE_PATH_UNSAFE", "daemon database path must not be a symbolic link");
    const info = statSync(requested);
    if (!info.isFile() || info.nlink !== 1) throw new FabricRemoteError("DAEMON_DATABASE_PATH_UNSAFE", "daemon database path must be a single-link regular file");
  } else {
    // A dangling final-component symlink is not reported by existsSync.
    try {
      if (lstatSync(requested).isSymbolicLink()) {
        throw new FabricRemoteError("DAEMON_DATABASE_PATH_UNSAFE", "daemon database path must not be a symbolic link");
      }
    } catch (error: unknown) {
      if (error instanceof FabricRemoteError) throw error;
      if (!isRecord(error) || error.code !== "ENOENT") throw error;
    }
  }
  return canonicalDaemonPath(requested);
}

export type DaemonLock =
  | { kind: "production-election-proof"; database: null; path: string; token: string }
  | { kind: "test-forced-process-lock"; database: Database.Database; path: string; token: string };

function productionElectionProof(): { actionId: string; electionGeneration: number; daemonInstanceGeneration: number } | undefined {
  if (process.env.AGENT_FABRIC_BOOTSTRAP_MODE !== "production-election") return undefined;
  const actionId = process.env.AGENT_FABRIC_BOOTSTRAP_ACTION_ID;
  const electionGeneration = Number(process.env.AGENT_FABRIC_BOOTSTRAP_ELECTION_GENERATION);
  const daemonInstanceGeneration = Number(process.env.AGENT_FABRIC_DAEMON_INSTANCE_GENERATION);
  if (
    actionId === undefined
    || actionId.trim().length === 0
    || !Number.isSafeInteger(electionGeneration)
    || electionGeneration < 1
    || !Number.isSafeInteger(daemonInstanceGeneration)
    || daemonInstanceGeneration < 1
  ) {
    throw new FabricRemoteError("DAEMON_ELECTION_PROOF_INVALID", "production daemon election proof is incomplete");
  }
  return { actionId, electionGeneration, daemonInstanceGeneration };
}

export async function writeDaemonLockReceipt(path: string, owner: { pid: number; token: string; socketPath?: string }): Promise<void> {
  if (productionElectionProof() !== undefined) return;
  const temporaryPath = `${path}.tmp-${process.pid}-${randomBytes(12).toString("hex")}`;
  const handle = await open(temporaryPath, "wx", 0o600);
  try {
    await handle.writeFile(`${JSON.stringify(owner)}\n`, "utf8");
    await handle.sync();
  } finally {
    await handle.close();
  }
  try {
    await rename(temporaryPath, path);
    await chmod(path, 0o600);
  } finally {
    await rm(temporaryPath, { force: true });
  }
}

async function acquireDaemonLock(path: string): Promise<DaemonLock> {
  const token = randomBytes(24).toString("base64url");
  const lockDatabasePath = `${path}.sqlite3`;
  let database: Database.Database | undefined;
  try {
    database = new Database(lockDatabasePath, { timeout: 0 });
    await chmod(lockDatabasePath, 0o600);
    database.pragma("journal_mode = DELETE");
    database.exec("CREATE TABLE IF NOT EXISTS daemon_lock_owner(singleton INTEGER PRIMARY KEY CHECK(singleton = 1), pid INTEGER NOT NULL, token TEXT NOT NULL)");
    database.exec("BEGIN EXCLUSIVE");
    database.prepare("INSERT OR REPLACE INTO daemon_lock_owner(singleton, pid, token) VALUES (1, ?, ?)").run(process.pid, token);
    await writeDaemonLockReceipt(path, { pid: process.pid, token });
    return { kind: "test-forced-process-lock", database, path, token };
  } catch (error: unknown) {
    if (database !== undefined) {
      try { database.exec("ROLLBACK"); } catch { /* no active transaction */ }
      database.close();
    }
    if (isRecord(error) && (error.code === "SQLITE_BUSY" || error.code === "SQLITE_LOCKED")) {
      throw new FabricRemoteError("DAEMON_ALREADY_RUNNING", "agent fabric daemon already owns the coordination lock");
    }
    throw error;
  }
}

export async function acquireDaemonLocks(paths: string[]): Promise<DaemonLock[]> {
  const proof = productionElectionProof();
  if (proof !== undefined) {
    return [...new Set(paths)].sort().map((path) => ({
      kind: "production-election-proof" as const,
      database: null,
      path,
      token: `election-${proof.electionGeneration}-${proof.daemonInstanceGeneration}`,
    }));
  }
  const locks: DaemonLock[] = [];
  try {
    for (const path of [...new Set(paths)].sort()) locks.push(await acquireDaemonLock(path));
    return locks;
  } catch (error: unknown) {
    await Promise.allSettled(locks.reverse().map(releaseDaemonLock));
    throw error;
  }
}

async function releaseDaemonLock(lock: DaemonLock): Promise<void> {
  if (lock.kind === "production-election-proof") return;
  let record: unknown;
  try {
    record = JSON.parse(await readFile(lock.path, "utf8"));
  } catch {
    record = undefined;
  }
  if (isRecord(record) && record.token === lock.token) {
    await rm(lock.path, { force: true });
  }
  try { lock.database.exec("COMMIT"); } catch { /* process teardown */ }
  lock.database.close();
}

export async function releaseDaemonLocks(locks: DaemonLock[]): Promise<void> {
  await Promise.all(locks.map(releaseDaemonLock));
}

type PreparedDaemonStart = DaemonStartOptions & {
  productRoot: string;
  adapters: NonNullable<FabricOpenOptions["adapters"]>;
  executionProfile: string;
  maximumConcurrentProviderTurns: number;
  workspaceRoots: string[];
};

type ProductionSpawn = {
  bootstrapCapability: string;
  pid: number;
  identity: Promise<PrivateDiscoveryIdentity>;
  exit: Promise<void>;
  release(): void;
  stop(clean: boolean): Promise<void>;
};

type PrivateDaemonAttachment = {
  receipt: PrivateDiscoveryCapabilityReceipt;
  owner: PrivateDiscoveryOwner;
};

export async function preflightFabricDaemonStart(
  _options: Pick<DaemonStartOptions, "configuration"> = {},
): Promise<void> {
  // The pin is compiled from this package tree, so verify the verifier's
  // code-adjacent tree too. `agentsHome` may be a fixture, deployment home, or
  // another worktree and is not the authority for this source artefact.
  await verifyReviewProfileCatalogueDigest();
  await preflightProtocolBuild();
}

function normalizedStartOptions(options: DaemonStartOptions): DaemonStartOptions {
  return {
    ...options,
    databasePath: resolve(options.databasePath),
    stateDirectory: resolve(options.stateDirectory),
    runtimeDirectory: resolve(options.runtimeDirectory),
    socketPath: resolve(options.socketPath),
  };
}

async function prepareDaemonStart(options: DaemonStartOptions): Promise<PreparedDaemonStart> {
  if (
    options.lifecycleReceiptAuthorityId !== undefined &&
    options.lifecycleReceiptAuthorityId.trim().length === 0
  ) {
    throw new TypeError("lifecycle receipt authority ID must not be empty");
  }
  const databasePath = safeDatabasePath(options.databasePath);
  const productRoot = resolveFabricRoots({
    agentsHomeFlag: options.configuration?.agentsHome,
  }).productRoot;
  let adapters = options.adapters ?? {};
  let executionProfile = options.executionProfile ?? "headless";
  let maximumConcurrentProviderTurns = options.maximumConcurrentProviderTurns ?? 8;
  let workspaceRoots = options.workspaceRoots ?? [process.cwd()];
  if (options.configuration !== undefined) {
    if (options.adapters !== undefined) {
      throw new TypeError("daemon accepts explicit adapters or trusted configuration, not both");
    }
    const composition = await composeDaemonConfiguration({ ...options.configuration, stateDirectory: options.stateDirectory });
    adapters = composition.adapters;
    executionProfile = composition.executionProfile;
    maximumConcurrentProviderTurns = composition.maximumConcurrentProviderTurns;
    workspaceRoots = composition.workspaceRoots;
  }
  workspaceRoots = [...new Set(workspaceRoots.map(canonicalDaemonPath))].sort();
  return {
    ...options,
    databasePath,
    productRoot,
    adapters,
    executionProfile,
    maximumConcurrentProviderTurns,
    workspaceRoots,
  };
}

function stableBootstrapActionId(options: Pick<DaemonStartOptions, "databasePath" | "socketPath">): string {
  return `daemon-bootstrap-${createHash("sha256")
    .update(JSON.stringify({ databasePath: options.databasePath, socketPath: options.socketPath }))
    .digest("hex")}`;
}

function identityMatchesOwner(identity: PrivateDiscoveryIdentity, owner: PrivateDiscoveryOwner): boolean {
  return identity.actionId === owner.actionId
    && identity.electionGeneration === owner.electionGeneration
    && identity.daemonInstanceGeneration === owner.daemonInstanceGeneration
    && identity.socketPath === owner.socketPath
    && identity.pid === owner.pid
    && identity.bootstrapCapabilityHash === owner.bootstrapCapabilityHash;
}

function readyMatchesOwner(receipt: BootstrapReadyReceipt, owner: PrivateDiscoveryOwner): boolean {
  return receipt.actionId === owner.actionId
    && receipt.electionGeneration === owner.electionGeneration
    && receipt.daemonInstanceGeneration === owner.daemonInstanceGeneration
    && receipt.socketPath === owner.socketPath;
}

function socketEntryExists(socketPath: string): boolean {
  try {
    lstatSync(socketPath);
    return true;
  } catch (error: unknown) {
    if (isRecord(error) && error.code === "ENOENT") return false;
    throw error;
  }
}

function processIsRunning(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error: unknown) {
    if (isRecord(error) && error.code === "ESRCH") return false;
    return true;
  }
}

async function reconcileUnreachablePrivateDaemon(
  paths: PrivateDiscoveryPaths,
  socketPath: string,
): Promise<boolean> {
  const discovery = await readPrivateDiscovery(paths, socketPath);
  if (discovery.status !== "active" || processIsRunning(discovery.owner.pid)) {
    return false;
  }
  await markPrivateDiscoveryTerminal({
    paths,
    expected: discovery.owner,
    state: "crashed",
    exitCode: null,
    signal: null,
  });
  return true;
}

async function privateDaemonHandshake(
  paths: PrivateDiscoveryPaths,
  election: BootstrapElection,
  socketPath: string,
  provisional: PrivateDiscoveryIdentity | undefined,
  expectedLifecycleReceiptAuthorityId: string | undefined,
): Promise<DaemonHandshakeResult<PrivateDaemonAttachment>> {
  const discovery = await readPrivateDiscovery(paths, socketPath);
  if (discovery.status === "absent") {
    if (socketEntryExists(socketPath)) {
      return {
        status: "unavailable",
        reason: "unreachable",
        message: "trusted socket exists without generation-bound private discovery",
        reconciliationRequired: true,
      };
    }
    return { status: "unavailable", reason: "absent", message: "private daemon discovery is absent" };
  }
  if (discovery.status === "terminal") {
    return {
      status: "unavailable",
      reason: "stale",
      message: `daemon generation ${String(discovery.owner.daemonInstanceGeneration)} is proved ${discovery.owner.state}`,
      terminalEvidence: {
        state: discovery.owner.state === "stopped" ? "stopped" : "crashed",
        actionId: discovery.owner.actionId,
        electionGeneration: discovery.owner.electionGeneration,
        daemonInstanceGeneration: discovery.owner.daemonInstanceGeneration,
        socketPath: discovery.owner.socketPath,
      },
    };
  }
  if (discovery.status === "ambiguous") {
    return {
      status: "unavailable",
      reason: "unreachable",
      message: discovery.message,
      reconciliationRequired: true,
    };
  }
  if (
    discovery.receipt.lifecycleReceiptAuthorityId !==
      (expectedLifecycleReceiptAuthorityId ?? null)
  ) {
    return {
      status: "incompatible",
      responsive: true,
      message: "daemon lifecycle receipt authority configuration mismatch",
    };
  }

  let client: FabricDaemonClient;
  try {
    client = await FabricDaemonClient.connect(discovery.receipt.socketPath, discovery.receipt.bootstrapCapability);
  } catch (error: unknown) {
    return {
      status: "unavailable",
      reason: error instanceof FabricRemoteError && error.code === "DAEMON_CONNECT_TIMEOUT" ? "timeout" : "unreachable",
      message: error instanceof Error ? error.message : String(error),
      reconciliationRequired: true,
    };
  }
  const initialized = client.initializeResult;
  await client.close();
  if (initialized.protocolVersion !== FABRIC_PROTOCOL_VERSION || !initialized.capabilities.includes("rpc")) {
    return { status: "incompatible", responsive: true, message: "daemon private protocol is incompatible" };
  }
  if (
    discovery.receipt.protocolVersion !== undefined &&
    (
      discovery.receipt.protocolVersion !== initialized.protocolVersion ||
      discovery.receipt.features === undefined ||
      discovery.receipt.features.length !== initialized.capabilities.length ||
      discovery.receipt.features.some((feature) => !initialized.capabilities.includes(feature))
    )
  ) {
    return {
      status: "incompatible",
      responsive: true,
      message: "daemon private protocol does not match its discovery receipt",
    };
  }

  const provisionallyOwned = provisional !== undefined
    && identityMatchesOwner(provisional, discovery.owner);
  if (!provisionallyOwned) {
    let outcome;
    try {
      outcome = await election.readGenerationOutcome(discovery.owner.electionGeneration);
    } catch (error: unknown) {
      if (isRecord(error) && error.code === "BOOTSTRAP_GENERATION_SUPERSEDED") {
        return {
          status: "unavailable",
          reason: "stale",
          message: "daemon discovery generation was superseded",
          reconciliationRequired: true,
        };
      }
      throw error;
    }
    if (outcome?.kind !== "ready") {
      return {
        status: "unavailable",
        reason: "unreachable",
        message: "responsive daemon discovery has no confirmed ready generation",
        reconciliationRequired: true,
      };
    }
    if (!readyMatchesOwner(outcome.receipt, discovery.owner)) {
      throw new FabricRemoteError(
        "DAEMON_DISCOVERY_GENERATION_MISMATCH",
        "daemon discovery owner does not match the authoritative ready receipt",
      );
    }
  }
  return {
    status: "compatible",
    client: { receipt: discovery.receipt, owner: discovery.owner },
    protocolVersion: initialized.protocolVersion,
    daemonInstanceGeneration: discovery.owner.daemonInstanceGeneration,
    features: initialized.capabilities,
  };
}

async function markSpawnTerminal(
  election: BootstrapElection,
  paths: PrivateDiscoveryPaths,
  identity: PrivateDiscoveryIdentity,
  state: "stopped" | "crashed",
  exit: ChildExit,
): Promise<void> {
  const result = await election.withExclusiveLock(`terminal-${identity.actionId}`, async () => {
    await markPrivateDiscoveryTerminal({
      paths,
      expected: identity,
      state,
      exitCode: exit.code,
      signal: exit.signal,
    });
  });
  if (result.role === "observer") {
    const discovery = await readPrivateDiscovery(paths, identity.socketPath);
    if (discovery.status !== "terminal" || !identityMatchesOwner(identity, discovery.owner)) {
      throw new FabricRemoteError(
        "DAEMON_DISCOVERY_TERMINAL_UNCONFIRMED",
        "daemon exit could not confirm its generation-bound discovery transition",
      );
    }
  }
}

async function spawnProductionDaemon(input: {
  options: DaemonStartOptions;
  actionId: string;
  electionGeneration: number;
  paths: PrivateDiscoveryPaths;
  election: BootstrapElection;
}): Promise<ProductionSpawn> {
  const prepared = await prepareDaemonStart(input.options);
  const daemonInstanceGeneration = input.electionGeneration;
  const childOptions = { ...prepared };
  delete childOptions.inspectionHooks;
  const child = await spawnDaemonChild(childOptions, {
    mode: "production-election",
    actionId: input.actionId,
    electionGeneration: input.electionGeneration,
    daemonInstanceGeneration,
  });
  let cleanStopRequested = false;
  const identity = (async (): Promise<PrivateDiscoveryIdentity> => {
    await child.ready;
    await holdDaemonChildBeforeDiscovery(child.pid);
    return await publishPrivateDiscovery({
      paths: input.paths,
      actionId: input.actionId,
      electionGeneration: input.electionGeneration,
      daemonInstanceGeneration,
      socketPath: prepared.socketPath,
      pid: child.pid,
      bootstrapCapability: child.bootstrapCapability,
      lifecycleReceiptAuthorityId: prepared.lifecycleReceiptAuthorityId ?? null,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      features: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
    });
  })();
  const publishedIdentity = identity.then(
    (value) => value,
    () => undefined,
  );
  const exit = Promise.all([child.exit, publishedIdentity]).then(async ([childExit, owner]) => {
    if (owner === undefined) return;
    await markSpawnTerminal(
      input.election,
      input.paths,
      owner,
      cleanStopRequested ? "stopped" : "crashed",
      childExit,
    );
  });
  void exit.catch(() => undefined);
  let stopPromise: Promise<void> | undefined;
  return {
    bootstrapCapability: child.bootstrapCapability,
    pid: child.pid,
    identity,
    exit,
    release(): void {
      child.release();
    },
    stop(clean: boolean): Promise<void> {
      if (clean && child.isRunning()) cleanStopRequested = true;
      stopPromise ??= child.terminate().then(async () => await exit);
      return stopPromise;
    },
  };
}

function ownerHandle(spawned: ProductionSpawn, socketPath: string): FabricDaemonHandle {
  let stopPromise: Promise<void> | undefined;
  return {
    bootstrapCapability: spawned.bootstrapCapability,
    address: { transport: "unix", path: socketPath },
    pid: spawned.pid,
    ownsProcess: true,
    release(): void {
      spawned.release();
    },
    stop(): Promise<void> {
      stopPromise ??= spawned.stop(true);
      return stopPromise;
    },
    waitForExit(): Promise<void> {
      return spawned.exit;
    },
  };
}

function attachedHandle(
  attachment: PrivateDaemonAttachment,
  paths: PrivateDiscoveryPaths,
): FabricDaemonHandle {
  let detach: (() => void) | undefined;
  let released = false;
  const detached = new Promise<void>((resolvePromise) => { detach = resolvePromise; });
  const incumbentExit = (async (): Promise<void> => {
    while (!released) {
      const current = await readPrivateDiscovery(paths, attachment.owner.socketPath);
      if (current.status !== "active" || !identityMatchesOwner(attachment.owner, current.owner)) return;
      await new Promise<void>((resolvePromise) => setTimeout(resolvePromise, 50));
    }
  })();
  const localExit = Promise.race([incumbentExit, detached]);
  void localExit.catch(() => undefined);
  return {
    bootstrapCapability: attachment.receipt.bootstrapCapability,
    address: { transport: "unix", path: attachment.receipt.socketPath },
    pid: attachment.receipt.pid,
    ownsProcess: false,
    release(): void {
      released = true;
      detach?.();
    },
    async stop(): Promise<void> {
      released = true;
      detach?.();
    },
    waitForExit(): Promise<void> {
      return localExit;
    },
  };
}

export async function startFabricDaemon(options: DaemonStartOptions): Promise<FabricDaemonHandle> {
  const normalized = normalizedStartOptions(options);
  normalized.databasePath = safeDatabasePath(normalized.databasePath);
  const stateDirectoryWasAbsent = !existsSync(normalized.stateDirectory);
  const runtimeDirectoryWasAbsent = !existsSync(normalized.runtimeDirectory);
  const paths = privateDiscoveryPaths(normalized.runtimeDirectory);
  const election = new BootstrapElection({ runtimeDirectory: normalized.runtimeDirectory });
  const bootstrapArtifactPaths = [
    join(normalized.stateDirectory, "capability.key"),
    election.paths.lockPath,
    election.paths.leasePath,
    election.paths.readyPath,
    election.paths.attemptsPath,
    paths.receiptPath,
    paths.ownerPath,
  ];
  const absentBootstrapArtifacts = bootstrapArtifactPaths.filter((path) => !existsSync(path));
  const actionId = stableBootstrapActionId(normalized);
  let bootstrapDirectoriesPrepared = false;
  let provisional: PrivateDiscoveryIdentity | undefined;
  let spawned: ProductionSpawn | undefined;
  let attached;
  try {
    attached = await attachOrStartDaemon<PrivateDaemonAttachment>({
      actionId,
      socketPath: normalized.socketPath,
      requiredProtocolVersion: FABRIC_PROTOCOL_VERSION,
      requiredFeatures: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
      election,
      preflight: async () => {
        // This runs only when no compatible incumbent is attached: the
        // catalogue is a daemon-start invariant, not a per-client handshake.
        await preflightFabricDaemonStart(normalized);
      },
      handshake: async () => {
        if (!bootstrapDirectoriesPrepared && !existsSync(normalized.runtimeDirectory)) {
          return {
            status: "unavailable" as const,
            reason: "absent" as const,
            message: "private daemon discovery is absent",
          };
        }
        return await privateDaemonHandshake(
          paths,
          election,
          normalized.socketPath,
          provisional,
          normalized.lifecycleReceiptAuthorityId,
        );
      },
      preBootstrap: async () => {
        inspectFabricDatabase(normalized.databasePath, normalized.inspectionHooks);
        await Promise.all([
          ensurePrivateDirectory(normalized.stateDirectory),
          ensurePrivateDirectory(normalized.runtimeDirectory),
        ]);
        bootstrapDirectoriesPrepared = true;
      },
      reconcile: async (unavailable) => {
        if (!await reconcileUnreachablePrivateDaemon(paths, normalized.socketPath)) {
          return unavailable;
        }
        return await privateDaemonHandshake(
          paths,
          election,
          normalized.socketPath,
          provisional,
          normalized.lifecycleReceiptAuthorityId,
        );
      },
      spawn: async (bootstrap) => {
        spawned = await spawnProductionDaemon({
          options: normalized,
          actionId: bootstrap.actionId,
          electionGeneration: bootstrap.electionGeneration,
          paths,
          election,
        });
        return {
          ready: (async () => {
            provisional = await spawned.identity;
            return {
              daemonInstanceGeneration: provisional.daemonInstanceGeneration,
              socketPath: provisional.socketPath,
              protocolVersion: FABRIC_PROTOCOL_VERSION,
              features: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
              evidence: {
                databaseOwned: true,
                migrationsComplete: true,
                recoveryComplete: true,
                socketBound: true,
              },
            };
          })(),
        };
      },
    });
  } catch (error: unknown) {
    await spawned?.stop(false).catch(() => undefined);
    if (
      typeof error === "object" &&
      error !== null &&
      "code" in error &&
      error.code === "SCHEMA_CUTOVER_REQUIRED" &&
      "preserved" in error &&
      error.preserved === true
    ) {
      await Promise.allSettled(absentBootstrapArtifacts.map(async (path) => await rm(path, { force: true })));
      if (runtimeDirectoryWasAbsent) await rmdir(normalized.runtimeDirectory).catch(() => undefined);
      if (stateDirectoryWasAbsent) await rmdir(normalized.stateDirectory).catch(() => undefined);
    }
    throw error;
  }
  if (attached.started) {
    if (spawned === undefined) {
      throw new FabricRemoteError("DAEMON_BOOTSTRAP_INVALID", "bootstrap reported a started daemon without an owned child");
    }
    return ownerHandle(spawned, normalized.socketPath);
  }
  return attachedHandle(attached.client, paths);
}

/**
 * Test-only raw process launcher for process-lock cleanup and alias regression
 * coverage. Production callers must use startFabricDaemon's flock election.
 */
export async function forceStartFabricDaemonForTests(options: DaemonStartOptions): Promise<FabricDaemonHandle> {
  const normalized = normalizedStartOptions(options);
  normalized.databasePath = safeDatabasePath(normalized.databasePath);
  await Promise.all([
    ensurePrivateDirectory(normalized.stateDirectory),
    ensurePrivateDirectory(normalized.runtimeDirectory),
  ]);
  const prepared = await prepareDaemonStart(normalized);
  const child = await spawnDaemonChild(prepared, {
    mode: "test-forced-process-locks",
    actionId: `test-forced-${randomBytes(16).toString("hex")}`,
    electionGeneration: 1,
    daemonInstanceGeneration: 1,
  });
  try {
    await child.ready;
  } catch (error: unknown) {
    await child.terminate().catch(() => undefined);
    throw error;
  }
  let stopPromise: Promise<void> | undefined;
  return {
    bootstrapCapability: child.bootstrapCapability,
    address: { transport: "unix", path: prepared.socketPath },
    pid: child.pid,
    ownsProcess: true,
    release(): void {
      child.release();
    },
    stop(): Promise<void> {
      stopPromise ??= child.terminate();
      return stopPromise;
    },
    async waitForExit(): Promise<void> {
      await child.exit;
    },
  };
}
