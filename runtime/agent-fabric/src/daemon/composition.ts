import type { FabricOpenOptions } from "../domain/types.js";
import { isAbsolute, resolve } from "node:path";
import { verifyAdapterCompatibility, wrapperCommandEntrypointIndex } from "../adapters/compatibility.js";
import { loadAdapterModelConstraints } from "../adapters/model-selection.js";
import { loadFabricConfig } from "../config/index.js";
import { trustedWorkspaceRoots } from "../cli/workspace-trust.js";
import { verifyProviderConformance } from "../adapters/provider-conformance.js";
import { verifyNpmInstallAttestation } from "../adapters/npm-install-attestation-verifier.js";

type AdapterMap = NonNullable<FabricOpenOptions["adapters"]>;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function expandTrustedCommandPart(part: string, agentsHome: string, stateDirectory?: string): string {
  for (const [token, root] of [["${AGENTS_HOME}", agentsHome], ["${FABRIC_STATE_DIRECTORY}", stateDirectory]] as const) {
    if (!part.includes(token)) continue;
    if (root === undefined) throw new TypeError(`${token} requires a trusted runtime path`);
    if (part !== token && !part.startsWith(`${token}/`)) {
      throw new TypeError(`${token} must begin a trusted adapter command path`);
    }
    return resolve(`${root}${part.slice(token.length)}`);
  }
  return part;
}

function replaceUniqueOption(command: string[], option: string, value: string): string[] {
  const result: string[] = [];
  for (let index = 0; index < command.length; index += 1) {
    if (command[index] !== option) {
      result.push(command[index] as string);
      continue;
    }
    if (index + 1 >= command.length) throw new TypeError(`${option} requires a value`);
    index += 1;
  }
  result.push(option, value);
  return result;
}

export function parseDaemonAdapters(serialized: string | undefined): AdapterMap {
  if (serialized === undefined) return {};
  const value: unknown = JSON.parse(serialized);
  if (!isRecord(value)) throw new TypeError("daemon adapter composition must be an object");
  const adapters: AdapterMap = {};
  for (const [adapterId, candidate] of Object.entries(value)) {
    if (!isRecord(candidate) || !Array.isArray(candidate.command) || !candidate.command.every((part) => typeof part === "string") || candidate.command.length === 0 || !isRecord(candidate.environment) || !Object.values(candidate.environment).every((part) => typeof part === "string")) {
      throw new TypeError(`daemon adapter composition is invalid for ${adapterId}`);
    }
    const modelPolicy = candidate.modelPolicy;
    if (modelPolicy !== undefined && (!isRecord(modelPolicy) || !Array.isArray(modelPolicy.allowedFamilies) || !modelPolicy.allowedFamilies.every((item) => typeof item === "string") || !Array.isArray(modelPolicy.allowedModelPatterns) || !modelPolicy.allowedModelPatterns.every((item) => typeof item === "string") || typeof modelPolicy.requiresExplicitModel !== "boolean")) {
      throw new TypeError(`daemon adapter model policy is invalid for ${adapterId}`);
    }
    // Production compositions always attach wrapper provenance (see
    // composeDaemonConfiguration, which fails without it); when present it
    // must be well-formed, and the supervisor re-verifies it at every spawn.
    // Test harnesses may inject provenance-less fixture adapters through
    // AGENT_FABRIC_ADAPTERS_JSON, which is why absence is not rejected here.
    const wrapperProvenance = candidate.wrapperProvenance;
    if (wrapperProvenance !== undefined && (!isRecord(wrapperProvenance) || typeof wrapperProvenance.repositoryCommit !== "string" || typeof wrapperProvenance.wrapperPath !== "string")) {
      throw new TypeError(`daemon adapter wrapper provenance is invalid for ${adapterId}`);
    }
    const npmInstallProductRoot = candidate.npmInstallProductRoot;
    if (npmInstallProductRoot !== undefined && typeof npmInstallProductRoot !== "string") {
      throw new TypeError(`daemon adapter npm install product root is invalid for ${adapterId}`);
    }
    adapters[adapterId] = {
      command: candidate.command,
      environment: candidate.environment as Record<string, string>,
      ...(modelPolicy === undefined ? {} : { modelPolicy: modelPolicy as NonNullable<AdapterMap[string]["modelPolicy"]> }),
      ...(wrapperProvenance === undefined ? {} : {
        wrapperProvenance: wrapperProvenance as NonNullable<AdapterMap[string]["wrapperProvenance"]>,
      }),
      ...(npmInstallProductRoot === undefined ? {} : { npmInstallProductRoot }),
    };
  }
  return adapters;
}

