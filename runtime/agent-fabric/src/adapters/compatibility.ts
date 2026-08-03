import { execFile } from "node:child_process";
import { constants } from "node:fs";
import { access, readFile, realpath, stat } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, resolve, sep } from "node:path";
import { homedir } from "node:os";
import { promisify } from "node:util";

import { Ajv2020 } from "ajv/dist/2020.js";
import { parse } from "yaml";

import { FabricError } from "../errors.js";
import { isMandatoryPrimaryAdapter, isPrimaryAdapter, PRIMARY_ADAPTER_IDS } from "./primary-adapters.js";

export const EXECUTABLE_RESOLUTION_VERSION = 2 as const;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function resolveCompatibilityArtifact(compatibilityPath: string, value: string): string {
  const userHomeToken = "${USER_HOME}";
  if (value === userHomeToken || value.startsWith(`${userHomeToken}/`)) {
    return resolve(homedir(), value.slice(userHomeToken.length + 1));
  }
  if (value.includes(userHomeToken)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "${USER_HOME} must begin a compatibility artifact path");
  }
  if (isAbsolute(value)) return value;
  return resolve(dirname(compatibilityPath), "..", value);
}

const execFileAsync = promisify(execFile);

type ExecutableLookup = (name: string, path: string) => Promise<string | undefined>;

async function defaultExecutableLookup(name: string, path: string): Promise<string | undefined> {
  try {
    const { stdout } = await execFileAsync("which", [name], { env: { ...process.env, PATH: path } });
    return stdout.trim() || undefined;
  } catch {
    return undefined;
  }
}

export async function resolveExecutableOnPath(
  executable: string,
  dependencies: { path?: string; lookup?: ExecutableLookup } = {},
): Promise<string> {
  if (isAbsolute(executable)) return executable;
  const path = dependencies.path ?? process.env.PATH ?? "";
  const resolved = await (dependencies.lookup ?? defaultExecutableLookup)(executable, path);
  if (resolved === undefined) {
    throw new FabricError(
      "ADAPTER_ARTIFACT_MISSING",
      `adapter executable '${executable}' is not resolvable on PATH`,
    );
  }
  return resolved;
}

export async function resolveAdapterExecutable(input: {
  executable: string;
  executableOverride?: string;
  compatibilityPath?: string;
  path?: string;
  lookup?: ExecutableLookup;
}): Promise<string> {
  if (input.executableOverride !== undefined) {
    if (!isAbsolute(input.executableOverride)) {
      throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "executable_override must be an absolute path");
    }
    return input.executableOverride;
  }
  const executable = input.compatibilityPath !== undefined && input.executable.startsWith("${USER_HOME}")
    ? resolveCompatibilityArtifact(input.compatibilityPath, input.executable)
    : input.executable;
  return resolveExecutableOnPath(executable, input);
}

async function isExecutableFile(path: string): Promise<boolean> {
  try {
    await access(path, constants.X_OK);
    const metadata = await stat(path);
    if (!metadata.isFile()) return false;
    return true;
  } catch {
    return false;
  }
}

async function isRegularFile(path: string): Promise<boolean> {
  try {
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}

function usesExecutableResolutionRevision(document: Record<string, unknown>): boolean {
  if (!isRecord(document.adapters)) return false;
  return Object.values(document.adapters).some((value) => {
    if (!isRecord(value) || !isRecord(value.implementation)) return false;
    const executable = value.implementation.executable;
    const providerInstallRoot = value.implementation.provider_install_root;
    return typeof executable === "string" && !isAbsolute(executable)
      || providerInstallRoot === "${EXECUTABLE_ROOT}";
  });
}

function requireExecutableResolutionRevision(document: Record<string, unknown>): void {
  if (!usesExecutableResolutionRevision(document)) return;
  const policy = document.activation_policy;
  const version = isRecord(policy) ? policy.executable_resolution_version : undefined;
  if (version !== EXECUTABLE_RESOLUTION_VERSION) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      "PATH names and ${EXECUTABLE_ROOT} require executable_resolution_version: 2; restart the daemon after upgrading",
    );
  }
}

