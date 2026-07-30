import { readFile } from "node:fs/promises";
import { existsSync, realpathSync } from "node:fs";
import { isAbsolute, relative, resolve } from "node:path";
import { basename, dirname } from "node:path";

import type { ErrorObject, ValidateFunction } from "ajv";
import { Ajv2020 } from "ajv/dist/2020.js";
import { parse } from "yaml";

import { FabricError } from "../errors.js";

type ConfigDocument = {
  schemaVersion: number;
  adapters?: Record<string, { command?: string[] }>;
  allowedAdapters?: string[];
  activeAdapters?: string[];
  allowedProfiles?: string[];
  workspaceRoots?: string[];
  limits?: { maximumConcurrentProviderTurns?: number };
  environmentSources?: string[];
  listener?: string;
  providerCredentialSelector?: string;
  namedExecutionProfile?: string;
  allowListedAdapterId?: string;
};

export type ResolvedFabricConfig = {
  schemaVersion: 1;
  executionProfile?: string;
  adapterIds: string[];
  workspaceRoots: string[];
  limits: { maximumConcurrentProviderTurns: number };
  adapterCommands: Record<string, string[]>;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

type ConfigValidators = {
  trusted: ValidateFunction<ConfigDocument>;
  untrusted: ValidateFunction<ConfigDocument>;
};

type ConfigLayer = "global" | "local" | "project" | "run";

let configValidators: Promise<ConfigValidators> | undefined;

async function loadConfigValidators(): Promise<ConfigValidators> {
  configValidators ??= (async () => {
    const candidates = [
      new URL("../../schemas/config.schema.json", import.meta.url),
      new URL("../../../schemas/config.schema.json", import.meta.url),
    ];
    const schemaUrl = candidates.find((candidate) => existsSync(candidate));
    if (schemaUrl === undefined) {
      throw new Error("published config.schema.json is unavailable");
    }
    const schema: unknown = JSON.parse(await readFile(schemaUrl, "utf8"));
    if (!isRecord(schema) || typeof schema.$id !== "string") {
      throw new TypeError("published config.schema.json must be an identified object schema");
    }
    const ajv = new Ajv2020({ allErrors: true, strict: true, validateFormats: false });
    ajv.addSchema(schema);
    const trusted = ajv.getSchema<ConfigDocument>(schema.$id);
    const untrusted = ajv.getSchema<ConfigDocument>(`${schema.$id}#/$defs/projectConfig`);
    if (trusted === undefined || untrusted === undefined) {
      throw new TypeError("published config.schema.json must define global and project configuration schemas");
    }
    return { trusted, untrusted };
  })();
  return configValidators;
}

function escapePointerSegment(value: string): string {
  return value.replaceAll("~", "~0").replaceAll("/", "~1");
}

function displayInstancePath(path: string): string | undefined {
  if (path === "") return undefined;
  const topLevel = /^\/([^/]+)$/u.exec(path);
  return topLevel?.[1]?.replaceAll("~1", "/").replaceAll("~0", "~") ?? path;
}

function validationField(error: ErrorObject | undefined): string | undefined {
  if (error === undefined) return undefined;
  if (error.keyword === "additionalProperties" && typeof error.params.additionalProperty === "string") {
    const property = escapePointerSegment(error.params.additionalProperty);
    return error.instancePath === "" ? property : `${error.instancePath}/${property}`;
  }
  return displayInstancePath(error.instancePath);
}

async function parseDocument(path: string): Promise<unknown> {
  return parse(await readFile(path, "utf8"));
}

async function validateDocument(value: unknown, layer: ConfigLayer): Promise<ConfigDocument> {
  const validators = await loadConfigValidators();
  const validate = layer === "global" || layer === "local" ? validators.trusted : validators.untrusted;
  if (!validate(value)) {
    const error = validate.errors?.[0];
    const field = validationField(error);
    throw new FabricError(
      "CONFIG_UNTRUSTED_FIELD",
      `${layer} configuration does not match published config.schema.json${error?.message === undefined ? "" : `: ${error.message}`}`,
      { ...(field === undefined ? {} : { field }), cause: validate.errors },
    );
  }
  return value;
}

function rejectTrustedFields(document: ConfigDocument, layer: "project" | "run"): void {
  const candidates = [
    ["adapters", document.adapters],
    ["environmentSources", document.environmentSources],
    ["listener", document.listener],
    ["providerCredentialSelector", document.providerCredentialSelector],
  ] as const;
  for (const [field, value] of candidates) {
    if (value !== undefined) {
      if (field === "adapters" && isRecord(value)) {
        const adapter = Object.entries(value).find(([, entry]) => isRecord(entry) && entry.command !== undefined);
        if (adapter !== undefined) {
          throw new FabricError("CONFIG_UNTRUSTED_FIELD", `${layer} configuration cannot select an adapter command`, {
            field: `adapters.${adapter[0]}.command`,
          });
        }
      }
      throw new FabricError("CONFIG_UNTRUSTED_FIELD", `${layer} configuration cannot set ${field}`, { field });
    }
  }
}

function canonicalConfigPath(path: string): string {
  let cursor = resolve(path);
  const suffix: string[] = [];
  while (!existsSync(cursor)) {
    const parent = dirname(cursor);
    if (parent === cursor) {
      return resolve(path);
    }
    suffix.unshift(basename(cursor));
    cursor = parent;
  }
  return resolve(realpathSync(cursor), ...suffix);
}

function pathContained(child: string, parent: string): boolean {
  const parentPath = canonicalConfigPath(parent);
  const childPath = canonicalConfigPath(child);
  const rel = relative(parentPath, childPath);
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

function expandTrustedWorkspaceRoots(config: ConfigDocument, agentsHome: string | undefined): ConfigDocument {
  const token = "${AGENTS_HOME}";
  if (config.workspaceRoots === undefined || !config.workspaceRoots.some((path) => path.includes(token))) {
    return config;
  }
  if (agentsHome === undefined) {
    throw new FabricError("CONFIG_UNTRUSTED_FIELD", "trusted workspace root uses ${AGENTS_HOME} without an agents home");
  }
  return {
    ...config,
    workspaceRoots: config.workspaceRoots.map((path) => {
      if (!path.includes(token)) return path;
      if (
        path !== token &&
        !path.startsWith(`${token}/`) &&
        !path.startsWith(`${token}\\`)
      ) {
        throw new FabricError("CONFIG_UNTRUSTED_FIELD", "${AGENTS_HOME} must begin a trusted workspace root");
      }
      return `${agentsHome}${path.slice(token.length)}`;
    }),
  };
}

/**
 * The instance layer selects from the product's adapters; it never defines one.
 *
 * ADR 0019 makes the instance layer narrowing-only, and an adapter entry is the
 * one place a trusted layer names an executable. Overlaying a local entry would
 * let the instance substitute the command behind an adapter the product
 * activated, which is widening of the most consequential kind: the same adapter
 * id, the same allow-list, a different program. Restating the product's exact
 * entry is allowed, so an instance file may be authored as a narrowed copy of
 * the product's.
 */
function rejectAdapterCommandOverride(
  globalConfig: ConfigDocument,
  localConfig: ConfigDocument,
): void {
  if (localConfig.adapters === undefined) return;
  for (const [adapterId, entry] of Object.entries(localConfig.adapters)) {
    const shipped = globalConfig.adapters?.[adapterId];
    if (shipped === undefined) {
      throw new FabricError(
        "CONFIG_WIDENING_FORBIDDEN",
        `local configuration introduced an adapter the product does not define: ${adapterId}`,
        { field: `adapters.${adapterId}` },
      );
    }
    if (JSON.stringify(entry) !== JSON.stringify(shipped)) {
      throw new FabricError(
        "CONFIG_WIDENING_FORBIDDEN",
        `local configuration altered a product adapter command: ${adapterId}`,
        { field: `adapters.${adapterId}.command` },
      );
    }
  }
}

function mergeTrustedConfig(globalConfig: ConfigDocument, localConfig: ConfigDocument | undefined): ConfigDocument {
  if (localConfig === undefined) return globalConfig;
  rejectAdapterCommandOverride(globalConfig, localConfig);
  const intersect = (globalValues: string[] | undefined, localValues: string[] | undefined): string[] | undefined => {
    if (globalValues === undefined) return localValues === undefined ? undefined : [];
    if (localValues === undefined) return globalValues;
    const allowed = new Set(localValues);
    return globalValues.filter((value) => allowed.has(value));
  };
  let workspaceRoots = globalConfig.workspaceRoots;
  if (localConfig.workspaceRoots !== undefined) {
    const globalRoots = globalConfig.workspaceRoots ?? [];
    const widened = localConfig.workspaceRoots.find(
      (candidate) => !globalRoots.some((root) => pathContained(candidate, root)),
    );
    if (widened !== undefined) {
      throw new FabricError("CONFIG_WIDENING_FORBIDDEN", `local configuration widened workspace root: ${widened}`);
    }
    workspaceRoots = localConfig.workspaceRoots;
  }
  const globalLimit = globalConfig.limits?.maximumConcurrentProviderTurns ?? 8;
  const localLimit = localConfig.limits?.maximumConcurrentProviderTurns ?? globalLimit;
  const allowedAdapters = intersect(globalConfig.allowedAdapters, localConfig.allowedAdapters);
  const activeAdapters = intersect(globalConfig.activeAdapters, localConfig.activeAdapters);
  const allowedProfiles = intersect(globalConfig.allowedProfiles, localConfig.allowedProfiles);
  return {
    ...globalConfig,
    ...localConfig,
    // The product owns the adapter map outright; the guard above has already
    // proved the local layer neither added to it nor changed it.
    ...(globalConfig.adapters === undefined ? {} : { adapters: globalConfig.adapters }),
    ...(allowedAdapters === undefined ? {} : { allowedAdapters }),
    ...(activeAdapters === undefined ? {} : { activeAdapters }),
    ...(allowedProfiles === undefined ? {} : { allowedProfiles }),
    ...(workspaceRoots === undefined ? {} : { workspaceRoots }),
    limits: { maximumConcurrentProviderTurns: Math.min(globalLimit, localLimit) },
  };
}

type NarrowingState = {
  allowedProfiles: string[];
  executionProfile?: string;
  adapterIds: string[];
  workspaceRoots: string[];
  maximumConcurrentProviderTurns: number;
};

function applyUntrustedLayer(
  state: NarrowingState,
  config: ConfigDocument,
  layer: "project" | "run",
): void {
  if (config.namedExecutionProfile !== undefined) {
    if (
      !state.allowedProfiles.includes(config.namedExecutionProfile) ||
      (state.executionProfile !== undefined && state.executionProfile !== config.namedExecutionProfile)
    ) {
      throw new FabricError(
        "CONFIG_WIDENING_FORBIDDEN",
        `${layer} selected a profile outside ${layer === "project" ? "the allow-list" : "the current allow-list"}`,
      );
    }
    state.executionProfile = config.namedExecutionProfile;
  }
  if (config.allowListedAdapterId !== undefined) {
    if (!state.adapterIds.includes(config.allowListedAdapterId)) {
      throw new FabricError(
        "CONFIG_WIDENING_FORBIDDEN",
        `${layer} selected an adapter outside ${layer === "project" ? "the allow-list" : "the current allow-list"}`,
      );
    }
    state.adapterIds = [config.allowListedAdapterId];
  }
  if (config.workspaceRoots !== undefined) {
    const roots = config.workspaceRoots.map(canonicalConfigPath);
    if (roots.some((path) => !state.workspaceRoots.some((root) => pathContained(path, root)))) {
      throw new FabricError("CONFIG_WIDENING_FORBIDDEN", `${layer} widened a workspace root`);
    }
    state.workspaceRoots = roots;
  }
  const limit = config.limits?.maximumConcurrentProviderTurns;
  if (limit !== undefined) {
    if (!Number.isInteger(limit) || limit < 0 || limit > state.maximumConcurrentProviderTurns) {
      throw new FabricError("CONFIG_WIDENING_FORBIDDEN", `${layer} widened a numeric limit`);
    }
    state.maximumConcurrentProviderTurns = Math.min(state.maximumConcurrentProviderTurns, limit);
  }
}

export async function loadFabricConfig(options: {
  globalPath: string;
  localPath?: string;
  projectPath?: string;
  runPath?: string;
  agentsHome?: string;
  additionalWorkspaceRoots?: string[];
}): Promise<ResolvedFabricConfig> {
  const globalConfig = expandTrustedWorkspaceRoots(
    await validateDocument(await parseDocument(options.globalPath), "global"),
    options.agentsHome,
  );
  const localConfig =
    options.localPath === undefined
      ? undefined
      : expandTrustedWorkspaceRoots(
          await validateDocument(await parseDocument(options.localPath), "local"),
          options.agentsHome,
        );
  const trustedConfig = mergeTrustedConfig(globalConfig, localConfig);
  const allowedAdapters = trustedConfig.allowedAdapters ?? [];
  const activeAdapters = trustedConfig.activeAdapters ?? [];
  const disallowedActiveAdapter = activeAdapters.find((adapterId) => !allowedAdapters.includes(adapterId));
  if (disallowedActiveAdapter !== undefined) {
    throw new FabricError(
      "CONFIG_WIDENING_FORBIDDEN",
      `trusted configuration activated adapter outside the allow-list: ${disallowedActiveAdapter}`,
    );
  }
  const state: NarrowingState = {
    allowedProfiles: trustedConfig.allowedProfiles ?? [],
    adapterIds: activeAdapters,
    workspaceRoots: (trustedConfig.workspaceRoots ?? []).map(canonicalConfigPath),
    maximumConcurrentProviderTurns: trustedConfig.limits?.maximumConcurrentProviderTurns ?? 8,
  };
  state.workspaceRoots = [...new Set([
    ...state.workspaceRoots,
    ...(options.additionalWorkspaceRoots ?? []).map(canonicalConfigPath),
  ])].sort();

  for (const [layer, path] of [
    ["project", options.projectPath],
    ["run", options.runPath],
  ] as const) {
    if (path === undefined) continue;
    const value = await parseDocument(path);
    if (isRecord(value)) rejectTrustedFields(value as ConfigDocument, layer);
    applyUntrustedLayer(state, await validateDocument(value, layer), layer);
  }

  return {
    schemaVersion: 1,
    ...(state.executionProfile === undefined ? {} : { executionProfile: state.executionProfile }),
    adapterIds: state.adapterIds,
    workspaceRoots: state.workspaceRoots.map(canonicalConfigPath),
    limits: { maximumConcurrentProviderTurns: state.maximumConcurrentProviderTurns },
    adapterCommands: Object.fromEntries(
      state.adapterIds.flatMap((adapterId) => {
        const command = trustedConfig.adapters?.[adapterId]?.command;
        return command === undefined ? [] : [[adapterId, command] as const];
      }),
    ),
  };
}
