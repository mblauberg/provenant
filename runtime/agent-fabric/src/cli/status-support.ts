import { lstat } from "node:fs/promises";
import { join, resolve } from "node:path";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

import {
  mergeAdapterExecutableFailures,
  type AdapterExecutableFailure,
} from "../adapters/compatibility.js";
import { admitTsxLoader } from "../adapters/tsx-loader.js";
import { connectFabricDaemon } from "../daemon/client.js";
import { readDiscoveryReceipt } from "./mcp-provision.js";
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
    discovery = await readDiscoveryReceipt(paths);
    process.kill(discovery.pid, 0);
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
  const expectedRuntimeBuildIdentity = dependencies.runtimeBuildIdentity ?? await currentRuntimeBuildIdentity();
  if (discovery.runtimeBuildIdentity !== expectedRuntimeBuildIdentity) {
    try {
      const info = await (dependencies.inspectDaemonSocket ?? lstat)(discovery.socketPath);
      if (!info.isSocket() || info.uid !== process.getuid?.()) {
        return {
          reachable: false,
          status: "offline",
          pid: discovery.pid,
          socketPath: discovery.socketPath,
          protocolVersion: FABRIC_PROTOCOL_VERSION,
          activeAdapters: [],
        };
      }
    } catch {
      return {
        reachable: false,
        status: "offline",
        pid: discovery.pid,
        socketPath: discovery.socketPath,
        protocolVersion: FABRIC_PROTOCOL_VERSION,
        activeAdapters: [],
      };
    }
    return {
      reachable: true,
      status: "stale",
      pid: discovery.pid,
      socketPath: discovery.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters: [],
      code: "DAEMON_STALE_BUILD",
      detail: discovery.runtimeBuildIdentity === undefined
        ? "daemon discovery has no runtime build identity; operator reconciliation is required"
        : "daemon runtime build identity does not match the current client build; operator reconciliation is required",
      remedy: "do not signal the live daemon; reconcile its owning lifecycle, then rerun provenant status",
    };
  }
  try {
    const client = await (dependencies.connectDaemon ?? connectFabricDaemon)({
      socketPath: discovery.socketPath,
      capability: discovery.bootstrapCapability,
      requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
    });
    const activeAdapters = client.initializeResult.activeAdapters;
    await client.close();
    return {
      reachable: true,
      status: "live",
      pid: discovery.pid,
      socketPath: discovery.socketPath,
      protocolVersion: FABRIC_PROTOCOL_VERSION,
      activeAdapters,
    };
  } catch (error: unknown) {
    if (isProtocolIncompatibility(error)) {
      return {
        reachable: true,
        status: "incompatible",
        pid: discovery.pid,
        socketPath: discovery.socketPath,
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
      pid: discovery.pid,
      socketPath: discovery.socketPath,
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