export async function resolveProviderInstallRoot(input: {
  adapterId: string;
  configuredRoot: string;
  executable: string;
  compatibilityPath: string;
}): Promise<string> {
  if (input.configuredRoot !== "${EXECUTABLE_ROOT}") {
    return resolveCompatibilityArtifact(input.compatibilityPath, input.configuredRoot);
  }
  if (input.adapterId !== "opencode-acp") {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `executable-derived provider install root is only supported for opencode-acp: ${input.adapterId}`,
    );
  }
  let executable: string;
  try {
    executable = await realpath(input.executable);
  } catch (error: unknown) {
    throw new FabricError("ADAPTER_ARTIFACT_MISSING", `provider executable is unavailable: ${input.executable}`, { cause: error });
  }
  const components = executable.split(sep);
  const cellarIndex = components.lastIndexOf("Cellar");
  if (cellarIndex >= 0 && components[cellarIndex + 1] !== undefined && components[cellarIndex + 2] !== undefined) {
    return join(sep, ...components.slice(0, cellarIndex + 2));
  }
  return dirname(executable);
}

export type WrapperProvenance = {
  adapterId: string;
  repositoryCommit: string;
  wrapperPath: string;
};

const PROVIDER_IDENTITY_POLICY: Readonly<Record<string, string>> = {
  "claude-agent-sdk": "apple-designated",
  "codex-app-server": "apple-designated",
  agy: "apple-designated",
  "cursor-agent": "cursor-partial-signed-helpers",
  "kiro-acp": "apple-designated",
  "opencode-acp": "owner-controlled-install-root",
};

async function gitOutput(directory: string, args: string[]): Promise<string> {
  const { stdout } = await execFileAsync("git", ["-C", directory, ...args]);
  return stdout.trim();
}

async function verifyWrapperTrackedAndClean(input: {
  adapterId: string;
  wrapperDirectory: string;
  wrapperArgument: string;
}): Promise<string> {
  const { adapterId, wrapperDirectory, wrapperArgument } = input;
  let wrapperPath: string;
  try {
    wrapperPath = await gitOutput(wrapperDirectory, ["ls-files", "--full-name", "--error-unmatch", "--", wrapperArgument]);
  } catch (error: unknown) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `wrapper entrypoint is not tracked at the repository HEAD: ${adapterId} (${wrapperArgument})`,
      { cause: error },
    );
  }
  try {
    await gitOutput(wrapperDirectory, ["diff", "--quiet", "HEAD", "--", wrapperArgument]);
  } catch (error: unknown) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `wrapper entrypoint differs from its committed content: ${adapterId} (${wrapperPath})`,
      { cause: error },
    );
  }
  return wrapperPath;
}

/**
 * Derives provenance from the Git repository containing the configured wrapper
 * entrypoint. The wrapper must be tracked and clean against HEAD; its owning
 * package, source spans, TypeScript configuration and dependency symlinks are
 * intentionally outside this compatibility check.
 */
async function deriveWrapperProvenance(input: {
  adapterId: string;
  wrapperEntrypoint: string;
}): Promise<WrapperProvenance> {
  const configuredWrapperPath = resolve(input.wrapperEntrypoint);
  const wrapperDirectory = dirname(configuredWrapperPath);
  const wrapperArgument = `./${basename(configuredWrapperPath)}`;
  let repositoryRoot: string;
  let repositoryCommit: string;
  try {
    repositoryRoot = resolve(await gitOutput(wrapperDirectory, ["rev-parse", "--show-toplevel"]));
    repositoryCommit = await gitOutput(repositoryRoot, ["rev-parse", "HEAD"]);
  } catch (error: unknown) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `wrapper entrypoint has no Git repository provenance: ${input.adapterId}`,
      { cause: error },
    );
  }
  const wrapperPath = await verifyWrapperTrackedAndClean({
    adapterId: input.adapterId,
    wrapperDirectory,
    wrapperArgument,
  });
  return {
    adapterId: input.adapterId,
    repositoryCommit,
    wrapperPath,
  };
}

const VALUE_TAKING_NODE_OPTIONS = new Set(["--import", "--require", "--loader", "--experimental-loader", "--conditions"]);

/**
 * Index of the wrapper entrypoint inside a trusted adapter command: the
 * first argument after the executable that is not a runtime option or the
 * value of one (for example the tsx loader after --import).
 */
export function wrapperCommandEntrypointIndex(command: string[]): number {
  let index = 1;
  while (index < command.length) {
    const part = command[index] ?? "";
    if (part.startsWith("--")) {
      index += !part.includes("=") && VALUE_TAKING_NODE_OPTIONS.has(part) ? 2 : 1;
      continue;
    }
    return index;
  }
  return -1;
}

/**
 * Re-checks the wrapper's tracked-and-clean Git provenance immediately before an
 * adapter process spawn and requires the composition identity to match.
 */
