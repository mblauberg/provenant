import { lstat } from "node:fs/promises";
import { join, resolve } from "node:path";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

import {
  mergeAdapterExecutableFailures,
  type AdapterExecutableFailure,
} from "../adapters/compatibility.js";
import { admitTsxLoader } from "../adapters/tsx-loader.js";
import { BootstrapElection } from "../daemon/bootstrap-election.js";
import { connectFabricDaemon } from "../daemon/client.js";
import {
  privateDiscoveryMatchesBootstrapReady,
  privateDiscoveryPaths,
  readPrivateDiscovery,
} from "../daemon/private-discovery.js";
import type { FabricPaths } from "./paths.js";
import { FABRIC_PROTOCOL_VERSION } from "../daemon/protocol.js";
import { currentRuntimeBuildIdentity } from "../daemon/runtime-build-identity.js";

export type FabricDaemonStatus = {
  reachable: boolean;
  status: "live" | "offline" | "incompatible" | "stale";
  pid: number | null;
  socketPath: string;
  protocolVersion: typeof FABRIC_PROTOCOL_VERSION;
  activeAdapters: string[];
  code?: string;
  detail?: string;
  remedy?: string;
};

type StatusDaemonClient = Pick<Awaited<ReturnType<typeof connectFabricDaemon>>, "initializeResult" | "close">;
export type StatusDependencies = {
  connectDaemon?: (input: Parameters<typeof connectFabricDaemon>[0]) => Promise<StatusDaemonClient>;
  inspectDaemonSocket?: (path: string) => Promise<{ isSocket(): boolean; uid: number }>;
  runtimeBuildIdentity?: string;
};

export function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function errorCode(error: unknown, fallback: string): string {
  return error instanceof Error && "code" in error && typeof error.code === "string"
    ? error.code
    : fallback;
}

function isProtocolIncompatibility(error: unknown): boolean {
  return ["DAEMON_PROTOCOL_MISMATCH", "DAEMON_PROTOCOL_UNSUPPORTED", "PROTOCOL_INCOMPATIBLE"]
    .includes(errorCode(error, ""));
}

export async function daemonState(
  paths: FabricPaths,
  dependencies: StatusDependencies = {},
): Promise<FabricDaemonStatus> {
  let discovery;
  try {
    const directory = await lstat(paths.runtimeDirectory);
    if (directory.isSymbolicLink() || !directory.isDirectory() || (directory.mode & 0o777) !== 0o700) {
      throw new Error("fabric runtime directory is not a private non-symlink directory");
    }
    discovery = await readPrivateDiscovery(privateDiscoveryPaths(paths.runtimeDirectory), paths.socketPath);
  } catch {
    return {
      reachable: false,
      status: "offline",
      pid: null,
      socketPath: paths.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
    };
  }
  if (discovery.status === "ambiguous") {
    return {
      reachable: false,
      status: "offline",
      pid: discovery.owner?.pid ?? discovery.receipt?.pid ?? null,
      socketPath: paths.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
      code: "DAEMON_DISCOVERY_AMBIGUOUS",
      detail: discovery.message,
    };
  }
  if (discovery.status !== "active") {
    return {
      reachable: false,
      status: "offline",
      pid: null,
      socketPath: paths.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
    };
  }
  let election;
  try {
    election = await new BootstrapElection({ runtimeDirectory: paths.runtimeDirectory })
      .inspectCurrentReadOnlyWith(async (inspection) => inspection);
  } catch (error: unknown) {
    return {
      reachable: false,
      status: "offline",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
      code: errorCode(error, "DAEMON_ELECTION_INCONSISTENT"),
      detail: errorDetail(error),
    };
  }
  if (
    election.status !== "ready" ||
    !privateDiscoveryMatchesBootstrapReady(discovery.owner, election.receipt)
  ) {
    return {
      reachable: false,
      status: "offline",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
      code: election.status === "active" ? "BOOTSTRAP_IN_PROGRESS" : "DAEMON_ELECTION_INCONSISTENT",
      detail: election.status === "active"
        ? "bootstrap election is active"
        : "active daemon discovery does not match the successful bootstrap election",
    };
  }
  try {
    process.kill(discovery.receipt.pid, 0);
  } catch {
    return {
      reachable: false,
      status: "offline",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
    };
  }
  const expectedRuntimeBuildIdentity = dependencies.runtimeBuildIdentity ?? await currentRuntimeBuildIdentity();
  if (discovery.receipt.runtimeBuildIdentity !== expectedRuntimeBuildIdentity) {
    try {
      const info = await (dependencies.inspectDaemonSocket ?? lstat)(discovery.receipt.socketPath);
      if (!info.isSocket() || info.uid !== process.getuid?.()) {
        return {
          reachable: false,
          status: "offline",
          pid: discovery.receipt.pid,
          socketPath: discovery.receipt.socketPath,
          protocolVersion: FABRIC_PROTOCOL_VERSION,
          activeAdapters: [],
        };
      }
    } catch {
      return {
        reachable: false,
        status: "offline",
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
        protocolVersion: FABRIC_PROTOCOL_VERSION,
        activeAdapters: [],
      };
    }
    return {
      reachable: true,
      status: "stale",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
      code: "DAEMON_STALE_BUILD",
      detail: discovery.receipt.runtimeBuildIdentity === undefined
        ? "daemon discovery has no runtime build identity; operator reconciliation is required"
        : "daemon runtime build identity does not match the current client build; operator reconciliation is required",
      remedy: "do not signal the live daemon; reconcile its owning lifecycle, then rerun provenant status",
    };
  }
  try {
    const client = await (dependencies.connectDaemon ?? connectFabricDaemon)({
      socketPath: discovery.receipt.socketPath,
      capability: discovery.receipt.bootstrapCapability,
      requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
    });
    const activeAdapters = client.initializeResult.activeAdapters;
    await client.close();
    return {
      reachable: true,
      status: "live",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters,
    };
  } catch (error: unknown) {
    if (isProtocolIncompatibility(error)) {
      return {
        reachable: true,
        status: "incompatible",
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
        protocolVersion: FABRIC_PROTOCOL_VERSION,
        activeAdapters: [],
        code: "DAEMON_PROTOCOL_INCOMPATIBLE",
        detail: errorDetail(error),
        remedy: "restart the daemon through its owning Fabric lifecycle, then rerun provenant status",
      };
    }
    return {
      reachable: false,
      status: "offline",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
    };
  }
}

export async function verifyConfiguredTsxLoaders(
  adapterCommands: readonly (readonly string[])[],
  productRoot: string,
): Promise<string> {
  const loaderParts = [...new Set(adapterCommands.flatMap((command) =>
    command.filter((part) => part.includes("node_modules/") && part.endsWith("/loader.mjs"))))];
  for (const part of loaderParts) {
    const loaderPath = part.startsWith("${AGENTS_HOME}/")
      ? join(productRoot, part.slice("${AGENTS_HOME}/".length))
      : resolve(part);
    await admitTsxLoader({ loaderPath, productRoot });
  }
  return loaderParts.length === 0 ? "no tsx wrapper commands configured" : "tsx loader present";
}

export function mergeOptionalAdapterFailures(
  ...groups: readonly (readonly AdapterExecutableFailure[])[]
): AdapterExecutableFailure[] {
  return mergeAdapterExecutableFailures(...groups);
}

export function optionalAdapterFailureDetail(failures: readonly AdapterExecutableFailure[]): string {
  return failures
    .map((item) => `${item.adapterId}=unavailable: ${item.reasons.join("; ")}`)
    .join(" ");
}
