import { join } from "node:path";

import type { DaemonStartOptions } from "../daemon/client.js";
import type { FabricPaths } from "./paths.js";
import { resolveFabricRoots } from "./root-resolution.js";

export function defaultDaemonStartOptions(
  paths: FabricPaths,
  options: {
    agentsHomeFlag?: string | undefined;
    productRootFlag?: string | undefined;
    instanceRootFlag?: string | undefined;
    environment?: NodeJS.ProcessEnv | undefined;
  } = {},
): DaemonStartOptions {
  const { productRoot, instanceRoot } = resolveFabricRoots(options);
  return {
    ...paths,
    configuration: {
      globalConfigPath: join(instanceRoot, "config", "agent-fabric.yaml"),
      compatibilityPath: join(instanceRoot, "config", "adapter-compatibility.yaml"),
      compatibilitySchemaPath: join(
        productRoot,
        "runtime",
        "agent-fabric",
        "schemas",
        "adapter-compatibility.schema.json",
      ),
      agentsHome: productRoot,
    },
  };
}
