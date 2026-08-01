import { resolve } from "node:path";

import { validateEnabledAdapterExecutables } from "../src/adapters/compatibility.js";
import { FabricError } from "../src/errors.js";

function argumentValue(name: string): string {
  const index = process.argv.indexOf(name);
  const value = process.argv[index + 1];
  if (index === -1 || value === undefined || value.startsWith("--")) throw new Error(`${name} requires a value`);
  return resolve(value);
}

try {
  const compatibilityPath = argumentValue("--compatibility");
  const schemaIndex = process.argv.indexOf("--schema");
  const schemaPath = schemaIndex === -1 ? undefined : process.argv[schemaIndex + 1];
  if (schemaIndex !== -1 && (schemaPath === undefined || schemaPath.startsWith("--"))) throw new Error("--schema requires a value");
  const adapterIndex = process.argv.indexOf("--adapter");
  const adapter = adapterIndex === -1 ? undefined : process.argv[adapterIndex + 1];
  if (adapterIndex !== -1 && (adapter === undefined || adapter.startsWith("--"))) throw new Error("--adapter requires a value");
  const result = await validateEnabledAdapterExecutables({
    compatibilityPath,
    ...(schemaPath === undefined ? {} : { schemaPath: resolve(schemaPath) }),
    ...(adapter === undefined ? {} : { adapterIds: [adapter] }),
    mandatoryPrimary: process.argv.includes("--mandatory-primary"),
  });
  if (adapter !== undefined && result.unavailableOptionalAdapters.length > 0) {
    throw new FabricError(
      "ADAPTER_ARTIFACT_MISSING",
      result.unavailableOptionalAdapters.flatMap((failure) => failure.reasons).join("\n"),
    );
  }
  process.stdout.write(`${JSON.stringify({ status: "pass", ...result })}\n`);
} catch (error: unknown) {
  const message = error instanceof FabricError || error instanceof Error ? error.message : String(error);
  process.stderr.write(`${JSON.stringify({ status: "fail", code: error instanceof FabricError ? error.code : "VALIDATION_FAILED", message })}\n`);
  process.exitCode = 1;
}