export async function verifySpawnWrapperProvenance(input: {
  adapterId: string;
  command: string[];
  expected: { repositoryCommit: string; wrapperPath: string };
}): Promise<void> {
  const index = wrapperCommandEntrypointIndex(input.command);
  const entrypoint = index === -1 ? undefined : input.command[index];
  if (entrypoint === undefined) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", `adapter command has no wrapper entrypoint: ${input.adapterId}`);
  }
  const provenance = await deriveWrapperProvenance({ adapterId: input.adapterId, wrapperEntrypoint: entrypoint });
  if (provenance.repositoryCommit !== input.expected.repositoryCommit || provenance.wrapperPath !== input.expected.wrapperPath) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `wrapper provenance changed since activation composition: ${input.adapterId}`,
    );
  }
}

export async function verifyAdapterCompatibility(input: {
  compatibilityPath: string;
  schemaPath: string;
  adapterIds: string[];
  requireEnabled: boolean;
  allowUnavailableOptional?: boolean;
}): Promise<{
  valid: true;
  adapterIds: string[];
  wrapperProvenance: WrapperProvenance[];
  resolvedExecutables: Record<string, string>;
  unavailableOptionalAdapters: AdapterExecutableFailure[];
}> {
  const document: unknown = parse(await readFile(input.compatibilityPath, "utf8"));
  const schema: unknown = JSON.parse(await readFile(input.schemaPath, "utf8"));
  if (!isRecord(schema)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "compatibility schema is not an object");
  }
  const ajv = new Ajv2020({ allErrors: true, strict: true });
  if (!ajv.validate(schema, document)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", ajv.errorsText(ajv.errors));
  }
  if (!isRecord(document) || !isRecord(document.adapters)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "compatibility registry lacks adapters");
  }
  requireExecutableResolutionRevision(document);

  const wrapperProvenance: WrapperProvenance[] = [];
  const resolvedExecutables: Record<string, string> = {};
  const unavailableOptionalAdapters: AdapterExecutableFailure[] = [];
  for (const adapterId of input.adapterIds) {
    const adapter = document.adapters[adapterId];
    if (!isRecord(adapter)) {
      throw new FabricError("NOT_FOUND", `adapter compatibility entry is missing: ${adapterId}`);
    }
    if (input.requireEnabled && adapter.enabled !== true) {
      throw new FabricError("ADAPTER_DISABLED", `adapter is not activated: ${adapterId}`);
    }
    if (!isRecord(adapter.implementation) || !isRecord(adapter.contract)) {
      throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", `adapter entry is incomplete: ${adapterId}`);
    }
    if (input.requireEnabled && typeof adapter.implementation.wrapper_entrypoint !== "string") {
      throw new FabricError(
        "ADAPTER_COMPATIBILITY_INVALID",
        `enabled adapter has no configured fabric wrapper: ${adapterId}`,
      );
    }
    if (input.requireEnabled) {
      const expectedIdentity = PROVIDER_IDENTITY_POLICY[adapterId];
      if (expectedIdentity !== undefined && adapter.implementation.provider_identity !== expectedIdentity) {
        throw new FabricError(
          "ADAPTER_COMPATIBILITY_INVALID",
          `enabled adapter has the wrong provider identity policy: ${adapterId}`,
        );
      }
      if (adapterId === "cursor-agent" && typeof adapter.implementation.cursor_install_root !== "string") {
        throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "enabled Cursor adapter has no canonical install root");
      }
      if (adapterId === "opencode-acp" && typeof adapter.implementation.provider_install_root !== "string") {
        throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "enabled OpenCode adapter has no canonical install root");
      }
    }
    const executable = adapter.implementation.executable;
    const executableOverride = adapter.implementation.executable_override;
    if (input.requireEnabled) {
      if (typeof executable !== "string") {
        throw new FabricError(
          "ADAPTER_COMPATIBILITY_INVALID",
          `enabled adapter has no provider executable: ${adapterId}`,
        );
      }
      if (executableOverride !== undefined && typeof executableOverride !== "string") {
        throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", `adapter executable_override is invalid: ${adapterId}`);
      }
    }
    try {
      if (typeof executable === "string" && (adapter.enabled === true || input.requireEnabled)) {
        const resolvedExecutable = await resolveAdapterExecutable({
          executable,
          ...(typeof executableOverride === "string" ? { executableOverride } : {}),
          compatibilityPath: input.compatibilityPath,
        });
        if (!(await isRegularFile(resolvedExecutable))) {
          throw new FabricError(
            "ADAPTER_ARTIFACT_MISSING",
            `adapter ${adapterId} is enabled but executable '${executable}' is not resolvable on PATH`,
          );
        }
        resolvedExecutables[adapterId] = resolvedExecutable;
      }
    } catch (error: unknown) {
      if (input.allowUnavailableOptional === true && !isPrimaryAdapter(adapterId) && error instanceof FabricError && error.code === "ADAPTER_ARTIFACT_MISSING") {
        unavailableOptionalAdapters.push({
          adapterId,
          executable: typeof executable === "string" ? executable : "<missing>",
          reason: error.message,
        });
        continue;
      }
      throw error;
    }
    const wrapperEntrypoint = adapter.implementation.wrapper_entrypoint;
    if (typeof wrapperEntrypoint === "string") {
      wrapperProvenance.push(await deriveWrapperProvenance({
        adapterId,
        wrapperEntrypoint: resolveCompatibilityArtifact(input.compatibilityPath, wrapperEntrypoint),
      }));
    }
  }
  return {
    valid: true,
    adapterIds: [...input.adapterIds],
    wrapperProvenance,
    resolvedExecutables,
    unavailableOptionalAdapters,
  };
}

