import { rm } from "node:fs/promises";

import { describe, expect, it } from "vitest";

import {
  resolveProviderAdapterSelection,
  runAdapterConformance,
  verifyAdapterCompatibility,
} from "../../../src/index.ts";
import {
  createResolvedStage4Compatibility,
  stage4FixtureCommand,
  stage4RepositoryPath,
  stage4SchemaPath,
} from "../../support/stage4-pi-agy-testkit.ts";

describe("Stage 4 Pi adapter", () => {
  it("keeps the checked-in adapter disabled without an available trusted open-model provider", async () => {
    await expect(
      verifyAdapterCompatibility({
        compatibilityPath: stage4RepositoryPath("config/adapter-compatibility.yaml"),
        schemaPath: stage4SchemaPath(),
        adapterIds: ["pi-rpc"],
        requireEnabled: true,
      }),
    ).rejects.toMatchObject({ code: "ADAPTER_DISABLED" });
  });

  it("uses the shared adapter protocol with deterministic generic/open-weight fixture capabilities", async () => {
    const result = await runAdapterConformance({
      command: stage4FixtureCommand("pi-rpc"),
      environment: {},
      action: {
        actionId: "pi-action",
        operation: "send_turn",
        payload: { modelFamily: "open-weight", model: "fixture-open-model" },
      },
    });
    expect(result.passed).toBe(true);
    expect(result.capabilities).toMatchObject({
      protocolVersion: 1,
      adapterContractVersion: 1,
      adapterId: "pi-rpc",
      actionJournal: true,
      controlModes: ["managed"],
      allowedModelFamilies: ["generic-open", "open-weight"],
      requiresExplicitModel: true,
    });
  });

  it("requires an explicit model and rejects non-generic families", async () => {
    const fixture = await createResolvedStage4Compatibility("pi-rpc");
    try {
      await expect(
        resolveProviderAdapterSelection({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterId: "pi-rpc",
          modelFamily: "open-weight",
        }),
      ).rejects.toMatchObject({ code: "ADAPTER_MODEL_REQUIRED" });
      await expect(
        resolveProviderAdapterSelection({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterId: "pi-rpc",
          modelFamily: "google",
          model: "gemini-fixture",
        }),
      ).rejects.toMatchObject({ code: "ADAPTER_FAMILY_FORBIDDEN" });
      await expect(
        resolveProviderAdapterSelection({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterId: "pi-rpc",
          modelFamily: "generic-open",
          model: "fixture-api-model",
        }),
      ).resolves.toMatchObject({ adapterId: "pi-rpc", modelFamily: "generic-open", enabled: true });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

});
