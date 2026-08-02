import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import { verifyAdapterCompatibility } from "../../src/adapters/compatibility.ts";
import { PRIMARY_ADAPTER_IDS } from "../../src/adapters/primary-adapters.ts";
import { FabricError } from "../../src/errors.ts";

function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

const compatibilityPath = repositoryPath("config/adapter-compatibility.yaml");
const schemaPath = repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json");
const originalPath = process.env.PATH;

afterEach(() => {
  process.env.PATH = originalPath;
});

describe("adapter compatibility without the machine's binaries", () => {
  // CI has no provider binaries installed. Metadata verification has to stand on
  // its own there, or the declaration can never be checked off the workstation
  // that happens to have the adapters. /usr/bin:/bin keeps git for the wrapper
  // provenance probe while excluding every adapter binary.
  it("verifies the declaration when no provider executable resolves", async () => {
    process.env.PATH = "/usr/bin:/bin";

    const result = await verifyAdapterCompatibility({
      compatibilityPath,
      schemaPath,
      adapterIds: [...PRIMARY_ADAPTER_IDS],
      requireEnabled: true,
      resolveExecutables: false,
    });

    expect(result.valid).toBe(true);
    expect(result.adapterIds).toEqual([...PRIMARY_ADAPTER_IDS]);
    expect(result.resolvedExecutables).toEqual({});
  });

  it("still refuses when a caller asks for executables that are absent", async () => {
    process.env.PATH = "/usr/bin:/bin";

    const failure = await verifyAdapterCompatibility({
      compatibilityPath,
      schemaPath,
      adapterIds: [...PRIMARY_ADAPTER_IDS],
      requireEnabled: true,
    }).catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(FabricError);
    expect((failure as FabricError).code).toBe("ADAPTER_ARTIFACT_MISSING");
  });
});
