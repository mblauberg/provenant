import { execFile } from "node:child_process";
import { constants } from "node:fs";
import { access, readFile, stat } from "node:fs/promises";
import { basename, delimiter, dirname, isAbsolute, join, resolve } from "node:path";
import { homedir } from "node:os";
import { promisify } from "node:util";

import { Ajv2020 } from "ajv/dist/2020.js";
import { parse } from "yaml";

import { FabricError } from "../errors.js";
import { EXECUTABLE_RESOLUTION_VERSION } from "../domain/versions.js";
import { isPrimaryAdapter, PRIMARY_ADAPTER_IDS } from "./primary-adapters.js";

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
  const pathEntries = path.split(delimiter);
  const extensions = process.platform === "win32" && !name.includes(".")
    ? (process.env.PATHEXT ?? ".COM;.EXE;.BAT;.CMD").split(";")
    : [""];
  for (const entry of pathEntries) {
    const directory = entry.length === 0 ? "." : entry;
    for (const extension of extensions) {
      const candidate = isAbsolute(name) ? name : join(directory, `${name}${extension}`);
      if (await isExecutableFile(candidate)) return resolve(candidate);
    }
  }
  return undefined;
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
  compatibilityPath?: string;
  path?: string;
  lookup?: ExecutableLookup;
}): Promise<string> {
  const executable = input.compatibilityPath !== undefined && input.executable.startsWith("${USER_HOME}")
    ? resolveCompatibilityArtifact(input.compatibilityPath, input.executable)
    : input.executable;
  return resolveExecutableOnPath(executable, input);
}

