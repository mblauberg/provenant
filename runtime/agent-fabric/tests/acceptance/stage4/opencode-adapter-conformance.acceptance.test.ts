import { rm } from "node:fs/promises";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { verifyAdapterCompatibility } from "../../../src/index.ts";
import {
  createCursorKiroCompatibilityFixture,
  repositoryPath,
  runStage4Fixture,
} from "../../support/stage4-cursor-kiro-testkit.ts";

describe("Stage 4 OpenCode ACP adapter public contract", () => {
  it("keeps the checked-in adapter enabled while portable fixtures remain verifiable", async () => {
    const fixture = process.env.AGENT_FABRIC_PORTABLE_TESTS === "1"
      ? await createCursorKiroCompatibilityFixture()
      : undefined;
    try {
      await expect(verifyAdapterCompatibility({
        compatibilityPath: fixture?.compatibilityPath ?? repositoryPath("config/adapter-compatibility.yaml"),
        schemaPath: fixture?.schemaPath ?? repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
        adapterIds: ["opencode-acp"],
        requireEnabled: true,
        resolveExecutables: false,
      })).resolves.toMatchObject({ valid: true, adapterIds: ["opencode-acp"] });
    } finally {
      if (fixture !== undefined) await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("runs explicit account-model turns through the durable adapter protocol", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    try {
      await expect(runStage4Fixture({
        adapterId: "opencode-acp",
        model: "opencode/deepseek-v4-flash-free",
        modelFamily: "generic-open",
        journalPath: join(fixture.directory, "opencode-journal.json"),
      })).resolves.toMatchObject({
        passed: true,
        capabilities: {
          adapterId: "opencode-acp",
          requiresExplicitModel: true,
          modelFamilies: ["generic-open"],
        },
        action: { status: "terminal", executionCount: 1, retryMatched: true, changedPayloadRejected: true },
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });
});
