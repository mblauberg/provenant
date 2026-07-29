import { readFile, rm, writeFile } from "node:fs/promises";

import { describe, expect, it } from "vitest";
import { parse, stringify } from "yaml";

import {
  createPrimaryCompatibilityFixture,
  createPortableActivatedPrimaryFixture,
  requirePublicFunction,
} from "../../support/primary-adapter-testkit.ts";

describe("Section 21 Stage 3 adapter capability and activation gate", () => {
  function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === "object" && value !== null && !Array.isArray(value);
  }

  function record(value: unknown, label: string): Record<string, unknown> {
    if (!isRecord(value)) {
      throw new TypeError(`${label} must be an object`);
    }
    return value;
  }

  it("accepts configured primary adapters and keeps the visibility-only Herdr entry disabled", async () => {
    const verify = requirePublicFunction("verifyAdapterCompatibility");
    const fixture = await createPortableActivatedPrimaryFixture();

    try {
      for (const adapterId of ["claude-agent-sdk", "codex-app-server"]) {
        await expect(
          verify({
            compatibilityPath: fixture.compatibilityPath,
            schemaPath: fixture.schemaPath,
            adapterIds: [adapterId],
            requireEnabled: true,
          }),
        ).resolves.toMatchObject({ valid: true, adapterIds: [adapterId] });
      }
      await expect(
        verify({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterIds: ["herdr"],
          requireEnabled: true,
        }),
      ).rejects.toMatchObject({ code: "ADAPTER_DISABLED" });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("validates compatibility metadata without executing providers", async () => {
    const verify = requirePublicFunction("verifyAdapterCompatibility");
    const fixture = await createPrimaryCompatibilityFixture();

    await expect(
      verify({
        compatibilityPath: fixture.compatibilityPath,
        schemaPath: fixture.schemaPath,
        adapterIds: ["claude-agent-sdk", "codex-app-server", "herdr"],
        requireEnabled: false,
      }),
    ).resolves.toMatchObject({
      valid: true,
      adapterIds: ["claude-agent-sdk", "codex-app-server", "herdr"],
    });
  });

  it("does not reject a provider package update before runtime conformance", async () => {
    const verify = requirePublicFunction("verifyAdapterCompatibility");
    const fixture = await createPrimaryCompatibilityFixture();
    await writeFile(fixture.artifactPaths[0] ?? "", "updated provider fixture\n");

    await expect(
      verify({
        compatibilityPath: fixture.compatibilityPath,
        schemaPath: fixture.schemaPath,
        adapterIds: ["claude-agent-sdk"],
        requireEnabled: false,
      }),
    ).resolves.toMatchObject({ valid: true, adapterIds: ["claude-agent-sdk"] });
  });

  it("rejects an enabled adapter whose fabric-owned wrapper is not configured", async () => {
    const verify = requirePublicFunction("verifyAdapterCompatibility");
    const fixture = await createPrimaryCompatibilityFixture();
    const document: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
    const adapters = record(record(document, "compatibility document").adapters, "adapters");
    const adapter = record(adapters["claude-agent-sdk"], "claude adapter");
    adapter.enabled = true;
    await writeFile(fixture.compatibilityPath, stringify(document));

    await expect(
      verify({
        compatibilityPath: fixture.compatibilityPath,
        schemaPath: fixture.schemaPath,
        adapterIds: ["claude-agent-sdk"],
        requireEnabled: true,
      }),
    ).rejects.toMatchObject({ code: "ADAPTER_COMPATIBILITY_INVALID" });
  });

  it("requires the adapter-specific provider identity policy for enabled primaries", async () => {
    const verify = requirePublicFunction("verifyAdapterCompatibility");
    const fixture = await createPortableActivatedPrimaryFixture();
    const document: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
    const adapters = record(record(document, "compatibility document").adapters, "adapters");
    const adapter = record(adapters["claude-agent-sdk"], "claude adapter");
    const implementation = record(adapter.implementation, "implementation");
    implementation.provider_identity = "cursor-partial-signed-helpers";
    await writeFile(fixture.compatibilityPath, stringify(document));

    await expect(verify({
      compatibilityPath: fixture.compatibilityPath,
      schemaPath: fixture.schemaPath,
      adapterIds: ["claude-agent-sdk"],
      requireEnabled: true,
    })).rejects.toMatchObject({ code: "ADAPTER_COMPATIBILITY_INVALID" });
  });
});
