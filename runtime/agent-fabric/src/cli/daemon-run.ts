import { resolve } from "node:path";

import { startFabricDaemon, type DaemonStartOptions } from "../daemon/client.js";
import { resolveFabricPaths } from "./paths.js";

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  if (index === -1) return undefined;
  const value = arguments_[index + 1];
  if (value === undefined || value.startsWith("--")) {
    throw new Error(`${name} requires a path`);
  }
  return resolve(value);
}

export function daemonConfiguration(arguments_: string[]): DaemonStartOptions["configuration"] {
  const noAdapters = arguments_.includes("--no-adapters");
  const trustedConfigPath = option(arguments_, "--trusted-config");
  if (noAdapters === (trustedConfigPath !== undefined)) {
    throw new Error("daemon run requires exactly one of --no-adapters or --trusted-config PATH");
  }
  if (noAdapters) return undefined;
  const compatibilityPath = option(arguments_, "--compatibility");
  const compatibilitySchemaPath = option(arguments_, "--compatibility-schema");
  const agentsHome = option(arguments_, "--agents-home");
  if (
    trustedConfigPath === undefined ||
    compatibilityPath === undefined ||
    compatibilitySchemaPath === undefined ||
    agentsHome === undefined
  ) {
    throw new Error(
      "trusted daemon start requires --trusted-config, --compatibility, --compatibility-schema and --agents-home",
    );
  }
  const projectRoot = option(arguments_, "--project");
  const localConfigPath = option(arguments_, "--local-config");
  const runConfigPath = option(arguments_, "--run-config");
  return {
    globalConfigPath: trustedConfigPath,
    ...(localConfigPath === undefined ? {} : { localConfigPath }),
    ...(projectRoot === undefined ? {} : { projectRoot }),
    ...(runConfigPath === undefined ? {} : { runConfigPath }),
    compatibilityPath,
    compatibilitySchemaPath,
    agentsHome,
  };
}

export async function runForegroundDaemon(arguments_: string[]): Promise<void> {
  const paths = resolveFabricPaths({ createDirectories: true });
  const configuration = daemonConfiguration(arguments_);
  let daemon: Awaited<ReturnType<typeof startFabricDaemon>> | undefined;
  let signalRequested = false;
  let stopPromise: Promise<void> | undefined;
  const stop = async (): Promise<void> => {
    if (daemon === undefined) return;
    stopPromise ??= daemon.stop();
    return stopPromise;
  };
  const onSignal = (): void => {
    signalRequested = true;
    void stop().catch(() => undefined);
  };
  process.once("SIGINT", onSignal);
  process.once("SIGTERM", onSignal);
  try {
    daemon = await startFabricDaemon({
      databasePath: paths.databasePath,
      stateDirectory: paths.stateDirectory,
      runtimeDirectory: paths.runtimeDirectory,
      socketPath: paths.socketPath,
      ...(configuration === undefined ? {} : { configuration }),
    });
    const runningDaemon = daemon;
    if (signalRequested) await stop();
    const startedAt = new Intl.DateTimeFormat("en-CA", {
      timeZone: "Australia/Brisbane", dateStyle: "short", timeStyle: "medium", hourCycle: "h23",
    }).format(new Date());
    process.stdout.write(`agent-fabric ready pid=${runningDaemon.pid} protocol=1 socket=${runningDaemon.address.path} started=${startedAt} AEST (UTC+10)\n`);
    await runningDaemon.waitForExit();
  } finally {
    process.off("SIGINT", onSignal);
    process.off("SIGTERM", onSignal);
    await stop();
    if (daemon !== undefined) {
      process.stdout.write(daemon.ownsProcess ? "agent-fabric stopped\n" : "agent-fabric detached\n");
    }
  }
}
