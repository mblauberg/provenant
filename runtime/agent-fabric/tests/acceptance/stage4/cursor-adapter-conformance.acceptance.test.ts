import { readFile, rm } from "node:fs/promises";
import { join } from "node:path";

import { describe, expect, it } from "vitest";
import { parse } from "yaml";

import { verifyAdapterCompatibility } from "../../../src/index.ts";
import {
  createCursorKiroCompatibilityFixture,
  repositoryPath,
  requireStage4PublicFunction,
  runStage4Fixture,
} from "../../support/stage4-cursor-kiro-testkit.ts";

describe("Stage 4 Cursor adapter public contract", () => {
  it("keeps the checked-in adapter activated while portable fixtures remain verifiable", async () => {
    const fixture = process.env.AGENT_FABRIC_PORTABLE_TESTS === "1"
      ? await createCursorKiroCompatibilityFixture()
      : undefined;
    try {
      if (fixture === undefined) {
        const document = parse(await readFile(repositoryPath("config/adapter-compatibility.yaml"), "utf8")) as {
          adapters?: Record<string, { enabled?: boolean }>;
        };
        expect(document.adapters?.["cursor-agent"]).toMatchObject({ enabled: true });
      } else {
        await expect(verifyAdapterCompatibility({
          compatibilityPath: fixture.compatibilityPath,
          schemaPath: fixture.schemaPath,
          adapterIds: ["cursor-agent"],
          requireEnabled: true,
        })).resolves.toMatchObject({ valid: true, adapterIds: ["cursor-agent"] });
      }
    } finally {
      if (fixture !== undefined) await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("runs a fixture through the same durable adapter protocol without invoking Cursor", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    try {
      const report = await runStage4Fixture({
        adapterId: "cursor-agent",
        model: "composer-2.5",
        modelFamily: "cursor-composer",
        journalPath: join(fixture.directory, "cursor-journal.json"),
      });
      expect(report).toMatchObject({
        passed: true,
        protocolVersion: 1,
        capabilities: {
          adapterContractVersion: 1,
          adapterId: "cursor-agent",
          requiresExplicitModel: true,
          modelFamilies: ["cursor-composer", "xai"],
        },
        action: { status: "terminal", executionCount: 1, retryMatched: true, changedPayloadRejected: true },
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("accepts only explicit Composer or Grok selections", async () => {
    const validate = requireStage4PublicFunction("validateAdapterModelSelection");
    const fixture = await createCursorKiroCompatibilityFixture();
    const base = {
      compatibilityPath: fixture.compatibilityPath,
      schemaPath: fixture.schemaPath,
      adapterId: "cursor-agent",
      requireEnabled: true,
    };
    try {
      await expect(
        validate({ ...base, modelId: "composer-2.5", modelFamily: "cursor-composer" }),
      ).resolves.toMatchObject({ valid: true, adapterId: "cursor-agent", modelFamily: "cursor-composer" });
      await expect(validate({ ...base, modelId: "cursor-grok-4.5-high", modelFamily: "xai" })).resolves.toMatchObject({
        valid: true,
        adapterId: "cursor-agent",
        modelFamily: "xai",
      });
      await expect(validate({ ...base, modelId: null, modelFamily: "cursor-composer" })).rejects.toMatchObject({
        code: "MODEL_REQUIRED",
      });
      for (const [modelId, modelFamily] of [
        ["gpt-5.6", "openai"],
        ["claude-fable-5", "anthropic"],
        ["gemini-3.5-pro", "google"],
      ]) {
        await expect(validate({ ...base, modelId, modelFamily })).rejects.toMatchObject({ code: "MODEL_NOT_ALLOWED" });
      }
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

});
