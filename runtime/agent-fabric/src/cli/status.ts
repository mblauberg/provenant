import Database from "better-sqlite3";
import { lstat, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { verifyAdapterCompatibility } from "../adapters/compatibility.js";
import { verifyProviderConformance } from "../adapters/provider-conformance.js";
import { loadAdapterModelConstraints } from "../adapters/model-selection.js";
import { loadFabricConfig } from "../config/index.js";
import { assertDatabaseIntegrity } from "../persistence/invariants.js";
import { BootstrapElection, FLOCK_ELECTION_LOCK_PORT } from "../daemon/bootstrap-election.js";
import { connectFabricDaemon } from "../daemon/client.js";
import { privateDiscoveryPaths, readPrivateDiscovery } from "../daemon/private-discovery.js";
import { readDiscoveryReceipt } from "./mcp-provision.js";
import type { FabricPaths } from "./paths.js";
import { MCP_SEATS, resolveSeatPaths, type SeatMetadata } from "./seat-store.js";
import { trustedWorkspaceRoots } from "./workspace-trust.js";

type Check = { id: string; status: "pass" | "idle" | "fail"; code: string; detail: string };

type DoctorDaemonState =
  | { status: "live"; code: "DAEMON_LIVE"; detail: string; pid: number; socketPath: string }
  | { status: "idle"; code: "DAEMON_ON_DEMAND_IDLE"; detail: string; pid: null; socketPath: null }
  | { status: "failed"; code: string; detail: string; pid: number | null; socketPath: string | null };

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  const value = index === -1 ? undefined : arguments_[index + 1];
  if (index !== -1 && (value === undefined || value.startsWith("--"))) throw new Error(`${name} requires a value`);
  return value;
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function errorCode(error: unknown, fallback: string): string {
  return error instanceof Error && "code" in error && typeof error.code === "string"
    ? error.code
    : fallback;
}

function checkCode(id: string, outcome: "OK" | "FAILED"): string {
  return `${id.replaceAll("-", "_").toUpperCase()}_${outcome}`;
}

function generationIdentityMatches(
  owner: { actionId: string; electionGeneration: number; daemonInstanceGeneration: number; socketPath: string },
  ready: { actionId: string; electionGeneration: number; daemonInstanceGeneration: number; socketPath: string },
): boolean {
  return ready.actionId === owner.actionId
    && ready.electionGeneration === owner.electionGeneration
    && ready.daemonInstanceGeneration === owner.daemonInstanceGeneration
    && ready.socketPath === owner.socketPath;
}

function agentsHome(arguments_: string[]): string {
  return resolve(option(arguments_, "--agents-home") ?? process.env.AGENTS_HOME ?? process.cwd());
}

function pathsFor(arguments_: string[]): { agentsHome: string; config: string; compatibility: string; compatibilitySchema: string } {
  const home = agentsHome(arguments_);
  return {
    agentsHome: home,
    config: resolve(option(arguments_, "--trusted-config") ?? join(home, "config", "agent-fabric.yaml")),
    compatibility: resolve(option(arguments_, "--compatibility") ?? join(home, "config", "adapter-compatibility.yaml")),
    compatibilitySchema: resolve(option(arguments_, "--compatibility-schema") ?? join(home, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json")),
  };
}

async function daemonState(paths: FabricPaths): Promise<{ reachable: boolean; pid: number | null; socketPath: string; protocolVersion: 1; activeAdapters: string[] }> {
  try {
    const discovery = await readDiscoveryReceipt(paths);
    process.kill(discovery.pid, 0);
    const client = await connectFabricDaemon({ socketPath: discovery.socketPath, capability: discovery.bootstrapCapability });
    const activeAdapters = client.initializeResult.activeAdapters;
    await client.close();
    return { reachable: true, pid: discovery.pid, socketPath: discovery.socketPath, protocolVersion: 1, activeAdapters };
  } catch {
    return { reachable: false, pid: null, socketPath: paths.socketPath, protocolVersion: 1, activeAdapters: [] };
  }
}

async function seatStatus(paths: FabricPaths, project: string): Promise<Array<Record<string, unknown>>> {
  const seats: Array<Record<string, unknown>> = [];
  for (const seat of MCP_SEATS) {
    try {
      const location = await resolveSeatPaths({ stateDirectory: paths.stateDirectory, project, seat });
      const metadata: unknown = JSON.parse(await readFile(location.metadataPath, "utf8"));
      if (typeof metadata !== "object" || metadata === null) throw new Error("metadata is invalid");
      const value = metadata as SeatMetadata;
      seats.push({ seat, agentId: value.agentId, role: value.role, runId: value.runId, expiresAt: value.expiresAt, active: Date.parse(value.expiresAt) > Date.now() });
    } catch {
      seats.push({ seat, active: false, registered: false });
    }
  }
  return seats;
}

export async function fabricStatus(arguments_: string[], paths: FabricPaths): Promise<Record<string, unknown>> {
  const selected = pathsFor(arguments_);
  const config = await loadFabricConfig({ globalPath: selected.config, agentsHome: selected.agentsHome });
  const roots = [...new Set([...config.workspaceRoots, ...await trustedWorkspaceRoots({ stateDirectory: paths.stateDirectory, executionProfile: config.executionProfile ?? "headless" })])].sort();
  const project = resolve(option(arguments_, "--project") ?? process.cwd());
  const daemon = await daemonState(paths);
  return {
    schemaVersion: 1,
    daemon,
    executionProfile: config.executionProfile ?? "headless",
    configuredAdapters: config.adapterIds,
    activeAdapters: daemon.activeAdapters,
    trustedWorkspaceRoots: roots,
    project: { path: project, seats: await seatStatus(paths, project) },
  };
}

async function check(id: string, operation: () => string | undefined | Promise<string | undefined>): Promise<Check> {
  try {
    const detail = await operation();
    return { id, status: "pass", code: checkCode(id, "OK"), detail: detail === undefined || detail.length === 0 ? "ok" : detail };
  } catch (error: unknown) {
    return { id, status: "fail", code: errorCode(error, checkCode(id, "FAILED")), detail: errorDetail(error) };
  }
}

async function socketIsAbsent(socketPath: string): Promise<boolean> {
  try {
    await lstat(socketPath);
    return false;
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") return true;
    throw error;
  }
}

async function doctorDaemonState(paths: FabricPaths): Promise<DoctorDaemonState> {
  try {
    return await new BootstrapElection({ runtimeDirectory: paths.runtimeDirectory }).inspectCurrentWith(async (election) => {
      if (election.status === "active") {
        return {
          status: "failed" as const,
          code: "BOOTSTRAP_IN_PROGRESS",
          detail: "bootstrap election is active",
          pid: null,
          socketPath: null,
        };
      }
      const shutdown = await FLOCK_ELECTION_LOCK_PORT.probe(join(paths.runtimeDirectory, "daemon-shutdown.lock"));
      if (shutdown.status === "held") {
        return {
          status: "failed" as const,
          code: "DAEMON_SHUTDOWN_IN_PROGRESS",
          detail: "daemon shutdown transition is active",
          pid: null,
          socketPath: null,
        };
      }
      try {
      const discovery = await readPrivateDiscovery(privateDiscoveryPaths(paths.runtimeDirectory), paths.socketPath);
    if (discovery.status === "absent" || discovery.status === "terminal") {
      if (discovery.status === "terminal" && discovery.owner.state === "crashed") {
        return {
          status: "failed",
          code: "DAEMON_PROCESS_CRASHED",
          detail: `daemon generation crashed (exit=${String(discovery.owner.exitCode)} signal=${String(discovery.owner.signal)})`,
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      if (discovery.status === "terminal" && discovery.owner.state !== "stopped") {
        return {
          status: "failed",
          code: "DAEMON_DISCOVERY_INVALID",
          detail: `terminal daemon discovery state ${String(discovery.owner.state)} is not a clean stop`,
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      if (
        discovery.status === "terminal"
        && (discovery.owner.exitCode !== 0 || discovery.owner.signal !== null)
      ) {
        return {
          status: "failed",
          code: "DAEMON_PROCESS_UNCLEAN_STOP",
          detail: `daemon stopped uncleanly (exit=${String(discovery.owner.exitCode)} signal=${String(discovery.owner.signal)})`,
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      if (discovery.status === "absent" && election.status !== "absent") {
        if (election.status === "terminal") {
          return {
            status: "failed",
            code: election.receipt.code,
            detail: election.receipt.message,
            pid: null,
            socketPath: null,
          };
        }
        return {
          status: "failed",
          code: "DAEMON_DISCOVERY_MISSING",
          detail: "bootstrap completed but no generation-bound daemon discovery is available",
          pid: null,
          socketPath: null,
        };
      }
      if (!await socketIsAbsent(paths.socketPath)) {
        return {
          status: "failed",
          code: "DAEMON_SOCKET_STALE",
          detail: "daemon socket exists without an active generation-bound owner",
          pid: discovery.status === "terminal" ? discovery.owner.pid : null,
          socketPath: paths.socketPath,
        };
      }
      if (
        discovery.status === "terminal"
        && (election.status !== "ready" || !generationIdentityMatches(discovery.owner, election.receipt))
      ) {
        return {
          status: "failed",
          code: "DAEMON_ELECTION_INCONSISTENT",
          detail: "terminal daemon discovery has no matching successful bootstrap election",
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      return {
        status: "idle",
        code: "DAEMON_ON_DEMAND_IDLE",
        detail: discovery.status === "terminal" ? "on-demand daemon stopped cleanly" : "on-demand daemon has not been started",
        pid: null,
        socketPath: null,
      };
    }
    if (discovery.status === "ambiguous") {
      return {
        status: "failed",
        code: "DAEMON_DISCOVERY_AMBIGUOUS",
        detail: discovery.message,
        pid: discovery.owner?.pid ?? discovery.receipt?.pid ?? null,
        socketPath: paths.socketPath,
      };
    }
    if (
      election.status !== "ready" ||
      !generationIdentityMatches(discovery.owner, election.receipt)
    ) {
      return {
        status: "failed",
        code: "DAEMON_ELECTION_INCONSISTENT",
        detail: "active daemon discovery does not match the successful bootstrap election",
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    try {
      process.kill(discovery.receipt.pid, 0);
    } catch (error: unknown) {
      return {
        status: "failed",
        code: "DAEMON_PROCESS_UNAVAILABLE",
        detail: errorDetail(error),
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    let info;
    try {
      info = await lstat(discovery.receipt.socketPath);
    } catch (error: unknown) {
      return {
        status: "failed",
        code: "DAEMON_SOCKET_UNAVAILABLE",
        detail: errorDetail(error),
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    if (!info.isSocket() || info.uid !== process.getuid?.()) {
      return {
        status: "failed",
        code: "DAEMON_SOCKET_UNSAFE",
        detail: "daemon socket is not owned by the current user",
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    try {
      const client = await connectFabricDaemon({
        socketPath: discovery.receipt.socketPath,
        capability: discovery.receipt.bootstrapCapability,
      });
      await client.close();
    } catch (error: unknown) {
      return {
        status: "failed",
        code: "DAEMON_HANDSHAKE_FAILED",
        detail: errorDetail(error),
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    return {
      status: "live",
      code: "DAEMON_LIVE",
      detail: "daemon discovery, process, socket and handshake are healthy",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
    };
      } finally {
        await shutdown.handle.release();
      }
    });
  } catch (error: unknown) {
    return {
      status: "failed",
      code: errorCode(error, "DAEMON_DISCOVERY_FAILED"),
      detail: errorDetail(error),
      pid: null,
      socketPath: null,
    };
  }
}

export async function fabricDoctor(
  arguments_: string[],
  paths: FabricPaths,
  dependencies: { verifyProvider?: typeof verifyProviderConformance } = {},
): Promise<Record<string, unknown>> {
  const selected = pathsFor(arguments_);
  let adapterIds: string[] = [];
  let adapterCommands: string[][] = [];
  const checks: Check[] = [];
  checks.push(await check("configuration", async () => {
    const config = await loadFabricConfig({ globalPath: selected.config, agentsHome: selected.agentsHome });
    adapterIds = config.adapterIds;
    adapterCommands = adapterIds.map((adapterId) => config.adapterCommands[adapterId] ?? []);
  }));
  checks.push(await check("wrapper-loader", async () => {
    const loaderParts = [...new Set(adapterCommands.flat().filter((part) => part.includes("node_modules/tsx/dist/loader.mjs")))];
    for (const part of loaderParts) {
      const loaderPath = part.startsWith("${AGENTS_HOME}/") ? join(selected.agentsHome, part.slice("${AGENTS_HOME}/".length)) : part;
      try {
        await lstat(loaderPath);
      } catch {
        throw new Error(
          `tsx loader is missing: ${loaderPath}. Adapter wrappers execute tracked TypeScript source through tsx; ` +
          "reinstall dependencies including devDependencies (npm ci, not npm ci --omit=dev).",
        );
      }
    }
    return loaderParts.length === 0 ? "no tsx wrapper commands configured" : "tsx loader present";
  }));
  checks.push(await check("adapter-compatibility", async () => {
    const verification = await verifyAdapterCompatibility({ compatibilityPath: selected.compatibility, schemaPath: selected.compatibilitySchema, adapterIds, requireEnabled: true });
    return verification.wrapperProvenance
      .map((item) => `${item.adapterId}=${item.repositoryCommit}:${item.wrapperPath}`)
      .join(" ");
  }));
  checks.push(await check("provider-conformance", async () => {
    const verification = await verifyAdapterCompatibility({ compatibilityPath: selected.compatibility, schemaPath: selected.compatibilitySchema, adapterIds, requireEnabled: true });
    const observations = [];
    for (const adapterId of adapterIds) {
      const executable = verification.resolvedExecutables[adapterId];
      if (executable === undefined) throw new Error(`provider executable is missing: ${adapterId}`);
      const policy = await loadAdapterModelConstraints({
        compatibilityPath: selected.compatibility,
        schemaPath: selected.compatibilitySchema,
        adapterId,
      });
      if (policy.providerIdentity === undefined) continue;
      const observation = await (dependencies.verifyProvider ?? verifyProviderConformance)({
        adapterId,
        executable,
        ...(policy.cursorInstallRoot === undefined ? {} : { cursorInstallRoot: policy.cursorInstallRoot }),
        ...(policy.providerInstallRoot === undefined ? {} : { providerInstallRoot: policy.providerInstallRoot }),
      });
      observations.push(`${adapterId}=${observation.interface.version}:${observation.identity.sha256}:${observation.identity.assurance}`);
    }
    return observations.join(" ");
  }));
  for (const [id, path, expectedKind] of [
    ["state-directory", paths.stateDirectory, "directory"],
    ["runtime-directory", paths.runtimeDirectory, "directory"],
  ] as const) {
    checks.push(await check(id, async () => {
      const info = await lstat(path);
      if (info.isSymbolicLink() || (expectedKind === "directory" && !info.isDirectory()) || (info.mode & 0o077) !== 0) throw new Error(`${path} must be a private non-symlink directory`);
    }));
  }
  checks.push(await check("database-integrity", () => {
    const database = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
    try { assertDatabaseIntegrity(database); } finally { database.close(); }
  }));
  const daemon = await doctorDaemonState(paths);
  checks.push({
    id: "daemon-socket",
    status: daemon.status === "live" ? "pass" : daemon.status === "idle" ? "idle" : "fail",
    code: daemon.code,
    detail: daemon.detail,
  });
  const failed = checks.find((item) => item.status === "fail");
  return {
    schemaVersion: 1,
    healthy: failed === undefined,
    state: failed === undefined ? daemon.status : "failed",
    code: failed?.code ?? daemon.code,
    daemon: {
      status: daemon.status,
      pid: daemon.pid,
      socketPath: daemon.socketPath,
    },
    checks,
  };
}
