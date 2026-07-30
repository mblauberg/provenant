import type { DaemonStartOptions } from "../daemon/client.js";
import type { FabricPaths } from "./paths.js";
import { resolveSplitConfiguration, type SplitRootOptions } from "./split-config-paths.js";

export function defaultDaemonStartOptions(
  paths: FabricPaths,
  agentsHomeValue: string | undefined,
  options: Omit<SplitRootOptions, "agentsHomeValue"> = {},
): DaemonStartOptions {
  return {
    ...paths,
    configuration: resolveSplitConfiguration({ ...options, agentsHomeValue }),
  };
}
