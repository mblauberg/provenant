import { existsSync } from "node:fs";
import { join, resolve } from "node:path";

import { verifyAdapterCompatibility } from "../adapters/compatibility.js";
import { loadFabricConfig } from "../config/index.js";
import { FabricError } from "../errors.js";
import { verifyProviderConformance } from "../adapters/provider-conformance.js";
import {
  supportsCertifyingAnswerBearingLeg,
  type ProviderIdentityAssurance,
} from "../adapters/provider-identity.js";
import { loadAdapterModelConstraints } from "../adapters/model-selection.js";
import { resolveFabricRoots } from "../domain/fabric-roots.js";
import { hasPairedInstanceRoot, type RootPairingDependencies } from "./instance-root-pairing.js";
import { verifyNpmInstallAttestation } from "../adapters/npm-install-attestation-verifier.js";

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

type ParsedArguments = Partial<Record<ValueOption, string>> & { json?: boolean };

function parseArguments(arguments_: string[]): ParsedArguments {
  const allowed = new Set<string>(VALUE_OPTIONS);
  const parsed: ParsedArguments = {};
  for (let index = 0; index < arguments_.length;) {
    const name = arguments_[index];
    if (name === "--json") {
      if (parsed.json === true) throw new Error("adapter executable received duplicate option: --json");
      parsed.json = true;
      index += 1;
      continue;
    }
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
    index += 2;
  }
  return parsed;
}

type AdapterExecutableResolution = Readonly<{
  executable: string;
  providerAssurance?: ProviderIdentityAssurance;
  certifyingAnswerBearingLeg: boolean;
}>;

async function resolveAdapterExecutableResolutionCli(
  arguments_: string[],
  dependencies: {
    verifyProvider?: typeof verifyProviderConformance;
    rootPairing?: RootPairingDependencies;
    verifyNpmInstall?: typeof verifyNpmInstallAttestation;
  } = {},
): Promise<AdapterExecutableResolution> {
  const parsed = parseArguments(arguments_);
  const adapterId = parsed["--adapter"];
  if (adapterId === undefined) {
    throw new Error("adapter executable requires --adapter <id>");
  }
  const rootOptions = {
    agentsHomeFlag: parsed["--agents-home"],
    productRootFlag: parsed["--product-root"],
    instanceRootFlag: parsed["--instance-root"],
  };
  const { productRoot, instanceRoot } = resolveFabricRoots(rootOptions);
  const compatibilityPath = resolve(
    parsed["--compatibility"] ?? join(productRoot, "config", "adapter-compatibility.yaml"),
  );
  const pinnedConfig = parsed["--config"];
  const configPath = resolve(pinnedConfig ?? join(productRoot, "config", "agent-fabric.yaml"));
  const instanceConfigPath = resolve(join(instanceRoot, "config", "agent-fabric.yaml"));
  const localConfigPath =
    pinnedConfig === undefined
      && hasPairedInstanceRoot({ productRoot, instanceRoot }, dependencies.rootPairing)
      && instanceConfigPath !== configPath
      && existsSync(instanceConfigPath)
      ? instanceConfigPath
      : undefined;
  const schemaPath = resolve(
    parsed["--compatibility-schema"] ??
      join(productRoot, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json"),
  );
  // #528 keeps ${AGENTS_HOME} as the product-side token; ADR 0019 makes the
  // shipped configuration and compatibility product-owned and the instance file
  // a narrowing layer. Composing both here means this command agrees with the
  // daemon about whether an adapter is active.
  const config = await loadFabricConfig({
    globalPath: configPath,
    ...(localConfigPath === undefined ? {} : { localPath: localConfigPath }),
    agentsHome: productRoot,
  });
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
  await (dependencies.verifyNpmInstall ?? verifyNpmInstallAttestation)(productRoot);
  const policy = await loadAdapterModelConstraints({ compatibilityPath, schemaPath, adapterId, requireEnabled: true });
  const providerVerification = await (dependencies.verifyProvider ?? verifyProviderConformance)({
    adapterId,
    executable,
    ...(policy.cursorInstallRoot === undefined ? {} : { cursorInstallRoot: policy.cursorInstallRoot }),
    ...(policy.providerInstallRoot === undefined ? {} : { providerInstallRoot: policy.providerInstallRoot }),
  });
  const providerAssurance = providerVerification.identity?.assurance;
  return {
    executable,
    providerAssurance,
    certifyingAnswerBearingLeg: providerAssurance === undefined
      ? false
      : supportsCertifyingAnswerBearingLeg(providerAssurance),
  };
}

export type AdapterExecutableAttestation = Readonly<{
  executable: string;
  providerAssurance: ProviderIdentityAssurance;
  certifyingAnswerBearingLeg: boolean;
}>;

export async function resolveAdapterExecutableAttestationCli(
  arguments_: string[],
  dependencies: {
    verifyProvider?: typeof verifyProviderConformance;
    rootPairing?: RootPairingDependencies;
    verifyNpmInstall?: typeof verifyNpmInstallAttestation;
  } = {},
): Promise<AdapterExecutableAttestation> {
  const result = await resolveAdapterExecutableResolutionCli(arguments_, dependencies);
  if (result.providerAssurance === undefined) {
    throw new FabricError("ADAPTER_IDENTITY_MISMATCH", "provider conformance did not observe an assurance level");
  }
  return result as AdapterExecutableAttestation;
}

export async function resolveAdapterExecutableCli(
  arguments_: string[],
  dependencies: {
    verifyProvider?: typeof verifyProviderConformance;
    rootPairing?: RootPairingDependencies;
    verifyNpmInstall?: typeof verifyNpmInstallAttestation;
  } = {},
): Promise<string> {
  return (await resolveAdapterExecutableResolutionCli(arguments_, dependencies)).executable;
}
