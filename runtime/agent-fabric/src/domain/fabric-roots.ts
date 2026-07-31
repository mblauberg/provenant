import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";

export type FabricRoots = {
  productRoot: string;
  instanceRoot: string;
};

export type FabricRootResolutionOptions = {
  productRootFlag?: string | undefined;
  instanceRootFlag?: string | undefined;
  agentsHomeFlag?: string | undefined;
  environment?: NodeJS.ProcessEnv | undefined;
};

function environmentPath(environment: NodeJS.ProcessEnv, name: string): string | undefined {
  const value = environment[name];
  if (value === undefined || value.length === 0) return undefined;
  if (!isAbsolute(value)) throw new TypeError(`${name} must be an absolute path, got ${value}`);
  return resolve(value);
}

function optionalPath(value: string | undefined): string | undefined {
  return value === undefined || value.length === 0 ? undefined : resolve(value);
}

export function resolveFabricRoots(options: FabricRootResolutionOptions): FabricRoots {
  const environment = options.environment ?? process.env;
  const agentsHomeFlag = optionalPath(options.agentsHomeFlag);
  const defaultRoot = resolve(join(homedir(), ".agents"));
  return {
    productRoot:
      optionalPath(options.productRootFlag) ??
      agentsHomeFlag ??
      environmentPath(environment, "AGENT_FABRIC_PRODUCT_ROOT") ??
      environmentPath(environment, "AGENTS_HOME") ??
      defaultRoot,
    instanceRoot:
      optionalPath(options.instanceRootFlag) ??
      agentsHomeFlag ??
      environmentPath(environment, "AGENT_FABRIC_INSTANCE_ROOT") ??
      defaultRoot,
  };
}

/**
 * An instance layer is legitimate when its machine-local pointer names the
 * resolved product. Filesystem access stays outside this domain module; the
 * caller supplies both paths already canonicalised, because the two sides are
 * produced by different tools: `install-harness` records
 * `product_root.resolve(strict=True)`, which follows symlinks, while a product
 * root taken from a flag or the environment is only lexically resolved. Comparing
 * those two directly reports a genuinely paired instance as unpaired whenever the
 * product is reached through a symlink, and the local layer then silently
 * disappears.
 */
export function isInstanceRootPaired(
  canonicalProductRoot: string | undefined,
  pointerProductRoot: string | undefined,
): boolean {
  return canonicalProductRoot !== undefined
    && pointerProductRoot !== undefined
    && canonicalProductRoot === pointerProductRoot;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

export function fabricCliCommand(options: {
  productRootFlag?: string | undefined;
  agentsHomeFlag?: string | undefined;
  environment?: NodeJS.ProcessEnv | undefined;
} = {}): string {
  const { productRoot } = resolveFabricRoots(options);
  return shellQuote(join(productRoot, "scripts", "agent-fabric"));
}
