import { validateEnabledAdapterExecutables } from "../src/adapters/compatibility.js";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { parse } from "yaml";
import { FabricError } from "../src/errors.js";

const root = resolve(import.meta.dirname, "../../..");

function argumentValue(name: string, required = false): string | undefined {
  const index = process.argv.indexOf(name);
  if (index === -1) {
    if (required) throw new Error(`${name} requires a value`);
    return undefined;
  }
  const value = process.argv[index + 1];
  if (value === undefined || value.startsWith("--")) throw new Error(`${name} requires a value`);
  return resolve(value);
}

function argumentValues(name: string): string[] {
  const values: string[] = [];
  for (let index = 0; index < process.argv.length; index += 1) {
    if (process.argv[index] !== name) continue;
    const value = process.argv[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${name} requires a value`);
    values.push(value);
    index += 1;
  }
  return values;
}

function errorCode(error: unknown): string | undefined {
  return error instanceof FabricError ? error.code : undefined;
}

const adapterIds = argumentValues("--adapter");
try {
  const compatibilityPath = argumentValue("--compatibility", true) as string;
  const schemaPath = argumentValue("--schema") ?? join(root, "runtime/agent-fabric/schemas/adapter-compatibility.schema.json");
  const configPath = argumentValue("--config") ?? join(resolve(compatibilityPath, ".."), "agent-fabric.yaml");
  let activeAdapterIds: string[] | undefined;
  try {
    const config: unknown = parse(await readFile(configPath, "utf8"));
    const active = typeof config === "object" && config !== null && !Array.isArray(config)
      ? Reflect.get(config, "activeAdapters")
      : undefined;
    if (Array.isArray(active) && active.every((item) => typeof item === "string")) {
      activeAdapterIds = active;
    }
  } catch {
    // Test and portable callers may validate an isolated compatibility file;
    // the primary policy remains the safe default when no active config exists.
  }
  const report = await validateEnabledAdapterExecutables({
    compatibilityPath,
    schemaPath,
    ...(adapterIds.length === 0 ? {} : { adapterIds }),
    ...(activeAdapterIds === undefined ? {} : { activeAdapterIds }),
  });
  if (adapterIds.length > 0) {
    const missing = adapterIds.filter((adapterId) => report.resolvedExecutables[adapterId] === undefined);
    if (missing.length > 0) {
      throw new FabricError("ADAPTER_ARTIFACT_MISSING", `adapter executable validation did not resolve: ${missing.join(", ")}`);
    }
    process.stdout.write(JSON.stringify({ status: "pass", resolvedExecutables: report.resolvedExecutables }) + "\n");
    process.exitCode = 0;
  }
  for (const failure of report.unavailableOptionalAdapters) {
    process.stdout.write(`warning: optional adapter ${failure.adapterId} is unavailable: ${failure.reason}\n`);
  }
} catch (error: unknown) {
  const message = error instanceof FabricError || error instanceof Error ? error.message : String(error);
  if (adapterIds.length > 0) {
    process.stderr.write(`${JSON.stringify({ status: "fail", code: errorCode(error) ?? "VALIDATION_FAILED", message })}\n`);
  } else {
    process.stderr.write(`${message}\n`);
  }
  process.exitCode = 1;
}
