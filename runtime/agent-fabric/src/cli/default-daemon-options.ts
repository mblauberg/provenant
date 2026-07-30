import type { DaemonStartOptions } from "../daemon/client.js";
import type { FabricPaths } from "./paths.js";
import { resolveSplitConfiguration, type SplitConfigurationOptions } from "./split-config-paths.js";

export function defaultDaemonStartOptions(
  paths: FabricPaths,
  options: SplitConfigurationOptions = {},
): DaemonStartOptions {
  return {
    ...paths,
    configuration: resolveSplitConfiguration(options),
  };
}
