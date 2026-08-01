import { connectFabricDaemon, startFabricDaemon } from "../../src/index.ts";
import type { Fabric, FabricClient } from "../../src/index.ts";

import {
  terminateTrackedTestProcess,
  trackTestProcess,
  untrackTestProcess,
} from "./test-process-registry.ts";

export type RetainedDaemon = Awaited<ReturnType<typeof startFabricDaemon>>;
export type RetainedDaemonOptions = Parameters<typeof startFabricDaemon>[0];

export type RetainedDaemonStarted = (input: {
  pid: number;
  directory: string;
  releaseDaemonHandle: () => void;
}) => Promise<void> | void;

export async function startTrackedRetainedDaemon(
  options: RetainedDaemonOptions,
  label: string,
): Promise<RetainedDaemon> {
  const daemon = await startFabricDaemon(options);
  trackTestProcess(daemon.pid, label);
  return daemon;
}

export async function stopTrackedRetainedDaemon(daemon: RetainedDaemon): Promise<void> {
  try {
    await daemon.stop();
    untrackTestProcess(daemon.pid);
  } finally {
    await terminateTrackedTestProcess(daemon.pid);
  }
}

export async function restartTrackedRetainedDaemon(input: {
  options: RetainedDaemonOptions;
  socketPath: string;
  capability: string;
  label: string;
}): Promise<{ fabric: Fabric; chair: FabricClient }> {
  const daemon = await startTrackedRetainedDaemon(input.options, input.label);
  let chair: Awaited<ReturnType<typeof connectFabricDaemon>>;
  try {
    chair = await connectFabricDaemon({
      socketPath: input.socketPath,
      capability: input.capability,
    });
  } catch (error: unknown) {
    await stopTrackedRetainedDaemon(daemon);
    throw error;
  }

  let closed = false;
  return {
    chair: chair as unknown as FabricClient,
    fabric: {
      close: async () => {
        if (closed) return;
        closed = true;
        await chair.close();
        await stopTrackedRetainedDaemon(daemon);
      },
    } as unknown as Fabric,
  };
}
