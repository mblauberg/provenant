import { describe, expect, it } from "vitest";

import { verifyAdapterCompatibility } from "../../src/adapters/compatibility.ts";
import { repositoryPath } from "../support/primary-adapter-testkit.ts";

const liveSmokeEnabled = process.env.AGENT_FABRIC_LIVE_COMPATIBILITY_SMOKE === "1";

describe.skipIf(!liveSmokeEnabled)("live workstation adapter compatibility smoke", () => {
  it("validates every checked-in active adapter entry", async () => {
    const result = await verifyAdapterCompatibility({
      compatibilityPath: repositoryPath("config/adapter-compatibility.yaml"),
      schemaPath: repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
      adapterIds: ["agy", "claude-agent-sdk", "codex-app-server", "cursor-agent", "opencode-acp"],
      requireEnabled: true,
    });

    expect(result).toMatchObject({
      valid: true,
      adapterIds: ["agy", "claude-agent-sdk", "codex-app-server", "cursor-agent", "opencode-acp"],
    });
  }, 30_000);
});
