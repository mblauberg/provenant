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
  await validateEnabledAdapterExecutables({ compatibilityPath: argumentValue("--compatibility") });
} catch (error: unknown) {
  const message = error instanceof FabricError || error instanceof Error ? error.message : String(error);
  process.stderr.write(`${message}\n`);
  process.exitCode = 1;
}
