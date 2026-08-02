import { readFile } from "node:fs/promises";

import { parse } from "yaml";

import { FabricError } from "../errors.js";
import { resolveAdapterExecutable, resolveCompatibilityArtifact, verifyAdapterCompatibility } from "./compatibility.js";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "adapter model constraints are invalid");
  }
  return value;
}

export async function loadAdapterModelConstraints(input: {
  compatibilityPath: string;
  schemaPath: string;
  adapterId: string;
  requireEnabled?: boolean;
  resolveExecutables?: boolean;
}): Promise<{ enabled: boolean; allowed: string[]; patterns: string[]; requiresExplicitModel: boolean; wrapperEntrypoint?: string; providerExecutable?: string; cursorInstallRoot?: string; providerInstallRoot?: string; providerIdentity?: string }> {
  await verifyAdapterCompatibility({
    compatibilityPath: input.compatibilityPath,
    schemaPath: input.schemaPath,
    adapterIds: [input.adapterId],
    requireEnabled: input.requireEnabled ?? true,
    resolveExecutables: false,
  });
  const document: unknown = parse(await readFile(input.compatibilityPath, "utf8"));
  if (!isRecord(document) || !isRecord(document.adapters)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "adapter compatibility entry is missing");
  }
  const adapter = document.adapters[input.adapterId];
  if (!isRecord(adapter)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "adapter compatibility entry is missing");
  }
  if (!isRecord(adapter.model_family_constraints)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "adapter model constraints are missing");
  }
  const implementation = isRecord(adapter.implementation) ? adapter.implementation : {};
  const wrapperEntrypoint = typeof implementation.wrapper_entrypoint === "string"
    ? resolveCompatibilityArtifact(input.compatibilityPath, implementation.wrapper_entrypoint)
    : undefined;
  const providerExecutable = input.resolveExecutables !== false && typeof implementation.executable === "string"
    ? await resolveAdapterExecutable({
      executable: implementation.executable,
      compatibilityPath: input.compatibilityPath,
    })
    : undefined;
  const cursorInstallRoot = typeof implementation.cursor_install_root === "string"
    ? resolveCompatibilityArtifact(input.compatibilityPath, implementation.cursor_install_root)
    : undefined;
  const providerInstallRoot = typeof implementation.provider_install_root === "string"
    ? resolveCompatibilityArtifact(input.compatibilityPath, implementation.provider_install_root)
    : undefined;
  const providerIdentity = typeof implementation.provider_identity === "string"
    ? implementation.provider_identity
    : undefined;
  return {
    enabled: adapter.enabled === true,
    allowed: stringArray(adapter.model_family_constraints.allowed),
    patterns: adapter.model_family_constraints.allowed_model_patterns === undefined
      ? []
      : stringArray(adapter.model_family_constraints.allowed_model_patterns),
    // Fail closed on omission: only an explicit `requires_explicit_model:
    // false` pin opts an adapter into account-default dispatch (#190).
    requiresExplicitModel: adapter.model_family_constraints.requires_explicit_model !== false,
    ...(wrapperEntrypoint === undefined ? {} : { wrapperEntrypoint }),
    ...(providerExecutable === undefined ? {} : { providerExecutable }),
    ...(cursorInstallRoot === undefined ? {} : { cursorInstallRoot }),
    ...(providerInstallRoot === undefined ? {} : { providerInstallRoot }),
    ...(providerIdentity === undefined ? {} : { providerIdentity }),
  };
}

function patternMatches(model: string, pattern: string): boolean {
  const expression = pattern
    .split("*")
    .map((part) => part.replace(/[\\^$.*+?()[\]{}|]/gu, "\\$&"))
    .join(".*");
  return new RegExp(`^${expression}$`, "iu").test(model);
}

