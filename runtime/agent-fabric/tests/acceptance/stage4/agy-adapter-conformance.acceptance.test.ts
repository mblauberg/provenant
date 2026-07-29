import { readFile, rm } from "node:fs/promises";

import { describe, expect, it } from "vitest";
import { parse } from "yaml";

import {
  resolveProviderAdapterSelection,
  runAdapterConformance,
  verifyAdapterCompatibility,
} from "../../../src/index.ts";
import {
  createResolvedStage4Compatibility,
  stage4FixtureCommand,
  stage4RepositoryPath,
} from "../../support/stage4-pi-agy-testkit.ts";

describe("Stage 4 Agy adapter", () => {
  it("keeps the checked-in adapter activated while portable fixtures remain verifiable", async () => {
    const fixture = process.env.AGENT_FABRIC_PORTABLE_TESTS === "1"
      ? await createResolvedStage4Compatibility("agy")
      : undefined;
    try {
      if (fixture === undefined) {
        const document = parse(await readFile(stage4RepositoryPath("config/adapter-compatibility.yaml"), "utf8")) as {
          adapters?: Record<string, { enabled?: boolean }>;
        };
        expect(document.adapters?.agy).toMatchObject({ enabled: true });
      } else {
        await expect(verifyAdapterCompatibility({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterIds: ["agy"],
          requireEnabled: true,
        })).resolves.toMatchObject({ valid: true, adapterIds: ["agy"] });
      }
    } finally {
      if (fixture !== undefined) await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("uses the shared adapter protocol with deterministic Google-only fixture capabilities", async () => {
    const result = await runAdapterConformance({
      command: stage4FixtureCommand("agy"),
      environment: {},
      action: {
        actionId: "agy-action",
        operation: "send_turn",
        payload: { modelFamily: "google", model: "gemini-fixture" },
      },
    });
    expect(result.passed).toBe(true);
    expect(result.capabilities).toMatchObject({
      protocolVersion: 1,
      adapterContractVersion: 1,
      adapterId: "agy",
      actionJournal: true,
      controlModes: ["managed"],
      allowedModelFamilies: ["google"],
      requiresExplicitModel: true,
    });
  });

  it("requires an explicit model and rejects every non-Google family", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    try {
      await expect(
        resolveProviderAdapterSelection({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterId: "agy",
          modelFamily: "google",
        }),
      ).rejects.toMatchObject({ code: "ADAPTER_MODEL_REQUIRED" });
      for (const modelFamily of ["openai", "anthropic", "xai", "generic-open", "open-weight"]) {
        await expect(
          resolveProviderAdapterSelection({
            compatibilityPath: fixture.compatibilityPath,
            schemaPath: fixture.schemaPath,
            adapterId: "agy",
            modelFamily,
            model: "wrong-family-fixture",
          }),
        ).rejects.toMatchObject({ code: "ADAPTER_FAMILY_FORBIDDEN" });
      }
      await expect(
        resolveProviderAdapterSelection({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterId: "agy",
          modelFamily: "google",
          model: "gemini-fixture",
        }),
      ).resolves.toMatchObject({ adapterId: "agy", modelFamily: "google", enabled: true });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

});
