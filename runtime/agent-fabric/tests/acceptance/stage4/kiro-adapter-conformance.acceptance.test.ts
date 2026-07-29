import { rm } from "node:fs/promises";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { verifyAdapterCompatibility } from "../../../src/index.ts";
import { loadFabricConfig } from "../../../src/config/index.ts";
import {
  createCursorKiroCompatibilityFixture,
  repositoryPath,
  requireStage4PublicFunction,
  runStage4Fixture,
} from "../../support/stage4-cursor-kiro-testkit.ts";

describe("Stage 4 Kiro ACP adapter public contract", () => {
  it("activates Kiro in the trusted global composition", async () => {
    await expect(loadFabricConfig({
      globalPath: repositoryPath("config/agent-fabric.yaml"),
      agentsHome: repositoryPath("."),
    })).resolves.toMatchObject({
      adapterIds: expect.arrayContaining(["kiro-acp"]),
    });
  });

  it("keeps the checked-in adapter enabled while portable fixtures remain verifiable", async () => {
    const fixture = process.env.AGENT_FABRIC_PORTABLE_TESTS === "1"
      ? await createCursorKiroCompatibilityFixture()
      : undefined;
    try {
      const verification = verifyAdapterCompatibility({
        compatibilityPath: fixture?.compatibilityPath
          ?? repositoryPath("config/adapter-compatibility.yaml"),
        schemaPath: fixture?.schemaPath
          ?? repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
        adapterIds: ["kiro-acp"],
        requireEnabled: true,
      });
      await expect(verification).resolves.toMatchObject({ valid: true, adapterIds: ["kiro-acp"] });
    } finally {
      if (fixture !== undefined) await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("runs a fixture through the same durable adapter protocol without invoking Kiro", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    try {
      const report = await runStage4Fixture({
        adapterId: "kiro-acp",
        model: "qwen3-coder",
        modelFamily: "open-weight",
        journalPath: join(fixture.directory, "kiro-journal.json"),
      });
      expect(report).toMatchObject({
        passed: true,
        protocolVersion: 1,
        capabilities: {
          adapterContractVersion: 1,
          adapterId: "kiro-acp",
          requiresExplicitModel: true,
          modelFamilies: ["open-weight"],
        },
        action: { status: "terminal", executionCount: 1, retryMatched: true, changedPayloadRejected: true },
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("requires an explicit open-weight Kiro model without exact model locks", async () => {
    const validate = requireStage4PublicFunction("validateAdapterModelSelection");
    const fixture = await createCursorKiroCompatibilityFixture();
    const base = {
      compatibilityPath: fixture.compatibilityPath,
      schemaPath: fixture.schemaPath,
      adapterId: "kiro-acp",
      requireEnabled: true,
    };
    try {
      await expect(validate({ ...base, modelId: "qwen3-coder", modelFamily: "open-weight" })).resolves.toMatchObject({
        valid: true,
        adapterId: "kiro-acp",
        modelFamily: "open-weight",
      });
      await expect(validate({ ...base, modelId: null, modelFamily: "open-weight" })).rejects.toMatchObject({
        code: "MODEL_REQUIRED",
      });
      await expect(validate({ ...base, modelId: "qwen-future-coder", modelFamily: "open-weight" })).resolves.toMatchObject({
        valid: true,
        adapterId: "kiro-acp",
        modelFamily: "open-weight",
      });
      for (const [modelId, modelFamily] of [
        ["claude-fable-5", "anthropic"],
        ["gpt-5.6", "openai"],
        ["gemini-3.5-pro", "google"],
        ["grok-4.5", "xai"],
        ["composer-2.5", "cursor-composer"],
      ]) {
        await expect(validate({ ...base, modelId, modelFamily })).rejects.toMatchObject({
          code: "MODEL_FAMILY_NOT_ALLOWED",
        });
      }
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("admits Kiro's advertised auto alias through the checked-in Fabric policy", async () => {
    const validate = requireStage4PublicFunction("validateAdapterModelSelection");
    const base = {
      compatibilityPath: repositoryPath("config/adapter-compatibility.yaml"),
      schemaPath: repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
      adapterId: "kiro-acp",
      requireEnabled: true,
      modelFamily: "open-weight",
    };

    await expect(validate({ ...base, modelId: "auto" })).resolves.toMatchObject({
      valid: true,
      adapterId: "kiro-acp",
      modelId: "auto",
    });
    await expect(validate({ ...base, modelId: "unknown-alias" })).rejects.toMatchObject({
      code: "MODEL_NOT_ALLOWED",
    });
  });

});
