import { existsSync } from "node:fs";
import { join } from "node:path";

import type { DaemonStartOptions } from "../daemon/client.js";
import { resolveFabricRoots, type FabricRootResolutionOptions } from "../domain/fabric-roots.js";
import { hasPairedInstanceRoot, type RootPairingDependencies } from "./instance-root-pairing.js";

export type SplitConfiguration = NonNullable<DaemonStartOptions["configuration"]>;

export type SplitConfigurationOptions = FabricRootResolutionOptions & RootPairingDependencies;

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
 * The local layer is offered only when the instance pointer pairs the resolved
 * instance with the resolved product, the file exists and it is not the very
 * file already loaded as the global layer. A default `~/.agents` instance must
 * not become a local layer for an unrelated product tree selected through
 * `AGENTS_HOME`.
 */
export function resolveSplitConfiguration(
  options: SplitConfigurationOptions = {},
): SplitConfiguration {
  const exists = options.exists ?? existsSync;
  const { productRoot, instanceRoot } = resolveFabricRoots(options);
  const globalConfigPath = join(productRoot, "config", "agent-fabric.yaml");
  const localConfigPath = join(instanceRoot, "config", "agent-fabric.yaml");
  const offerLocal = hasPairedInstanceRoot({ productRoot, instanceRoot }, options)
    && localConfigPath !== globalConfigPath
    && exists(localConfigPath);
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
