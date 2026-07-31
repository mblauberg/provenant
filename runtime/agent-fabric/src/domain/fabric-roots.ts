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
 * caller supplies the pointer value after reading it.
 */
export function isInstanceRootPaired(
  roots: FabricRoots,
  pointerProductRoot: string | undefined,
): boolean {
  return pointerProductRoot !== undefined && resolve(pointerProductRoot) === roots.productRoot;
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