export function assessAdapterModelPolicy(input: {
  modelFamily: string;
  modelId?: string | null;
  allowedFamilies: readonly string[];
  allowedModelPatterns?: readonly string[];
  requiresExplicitModel: boolean;
}): { allowed: true; reason: "allowed" } | { allowed: false; reason: "model-required" | "family-forbidden" | "model-forbidden" } {
  const modelAbsent = input.modelId === undefined || input.modelId === null || input.modelId.length === 0;
  if (input.requiresExplicitModel && modelAbsent) {
    return { allowed: false, reason: "model-required" };
  }
  // requiresExplicitModel false means account-default-only dispatch (#190):
  // the provider account's default model is used and the runtime rejects
  // explicit ids, so a present id fails closed here instead of reaching the
  // provider's known rejection path.
  if (!input.requiresExplicitModel && !modelAbsent) {
    return { allowed: false, reason: "model-forbidden" };
  }
  // On the account-default path there is no identifier for the pattern gate
  // to assess. The family gate still applies, and the open-weight family
  // bridge always needs an explicit matching model.
  const accountDefault = modelAbsent && !input.requiresExplicitModel;
  const patterns = input.allowedModelPatterns ?? [];
  const patternMatch = patterns.length === 0 ||
    (!modelAbsent && patterns.some((pattern) => patternMatches(input.modelId ?? "", pattern)));
  const familyAllowed = input.allowedFamilies.includes(input.modelFamily) || (
    patterns.length > 0 && input.allowedFamilies.includes("open-weight") && patternMatch
  );
  if (!familyAllowed) return { allowed: false, reason: "family-forbidden" };
  if (!patternMatch && !accountDefault) {
    return { allowed: false, reason: "model-forbidden" };
  }
  return { allowed: true, reason: "allowed" };
}

export async function resolveProviderAdapterSelection(input: {
  compatibilityPath: string;
  schemaPath: string;
  adapterId: string;
  modelFamily: string;
  model?: string;
}): Promise<{ adapterId: string; modelFamily: string; model: string; enabled: true }> {
  const policy = await loadAdapterModelConstraints({
    compatibilityPath: input.compatibilityPath,
    schemaPath: input.schemaPath,
    adapterId: input.adapterId,
    resolveExecutables: false,
  });
  const assessment = assessAdapterModelPolicy({
    modelFamily: input.modelFamily,
    ...(input.model === undefined ? {} : { modelId: input.model }),
    allowedFamilies: policy.allowed,
    allowedModelPatterns: policy.patterns,
    requiresExplicitModel: policy.requiresExplicitModel,
  });
  if (!assessment.allowed && assessment.reason === "model-required") {
    throw new FabricError("ADAPTER_MODEL_REQUIRED", "adapter requires an explicit model");
  }
  if (!assessment.allowed) {
    throw new FabricError("ADAPTER_FAMILY_FORBIDDEN", "model family is outside the adapter allow-list");
  }
  const model = input.model ?? "";
  return { adapterId: input.adapterId, modelFamily: input.modelFamily, model, enabled: true };
}

export async function validateAdapterModelSelection(input: {
  compatibilityPath: string;
  schemaPath: string;
  adapterId: string;
  requireEnabled: boolean;
  modelId: string | null;
  modelFamily: string;
}): Promise<{ valid: true; adapterId: string; modelFamily: string; modelId: string }> {
  const policy = await loadAdapterModelConstraints({
    compatibilityPath: input.compatibilityPath,
    schemaPath: input.schemaPath,
    adapterId: input.adapterId,
    requireEnabled: input.requireEnabled,
    resolveExecutables: false,
  });
  const assessment = assessAdapterModelPolicy({
    modelFamily: input.modelFamily,
    modelId: input.modelId,
    allowedFamilies: policy.allowed,
    allowedModelPatterns: policy.patterns,
    requiresExplicitModel: policy.requiresExplicitModel,
  });
  if (!assessment.allowed && assessment.reason === "model-required") {
    throw new FabricError("MODEL_REQUIRED", "adapter requires an explicit model");
  }
  if (!assessment.allowed && assessment.reason === "family-forbidden") {
    const code = input.adapterId === "kiro-acp" ? "MODEL_FAMILY_NOT_ALLOWED" : "MODEL_NOT_ALLOWED";
    throw new FabricError(code, "model family is outside the adapter allow-list");
  }
  const modelId = input.modelId ?? "";
  if (!assessment.allowed) {
    throw new FabricError("MODEL_NOT_ALLOWED", "model identifier is outside the adapter pattern allow-list");
  }
  return { valid: true, adapterId: input.adapterId, modelFamily: input.modelFamily, modelId };
}