export type AdapterExecutableFailure = {
  adapterId: string;
  executable: string;
  reason: string;
};

export type AdapterExecutableValidationReport = {
  valid: true;
  resolvedExecutables: Record<string, string>;
  unavailableOptionalAdapters: AdapterExecutableFailure[];
};

function executableFailureReason(adapterId: string, executable: string): string {
  return `adapter ${adapterId} is enabled but executable '${executable}' is not resolvable on PATH`;
}

export async function validateEnabledAdapterExecutables(input: {
  compatibilityPath: string;
  schemaPath?: string;
  activeAdapterIds?: readonly string[];
  adapterIds?: readonly string[];
}): Promise<AdapterExecutableValidationReport> {
  const document: unknown = parse(await readFile(input.compatibilityPath, "utf8"));
  if (input.schemaPath !== undefined) {
    const schema: unknown = JSON.parse(await readFile(input.schemaPath, "utf8"));
    if (!isRecord(schema)) {
      throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "compatibility schema is not an object");
    }
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    if (!ajv.validate(schema, document)) {
      throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", `compatibility registry does not match published schema: ${ajv.errorsText(ajv.errors)}`);
    }
  }
  if (!isRecord(document) || !isRecord(document.adapters)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "compatibility registry lacks adapters");
  }
  const adapters = document.adapters;
  requireExecutableResolutionRevision(document);
  const resolvedExecutables: Record<string, string> = {};
  const failures: AdapterExecutableFailure[] = [];
  const adapterEntries = input.adapterIds === undefined
    ? Object.entries(adapters)
    : input.adapterIds.map((adapterId) => [adapterId, adapters[adapterId]] as const);
  for (const [adapterId, value] of adapterEntries) {
    if (!isRecord(value) || value.enabled !== true) continue;
    if (!isRecord(value.implementation)) {
      failures.push({
        adapterId,
        executable: "<missing>",
        reason: `enabled adapter has no provider executable: ${adapterId}`,
      });
      continue;
    }
    const executable = value.implementation.executable;
    const executableOverride = value.implementation.executable_override;
    if (typeof executable !== "string") {
      failures.push({
        adapterId,
        executable: "<missing>",
        reason: `enabled adapter has no provider executable: ${adapterId}`,
      });
      continue;
    }
    if (executableOverride !== undefined && typeof executableOverride !== "string") {
      failures.push({
        adapterId,
        executable,
        reason: `adapter executable_override is invalid: ${adapterId}`,
      });
      continue;
    }
    try {
      const candidate = await resolveAdapterExecutable({
        executable,
        ...(typeof executableOverride === "string" ? { executableOverride } : {}),
        compatibilityPath: input.compatibilityPath,
      });
      if (!(await isExecutableFile(candidate))) {
        throw new FabricError("ADAPTER_ARTIFACT_MISSING", executableFailureReason(adapterId, executable));
      }
      resolvedExecutables[adapterId] = candidate;
    } catch (error: unknown) {
      if (error instanceof FabricError && error.code === "ADAPTER_COMPATIBILITY_INVALID") {
        throw error;
      }
      failures.push({ adapterId, executable, reason: executableFailureReason(adapterId, executable) });
    }
  }
  const activeAdapterIds = input.activeAdapterIds ?? PRIMARY_ADAPTER_IDS;
  const primaryFailures = failures.filter((failure) => isMandatoryPrimaryAdapter(failure.adapterId, activeAdapterIds));
  if (primaryFailures.length > 0) {
    throw new FabricError(
      "ADAPTER_ARTIFACT_MISSING",
      ["enabled adapter executable validation failed:", ...failures.map((failure) => `- ${failure.reason}`)].join("\n"),
    );
  }
  return {
    valid: true,
    resolvedExecutables,
    unavailableOptionalAdapters: failures,
  };
}
