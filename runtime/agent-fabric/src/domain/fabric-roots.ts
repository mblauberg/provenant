import { homedir } from "node:os";
import { join, resolve } from "node:path";

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
  return value === undefined || value.length === 0 ? undefined : resolve(value);
}

function optionalPath(value: string | undefined): string | undefined {
  return value === undefined || value.length === 0 ? undefined : resolve(value);
}

export function resolveFabricRoots(options: FabricRootResolutionOptions): FabricRoots {
  const environment = options.environment ?? process.env;
  const agentsHome =
    optionalPath(options.agentsHomeFlag) ??
    environmentPath(environment, "AGENTS_HOME") ??
    resolve(join(homedir(), ".agents"));
  return {
    productRoot:
      optionalPath(options.productRootFlag) ??
      environmentPath(environment, "AGENT_FABRIC_PRODUCT_ROOT") ??
      agentsHome,
    instanceRoot:
      optionalPath(options.instanceRootFlag) ??
      environmentPath(environment, "AGENT_FABRIC_INSTANCE_ROOT") ??
      agentsHome,
  };
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
