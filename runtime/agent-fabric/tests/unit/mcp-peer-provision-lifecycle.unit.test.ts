import { createHash } from "node:crypto";
import { mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it, vi } from "vitest";

import { peerSeatAuthority } from "../../src/cli/observer-provision.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import { installSeatGeneration, projectKey } from "../../src/cli/seat-store.ts";
import { Fabric } from "../../src/core/fabric.ts";

const mocks = vi.hoisted(() => ({
  bind: vi.fn(),
  connect: vi.fn(),
  release: vi.fn(),
}));

vi.mock("../../src/daemon/client.ts", () => ({
  connectFabricDaemon: mocks.connect,
}));

vi.mock("../../src/cli/mcp-provision.ts", () => ({
  bindProvisionedSeatRoster: mocks.bind,
  startMcpProvisionDaemon: vi.fn(async () => ({
    address: { path: "/fixture/fabric.sock" },
    release: mocks.release,
  })),
}));

const { provisionMcpPeerSeats } = await import("../../src/cli/mcp-peer-provision.ts");

const roots: string[] = [];

afterEach(async () => {
  mocks.bind.mockReset();
  mocks.connect.mockReset();
  mocks.release.mockReset();
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

async function fixture(input: { peerExpiresAt?: string } = {}): Promise<{ project: string; paths: FabricPaths }> {
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
        metadata: {
          ...common,
          seat: "claude" as const,
          agentId: "claude-peer",
          role: "peer" as const,
          expiresAt: input.peerExpiresAt ?? common.expiresAt,
        },
      },
      {
        credential: `afc_${"b".repeat(43)}`,
        metadata: { ...common, seat: "codex" as const, agentId: "codex-chair", role: "chair" as const },
      },
    ],
  });
  return { project, paths };
}

async function registeredPeerFixture(): Promise<{
  project: string;
  paths: FabricPaths;
  delegatedAuthorityId: string;
}> {
  const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-peer-registration-")));
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
  const now = Date.now();
  const fabric = new Fabric({ databasePath: paths.databasePath, workspaceRoots: [project], clock: () => now });
  try {
    const roster = fabric.bootstrapCurrentMcpSeat({
      canonicalRoot: project,
      trustRecordDigest: `sha256:${"a".repeat(64)}`,
      seat: "codex",
      expiresAt: new Date(now + 60 * 60 * 1_000).toISOString(),
    });
    const chair = roster.credentials[0]!;
    const database = new Database(paths.databasePath, { readonly: true });
    const chairAuthority = database.prepare(
      "SELECT authority_json FROM authorities WHERE authority_id=?",
    ).get(chair.authorityId) as { authority_json: string };
    database.close();
    const key = projectKey(project);
    const delegated = fabric.delegateAuthority(roster.runId, roster.chairAgentId, {
      parentAuthorityId: chair.authorityId,
      authority: peerSeatAuthority(JSON.parse(chairAuthority.authority_json)),
      commandId: `peer-seat:${key}:${roster.runId}:agy`,
    });
    const agentId = `agy_bootstrap_peer_${createHash("sha256")
      .update(JSON.stringify({ projectKey: key, runId: roster.runId, seat: "agy" }))
      .digest("hex")
      .slice(0, 16)}`;
    fabric.registerAgent(roster.runId, roster.chairAgentId, {
      agentId,
      authorityId: delegated.authorityId,
    });
    await installSeatGeneration({
      stateDirectory,
      projectPath: project,
      generation: roster.generation,
      expectedPreviousGeneration: roster.expectedPreviousGeneration,
      seats: [{
        credential: chair.capability,
        metadata: {
          schemaVersion: 1,
          projectKey: key,
          projectPath: project,
          generation: roster.generation,
          previousGeneration: roster.expectedPreviousGeneration,
          originKind: "bootstrap",
          projectSessionId: roster.projectSessionId,
          sessionRevision: roster.sessionRevision,
          sessionGeneration: roster.sessionGeneration,
          runId: roster.runId,
          runRevision: roster.runRevision,
          chairAgentId: roster.chairAgentId,
          chairGeneration: roster.chairGeneration,
          chairLeaseId: roster.chairLeaseId,
          seat: "codex",
          agentId: chair.agentId,
          principalGeneration: chair.expectedPrincipalGeneration,
          role: "chair",
          expiresAt: roster.expiresAt,
        },
      }],
    });
    return { project, paths, delegatedAuthorityId: delegated.authorityId };
  } finally {
    await fabric.close();
  }
}

describe("MCP peer provision daemon lifecycle", () => {
  it("re-provisions an expired installed peer instead of returning stale metadata", async () => {
    const value = await fixture({ peerExpiresAt: "2000-01-01T00:00:00.000Z" });
    mocks.connect.mockRejectedValueOnce(new Error("expired peer requires provisioning"));

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "claude",
    ], value.paths)).rejects.toThrow("expired peer requires provisioning");
    expect(mocks.connect).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("releases an attached daemon when chair connection fails before database open", async () => {
    const value = await fixture();
    mocks.connect.mockRejectedValueOnce(new Error("chair connection refused"));

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toThrow("chair connection refused");
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("rejects a registration capability that does not match live custody", async () => {
    const value = await registeredPeerFixture();
    const close = vi.fn();
    mocks.connect.mockResolvedValueOnce({
      close,
      delegateAuthority: vi.fn(async () => ({ authorityId: value.delegatedAuthorityId })),
      registerAgent: vi.fn(async () => ({ capability: `afc_${"z".repeat(43)}` })),
    });

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toThrow("registered peer agy capability does not match live custody");
    expect(mocks.bind).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("releases the daemon and surfaces cleanup failure when client close rejects", async () => {
    const value = await fixture();
    const close = vi.fn(async () => {
      throw new Error("client close failed");
    });
    mocks.connect.mockResolvedValueOnce({ close });

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toThrow("client close failed");
    expect(close).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });
});
