import { join, resolve } from "node:path";

import { verifyAdapterCompatibility } from "../adapters/compatibility.js";
import { loadFabricConfig } from "../config/index.js";
import { FabricError } from "../errors.js";
import { verifyProviderConformance } from "../adapters/provider-conformance.js";
import { loadAdapterModelConstraints } from "../adapters/model-selection.js";
import { resolveFabricRoots } from "../domain/fabric-roots.js";

const VALUE_OPTIONS = [
  "--adapter",
  "--agents-home",
  "--product-root",
  "--instance-root",
  "--config",
  "--compatibility",
  "--compatibility-schema",
] as const;

type ValueOption = typeof VALUE_OPTIONS[number];

function parseArguments(arguments_: string[]): Partial<Record<ValueOption, string>> {
  const allowed = new Set<string>(VALUE_OPTIONS);
  const parsed: Partial<Record<ValueOption, string>> = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index];
    if (name === undefined || !allowed.has(name)) {
      throw new Error(`adapter executable received unknown option: ${name ?? "<missing>"}`);
    }
    const option = name as ValueOption;
    if (parsed[option] !== undefined) {
      throw new Error(`adapter executable received duplicate option: ${option}`);
    }
    const value = arguments_[index + 1];
    if (value === undefined || value.length === 0 || value.startsWith("-")) {
      throw new Error(`adapter executable requires a value for ${option}`);
    }
    parsed[option] = value;
  }
  return parsed;
}

export async function resolveAdapterExecutableCli(
  arguments_: string[],
  dependencies: { verifyProvider?: typeof verifyProviderConformance } = {},
): Promise<string> {
  const parsed = parseArguments(arguments_);
  const adapterId = parsed["--adapter"];
  if (adapterId === undefined) {
    throw new Error("adapter executable requires --adapter <id>");
  }
  const { productRoot, instanceRoot } = resolveFabricRoots({
    agentsHomeFlag: parsed["--agents-home"],
    productRootFlag: parsed["--product-root"],
    instanceRootFlag: parsed["--instance-root"],
  });
  const compatibilityPath = resolve(
    parsed["--compatibility"] ?? join(instanceRoot, "config", "adapter-compatibility.yaml"),
  );
  const configPath = resolve(
    parsed["--config"] ?? join(instanceRoot, "config", "agent-fabric.yaml"),
  );
  const schemaPath = resolve(
    parsed["--compatibility-schema"] ??
      join(productRoot, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json"),
  );
  // #528 keeps ${AGENTS_HOME} as the product-side compatibility token. #530
  // owns the separate instance-root token wiring for trusted workspace roots.
  const config = await loadFabricConfig({ globalPath: configPath, agentsHome: productRoot });
  if (!config.adapterIds.includes(adapterId)) {
    throw new FabricError("ADAPTER_DISABLED", `adapter is not active in trusted Fabric configuration: ${adapterId}`);
  }
  const verification = await verifyAdapterCompatibility({
    compatibilityPath,
    schemaPath,
    adapterIds: [adapterId],
    requireEnabled: true,
  });
  const executable = verification.resolvedExecutables[adapterId];
  if (executable === undefined) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `activated adapter has no provider executable: ${adapterId}`,
    );
  }
  const policy = await loadAdapterModelConstraints({ compatibilityPath, schemaPath, adapterId, requireEnabled: true });
  await (dependencies.verifyProvider ?? verifyProviderConformance)({
    adapterId,
    executable,
    ...(policy.cursorInstallRoot === undefined ? {} : { cursorInstallRoot: policy.cursorInstallRoot }),
    ...(policy.providerInstallRoot === undefined ? {} : { providerInstallRoot: policy.providerInstallRoot }),
  });
  return executable;
}
