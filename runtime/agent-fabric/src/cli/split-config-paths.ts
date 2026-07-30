import { existsSync } from "node:fs";
import { join } from "node:path";

import type { DaemonStartOptions } from "../daemon/client.js";
import { resolveFabricRoots, type FabricRootResolutionOptions } from "../domain/fabric-roots.js";

export type SplitConfiguration = NonNullable<DaemonStartOptions["configuration"]>;

export type SplitConfigurationOptions = FabricRootResolutionOptions & {
  /** Injected for tests; production reads the filesystem. */
  exists?: ((path: string) => boolean) | undefined;
};

/**
 * Bind every configuration path to the root that owns its file class.
 *
 * ADR 0019 assigns the shipped `agent-fabric.yaml`, `adapter-compatibility.yaml`
 * and the schemas to the product-shipped projection, and the instance's own
 * `agent-fabric.yaml` to the instance as a narrowing-only layer. So the product
 * root carries the global layer and the instance root is offered as
 * `localPath`, which the existing typed merge in `config/index.ts` admits only
 * as narrowing: allow-lists intersect, workspace roots must stay contained and
 * limits take the minimum.
 *
 * The local layer is offered only when it exists and is not the very file
 * already loaded as the global layer, so a fused layout, where both roots are
 * one directory, resolves exactly as it did before.
 */
export function resolveSplitConfiguration(
  options: SplitConfigurationOptions = {},
): SplitConfiguration {
  const exists = options.exists ?? existsSync;
  const { productRoot, instanceRoot } = resolveFabricRoots(options);
  const globalConfigPath = join(productRoot, "config", "agent-fabric.yaml");
  const localConfigPath = join(instanceRoot, "config", "agent-fabric.yaml");
  const offerLocal = localConfigPath !== globalConfigPath && exists(localConfigPath);
  return {
    globalConfigPath,
    ...(offerLocal ? { localConfigPath } : {}),
    compatibilityPath: join(productRoot, "config", "adapter-compatibility.yaml"),
    compatibilitySchemaPath: join(
      productRoot,
      "runtime",
      "agent-fabric",
      "schemas",
      "adapter-compatibility.schema.json",
    ),
    // The `${AGENTS_HOME}` token expands against the product, because the
    // shipped adapter commands address product code with it (#528).
    agentsHome: productRoot,
  };
}