export async function composeDaemonConfiguration(options: {
  globalConfigPath: string;
  localConfigPath?: string;
  projectConfigPath?: string;
  runConfigPath?: string;
  compatibilityPath: string;
  compatibilitySchemaPath: string;
  agentsHome: string;
  stateDirectory?: string;
  verifyProvider?: typeof verifyProviderConformance;
  verifyNpmInstall?: typeof verifyNpmInstallAttestation;
}): Promise<{ adapters: AdapterMap; executionProfile: string; maximumConcurrentProviderTurns: number; workspaceRoots: string[] }> {
  const trustedConfigOptions = {
    globalPath: options.globalConfigPath,
    agentsHome: options.agentsHome,
    ...(options.localConfigPath === undefined ? {} : { localPath: options.localConfigPath }),
  };
  const configOptions = {
    ...trustedConfigOptions,
    ...(options.projectConfigPath === undefined ? {} : { projectPath: options.projectConfigPath }),
    ...(options.runConfigPath === undefined ? {} : { runPath: options.runConfigPath }),
  };
  const allLocalTrustedRoots = options.stateDirectory === undefined
    ? []
    : await trustedWorkspaceRoots({
        stateDirectory: options.stateDirectory,
      });
  const candidateConfig = await loadFabricConfig({ ...configOptions, additionalWorkspaceRoots: allLocalTrustedRoots });
  const eligibleLocalTrustedRoots = options.stateDirectory === undefined
    ? []
    : await trustedWorkspaceRoots({
        stateDirectory: options.stateDirectory,
        executionProfile: candidateConfig.executionProfile ?? "headless",
      });
  const config = eligibleLocalTrustedRoots.length === allLocalTrustedRoots.length &&
      eligibleLocalTrustedRoots.every((root, index) => root === allLocalTrustedRoots[index])
    ? candidateConfig
    : await loadFabricConfig({ ...configOptions, additionalWorkspaceRoots: eligibleLocalTrustedRoots });
  const verification = await verifyAdapterCompatibility({ compatibilityPath: options.compatibilityPath, schemaPath: options.compatibilitySchemaPath, adapterIds: config.adapterIds, requireEnabled: true });
  if (config.adapterIds.length > 0) {
    await (options.verifyNpmInstall ?? verifyNpmInstallAttestation)(options.agentsHome);
  }
  const provenanceByAdapter = new Map(verification.wrapperProvenance.map((item) => [item.adapterId, item]));
  const adapters = Object.fromEntries(await Promise.all(config.adapterIds.map(async (adapterId) => {
    const command = config.adapterCommands[adapterId];
    if (command === undefined || command.length === 0) throw new TypeError(`activated adapter ${adapterId} has no trusted command`);
    const policy = await loadAdapterModelConstraints({
      compatibilityPath: options.compatibilityPath,
      schemaPath: options.compatibilitySchemaPath,
      adapterId,
      requireEnabled: true,
    });
    if (policy.providerExecutable === undefined || policy.providerIdentity === undefined) {
      throw new TypeError(`${adapterId} compatibility entry has no mandatory provider identity`);
    }
    await (options.verifyProvider ?? verifyProviderConformance)({
      adapterId,
      executable: policy.providerExecutable,
      ...(policy.cursorInstallRoot === undefined ? {} : { cursorInstallRoot: policy.cursorInstallRoot }),
      ...(policy.providerInstallRoot === undefined ? {} : { providerInstallRoot: policy.providerInstallRoot }),
    });
    let resolvedCommand = command.map((part) => expandTrustedCommandPart(part, options.agentsHome, options.stateDirectory));
    if (policy.wrapperEntrypoint === undefined) throw new TypeError(`${adapterId} compatibility entry has no configured fabric wrapper`);
    const provenance = provenanceByAdapter.get(adapterId);
    if (provenance === undefined) throw new TypeError(`${adapterId} activation has no verified wrapper provenance`);
    const wrapperIndex = wrapperCommandEntrypointIndex(resolvedCommand);
    if (wrapperIndex === -1) throw new TypeError(`${adapterId} trusted command has no wrapper entrypoint`);
    resolvedCommand[wrapperIndex] = policy.wrapperEntrypoint;
    if (policy.wrapperEntrypoint.endsWith(".ts")) {
      const loaderIndex = resolvedCommand.indexOf("--import");
      const loader = loaderIndex === -1 ? undefined : resolvedCommand[loaderIndex + 1];
      if (loader === undefined || !loader.endsWith("/tsx/dist/loader.mjs") || !resolvedCommand.includes("--conditions=source")) {
        throw new TypeError(
          `${adapterId} TypeScript wrapper requires the tsx loader and --conditions=source so first-party code executes from tracked source`,
        );
      }
    }
    if (policy.providerExecutable !== undefined) {
      if (!isAbsolute(policy.providerExecutable)) throw new TypeError(`${adapterId} provider executable must be absolute`);
      resolvedCommand = replaceUniqueOption(resolvedCommand, "--provider-executable", policy.providerExecutable);
      if (policy.providerIdentity !== undefined) {
        resolvedCommand = replaceUniqueOption(resolvedCommand, "--provider-identity-policy", policy.providerIdentity);
      }
      if (policy.cursorInstallRoot !== undefined) {
        resolvedCommand = replaceUniqueOption(resolvedCommand, "--provider-install-root", policy.cursorInstallRoot);
      }
      if (policy.providerInstallRoot !== undefined) {
        resolvedCommand = replaceUniqueOption(resolvedCommand, "--provider-install-root", policy.providerInstallRoot);
      }
    } else if (adapterId !== "claude-agent-sdk") {
      throw new TypeError(`${adapterId} compatibility entry has no configured provider executable`);
    }
    return [adapterId, {
      command: resolvedCommand,
      environment: {},
      modelPolicy: {
        allowedFamilies: policy.allowed,
        allowedModelPatterns: policy.patterns,
        requiresExplicitModel: policy.requiresExplicitModel,
      },
      wrapperProvenance: {
        repositoryCommit: provenance.repositoryCommit,
        wrapperPath: provenance.wrapperPath,
      },
      npmInstallProductRoot: options.agentsHome,
    }] as const;
  })));
  return {
    adapters,
    executionProfile: config.executionProfile ?? "headless",
    maximumConcurrentProviderTurns: config.limits.maximumConcurrentProviderTurns,
    workspaceRoots: config.workspaceRoots,
  };
}

export async function composeDaemonAdapters(options: Parameters<typeof composeDaemonConfiguration>[0]): Promise<AdapterMap> {
  return (await composeDaemonConfiguration(options)).adapters;
}
