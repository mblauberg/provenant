import { mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import type { FabricPaths } from "../../src/cli/paths.ts";
import { installSeatGeneration, projectKey } from "../../src/cli/seat-store.ts";

const mocks = vi.hoisted(() => ({
  connect: vi.fn(),
  release: vi.fn(),
}));

vi.mock("../../src/daemon/client.ts", () => ({
  connectFabricDaemon: mocks.connect,
}));

vi.mock("../../src/cli/mcp-provision.ts", () => ({
  bindProvisionedSeatRoster: vi.fn(),
  startMcpProvisionDaemon: vi.fn(async () => ({
    address: { path: "/fixture/fabric.sock" },
    release: mocks.release,
  })),
}));

const { provisionMcpPeerSeats } = await import("../../src/cli/mcp-peer-provision.ts");

const roots: string[] = [];

afterEach(async () => {
  mocks.connect.mockReset();
  mocks.release.mockReset();
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

async function fixture(): Promise<{ project: string; paths: FabricPaths }> {
  const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-peer-lifecycle-")));
  roots.push(root);
  const project = join(root, "project");
  const stateDirectory = join(root, "state");
  await Promise.all([mkdir(project), mkdir(stateDirectory, { mode: 0o700 })]);
  const paths = {
    stateDirectory,
    runtimeDirectory: join(root, "runtime"),
    databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
    socketPath: join(root, "runtime", "fabric-v1.sock"),
  };
  const generation = "a".repeat(64);
  const common = {
    schemaVersion: 1 as const,
    projectKey: projectKey(project),
    projectPath: project,
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
    stateDirectory,
    projectPath: project,
    generation,
    expectedPreviousGeneration: null,
    seats: [
      {
        credential: `afc_${"a".repeat(43)}`,
        metadata: { ...common, seat: "claude" as const, agentId: "claude-peer", role: "peer" as const },
      },
      {
        credential: `afc_${"b".repeat(43)}`,
        metadata: { ...common, seat: "codex" as const, agentId: "codex-chair", role: "chair" as const },
      },
    ],
  });
  return { project, paths };
}

describe("MCP peer provision daemon lifecycle", () => {
  it("releases an attached daemon when chair connection fails before database open", async () => {
    const value = await fixture();
    mocks.connect.mockRejectedValueOnce(new Error("chair connection refused"));

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toThrow("chair connection refused");
    expect(mocks.release).toHaveBeenCalledOnce();
  });
});