export async function isExecutableFile(path: string): Promise<boolean> {
  try {
    const info = await stat(path);
    if (!info.isFile()) return false;
    await access(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

export type WrapperProvenance = {
  adapterId: string;
  repositoryCommit: string;
  wrapperPath: string;
};

export type AdapterExecutableFailure = {
  adapterId: string;
  executable: string;
  reasons: string[];
};

function executableFailure(adapterId: string, executable: string, reason: string): AdapterExecutableFailure {
  return { adapterId, executable, reasons: [reason] };
}

export function mergeAdapterExecutableFailures(
  ...groups: readonly (readonly AdapterExecutableFailure[])[]
): AdapterExecutableFailure[] {
  const merged = new Map<string, { executables: Set<string>; reasons: Set<string> }>();
  for (const failure of groups.flat()) {
    const current = merged.get(failure.adapterId) ?? { executables: new Set<string>(), reasons: new Set<string>() };
    current.executables.add(failure.executable);
    for (const reason of failure.reasons) current.reasons.add(reason);
    merged.set(failure.adapterId, current);
  }
  return [...merged.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([adapterId, value]) => ({
      adapterId,
      executable: [...value.executables].filter((executable) => executable !== "<missing>").sort()[0] ?? "<missing>",
      reasons: [...value.reasons].sort(),
    }));
}

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
    }
    const executable = adapter.implementation.executable;
    if (input.requireEnabled) {
      if (typeof executable !== "string") {
        if (input.allowUnavailableOptional === true && !isPrimaryAdapter(adapterId)) {
          unavailableOptionalAdapters.push(executableFailure(
            adapterId,
            "<missing>",
            `enabled adapter has no provider executable: ${adapterId}`,
          ));
          continue;
        }
        throw new FabricError(
          "ADAPTER_COMPATIBILITY_INVALID",
          `enabled adapter has no provider executable: ${adapterId}`,
        );
      }
    }
    if (typeof executable === "string" && (adapter.enabled === true || input.requireEnabled)) {
      try {
        const candidate = await resolveAdapterExecutable({ executable, compatibilityPath: input.compatibilityPath });
        if (!(await isExecutableFile(candidate))) {
          throw new FabricError("ADAPTER_ARTIFACT_MISSING", `adapter executable '${executable}' is not executable: ${adapterId}`);
        }
        resolvedExecutables[adapterId] = candidate;
      } catch (error: unknown) {
        if (input.allowUnavailableOptional === true && !isPrimaryAdapter(adapterId) && error instanceof FabricError && error.code === "ADAPTER_ARTIFACT_MISSING") {
          unavailableOptionalAdapters.push(executableFailure(
            adapterId,
            typeof executable === "string" ? executable : "<missing>",
            error.message,
          ));
        } else {
          throw error;
        }
      }
    }
    if (unavailableOptionalAdapters.some((failure) => failure.adapterId === adapterId)) continue;
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

export async function validateEnabledAdapterExecutables(input: {
  compatibilityPath: string;
  schemaPath?: string;
  activeAdapterIds?: readonly string[];
  adapterIds?: readonly string[];
  mandatoryPrimary?: boolean;
}): Promise<{
  valid: true;
  executableResolutionVersion: typeof EXECUTABLE_RESOLUTION_VERSION;
  resolvedExecutables: Record<string, string>;
  unavailableOptionalAdapters: AdapterExecutableFailure[];
}> {
  const document: unknown = parse(await readFile(input.compatibilityPath, "utf8"));
  if (input.schemaPath !== undefined) {
    const schema: unknown = JSON.parse(await readFile(input.schemaPath, "utf8"));
    if (!isRecord(schema)) throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "compatibility schema is not an object");
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    if (!ajv.validate(schema, document)) {
      throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", `compatibility registry does not match published schema: ${ajv.errorsText(ajv.errors)}`);
    }
  }
  if (!isRecord(document) || !isRecord(document.adapters)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "compatibility registry lacks adapters");
  }
  const explicitSelection = input.adapterIds !== undefined;
  const selected = new Set(input.adapterIds ?? Object.keys(document.adapters));
  if (input.mandatoryPrimary === true) {
    for (const adapterId of PRIMARY_ADAPTER_IDS) selected.add(adapterId);
  }
  const resolvedExecutables: Record<string, string> = {};
  const failures: AdapterExecutableFailure[] = [];
  for (const adapterId of selected) {
    if (document.adapters[adapterId] === undefined) {
      failures.push(executableFailure(adapterId, "<missing>", `adapter compatibility entry is missing: ${adapterId}`));
    }
  }
  for (const [adapterId, value] of Object.entries(document.adapters)) {
    if (!selected.has(adapterId) || !isRecord(value)) continue;
    const primaryMandatory = input.mandatoryPrimary === true && isPrimaryAdapter(adapterId);
    const selectedDisabledOptional = explicitSelection && value.enabled !== true && !primaryMandatory;
    if (value.enabled !== true && !selectedDisabledOptional && !primaryMandatory) continue;
    if (primaryMandatory && value.enabled !== true) {
      failures.push(executableFailure(adapterId, "<disabled>", `mandatory primary adapter is disabled: ${adapterId}`));
      continue;
    }
    if (!isRecord(value.implementation)) {
      failures.push(executableFailure(adapterId, "<missing>", `selected adapter has no provider executable: ${adapterId}`));
      continue;
    }
    const executable = value.implementation.executable;
    if (typeof executable !== "string") {
      failures.push(executableFailure(adapterId, "<missing>", `selected adapter has no provider executable: ${adapterId}`));
      continue;
    }
    let candidate: string;
    try {
      candidate = await resolveAdapterExecutable({ executable, compatibilityPath: input.compatibilityPath });
    } catch (error: unknown) {
      if (!(error instanceof FabricError) || error.code !== "ADAPTER_ARTIFACT_MISSING") throw error;
      failures.push(executableFailure(
        adapterId,
        executable,
        `adapter ${adapterId} is enabled but executable '${executable}' is not resolvable on PATH`,
      ));
      continue;
    }
    if (candidate === undefined || !(await isExecutableFile(candidate))) {
      failures.push(executableFailure(adapterId, executable, `adapter ${adapterId} is enabled but executable '${executable}' is not resolvable on PATH`));
      continue;
    }
    resolvedExecutables[adapterId] = candidate;
  }
  const mandatoryIds = new Set(input.activeAdapterIds ?? PRIMARY_ADAPTER_IDS);
  const primaryFailures = failures.filter((failure) => mandatoryIds.has(failure.adapterId) || (input.mandatoryPrimary === true && isPrimaryAdapter(failure.adapterId)));
  if (primaryFailures.length > 0) {
    throw new FabricError("ADAPTER_ARTIFACT_MISSING", ["enabled adapter executable validation failed:", ...failures.flatMap((failure) => failure.reasons.map((reason) => `- ${reason}`))].join("\n"));
  }
  return { valid: true, executableResolutionVersion: EXECUTABLE_RESOLUTION_VERSION, resolvedExecutables, unavailableOptionalAdapters: mergeAdapterExecutableFailures(failures) };
}
