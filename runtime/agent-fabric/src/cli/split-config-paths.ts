import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";

import type { DaemonStartOptions } from "../daemon/client.js";

export type SplitConfiguration = NonNullable<DaemonStartOptions["configuration"]>;

export type SplitRootOptions = {
  /** The agents-home a caller already resolved, usually from `AGENTS_HOME`. */
  agentsHomeValue?: string | undefined;
  environment?: NodeJS.ProcessEnv | undefined;
  /** Injected for tests; production reads the filesystem. */
  exists?: ((path: string) => boolean) | undefined;
};

export type SplitRoots = {
  productRoot: string;
  instanceRoot: string;
};

function environmentRoot(environment: NodeJS.ProcessEnv, name: string): string | undefined {
  const value = environment[name];
  if (value === undefined || value.length === 0) return undefined;
  if (!isAbsolute(value)) throw new TypeError(`${name} must be an absolute path, got ${value}`);
  return resolve(value);
}

/**
 * Resolve the product and instance roots for normal daemon startup.
 *
 * The product root carries the shipped configuration, the compatibility policy
 * and the schemas; the instance root carries the user's own narrowing layer
 * (ADR 0019). An explicit `AGENT_FABRIC_INSTANCE_ROOT` outranks the generic
 * agents-home input on purpose: after the split `AGENTS_HOME` names the
 * product, because it is the token the shipped adapter commands expand
 * against, so without that ordering a split layout would collapse back to a
 * fused one whenever `AGENTS_HOME` was set.
 */
export function resolveSplitRoots(options: SplitRootOptions = {}): SplitRoots {
  const environment = options.environment ?? process.env;
  const agentsHome =
    options.agentsHomeValue === undefined || options.agentsHomeValue.length === 0
      ? undefined
      : resolve(options.agentsHomeValue);
  const defaultRoot = resolve(join(homedir(), ".agents"));
  return {
    productRoot:
      agentsHome ??
      environmentRoot(environment, "AGENT_FABRIC_PRODUCT_ROOT") ??
      environmentRoot(environment, "AGENTS_HOME") ??
      defaultRoot,
    instanceRoot:
      environmentRoot(environment, "AGENT_FABRIC_INSTANCE_ROOT") ??
      agentsHome ??
      environmentRoot(environment, "AGENTS_HOME") ??
      defaultRoot,
  };
}

/**
 * Bind every configuration path to the root that owns its file class.
 *
 * The instance file is offered as `localPath`, which the existing typed merge
 * in `config/index.ts` admits only as narrowing: allow-lists intersect,
 * workspace roots must stay contained and limits take the minimum. It is
 * offered only when it exists and is not the very file already loaded as the
 * global layer, so a fused layout resolves exactly as it did before.
 */
export function resolveSplitConfiguration(options: SplitRootOptions = {}): SplitConfiguration {
  const exists = options.exists ?? existsSync;
  const { productRoot, instanceRoot } = resolveSplitRoots(options);
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
    agentsHome: productRoot,
  };
}
