import { mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  McpPeerProvisionChairRequiredError,
  provisionMcpPeerSeats,
} from "../../src/cli/mcp-peer-provision.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import { installSeatGeneration, projectKey } from "../../src/cli/seat-store.ts";

const roots: string[] = [];
afterEach(async () => {
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

async function fixture(): Promise<{ project: string; paths: FabricPaths }> {
  const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-peer-provision-unit-")));
  roots.push(root);
  const project = join(root, "project");
  const stateDirectory = join(root, "state");
  await Promise.all([mkdir(project), mkdir(stateDirectory, { mode: 0o700 })]);
  return {
    project,
    paths: {
      stateDirectory,
      runtimeDirectory: join(root, "runtime"),
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(root, "runtime", "fabric-v1.sock"),
    },
  };
}

describe("MCP peer provision preconditions", () => {
  it("names bootstrap when no chair seat exists without attempting daemon startup", async () => {
    const value = await fixture();

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toBeInstanceOf(McpPeerProvisionChairRequiredError);
    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toThrow(/agent-fabric bootstrap --seat claude.*agent-fabric bootstrap --seat codex/u);
  });

  it("refuses to provision the active chair seat before daemon startup", async () => {
    const value = await fixture();
    const generation = "a".repeat(64);
    const common = {
      schemaVersion: 1 as const,
      projectKey: projectKey(value.project),
      projectPath: value.project,
      generation,
      previousGeneration: null,
      originKind: "bootstrap" as const,
      projectSessionId: "session-one",
      sessionRevision: 1,
      sessionGeneration: 1,
      runId: "run-one",
      runRevision: 1,
      chairAgentId: "codex-chair",
      chairGeneration: 1,
      chairLeaseId: "chair:run-one:1",
      principalGeneration: 1,
      expiresAt: "2099-01-01T00:00:00.000Z",
    };
    await installSeatGeneration({
      stateDirectory: value.paths.stateDirectory,
      projectPath: value.project,
      generation,
      expectedPreviousGeneration: null,
      seats: [
        {
          credential: `afc_${"a".repeat(43)}`,
          metadata: { ...common, seat: "claude", agentId: "claude-peer", role: "peer" },
        },
        {
          credential: `afc_${"b".repeat(43)}`,
          metadata: { ...common, seat: "codex", agentId: "codex-chair", role: "chair" },
        },
      ],
    });

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "codex",
    ], value.paths)).rejects.toThrow(/refuses to provision the active chair seat codex/u);
  });
});
