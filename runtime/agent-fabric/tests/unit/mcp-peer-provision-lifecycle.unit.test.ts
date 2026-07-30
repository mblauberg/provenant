import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it, vi } from "vitest";

import { peerSeatAuthority } from "../../src/cli/observer-provision.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import {
  installSeatGeneration,
  projectKey,
  resolveSeatPaths,
  type SeatMetadata,
} from "../../src/cli/seat-store.ts";
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

async function fixture(input: {
  peerExpiresAt?: string;
  expiresAt?: string;
  originKind?: "bootstrap" | "provisioned" | null;
} = {}): Promise<{ project: string; paths: FabricPaths }> {
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
    ...(input.originKind === null ? {} : { originKind: input.originKind ?? "bootstrap" as const }),
    projectSessionId: "session-one",
    sessionRevision: 1,
    sessionGeneration: 1,
    runId: "run-one",
    runRevision: 1,
    chairAgentId: "codex-chair",
    chairGeneration: 1,
    chairLeaseId: "chair:run-one:1",
    principalGeneration: 1,
    expiresAt: input.expiresAt ?? "2099-01-01T00:00:00.000Z",
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

async function registeredPeerFixture(input: {
  installPeer?: boolean;
  chairOriginKind?: "bootstrap" | "provisioned";
  installedExpiresAt?: string;
} = {}): Promise<{
  project: string;
  paths: FabricPaths;
  generation: string;
  sessionRevision: number;
  delegatedAuthorityId: string;
  chairAuthorityExpiresAt: string;
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
    const registration = fabric.registerAgent(roster.runId, roster.chairAgentId, {
      agentId,
      authorityId: delegated.authorityId,
    });
    const databaseAfterRegistration = new Database(paths.databasePath, { readonly: true });
    const registeredCapability = databaseAfterRegistration.prepare(`
      SELECT principal_generation
        FROM capabilities
       WHERE token_hash=?
    `).get(createHash("sha256").update(registration.capability).digest("hex")) as {
      principal_generation: number;
    };
    databaseAfterRegistration.close();
    const installedSeats: Array<{
      credential: string;
      metadata: Omit<SeatMetadata, "credentialPath">;
    }> = [{
      credential: chair.capability,
      metadata: {
        schemaVersion: 1,
        projectKey: key,
        projectPath: project,
        generation: roster.generation,
        previousGeneration: roster.expectedPreviousGeneration,
        originKind: input.chairOriginKind ?? "bootstrap",
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
        expiresAt: input.installedExpiresAt ?? roster.expiresAt,
      },
    }];
    if (input.installPeer === true) {
      installedSeats.unshift({
        credential: registration.capability,
        metadata: {
          schemaVersion: 1,
          projectKey: key,
          projectPath: project,
          generation: roster.generation,
          previousGeneration: roster.expectedPreviousGeneration,
          originKind: "provisioned",
          projectSessionId: roster.projectSessionId,
          sessionRevision: roster.sessionRevision,
          sessionGeneration: roster.sessionGeneration,
          runId: roster.runId,
          runRevision: roster.runRevision,
          chairAgentId: roster.chairAgentId,
          chairGeneration: roster.chairGeneration,
          chairLeaseId: roster.chairLeaseId,
          seat: "agy",
          agentId,
          principalGeneration: registeredCapability.principal_generation,
          role: "peer",
          expiresAt: input.installedExpiresAt ?? roster.expiresAt,
        },
      });
    }
    await installSeatGeneration({
      stateDirectory,
      projectPath: project,
      generation: roster.generation,
      expectedPreviousGeneration: roster.expectedPreviousGeneration,
      seats: installedSeats,
    });
    return {
      project,
      paths,
      generation: roster.generation,
      sessionRevision: roster.sessionRevision,
      delegatedAuthorityId: delegated.authorityId,
      chairAuthorityExpiresAt: (JSON.parse(chairAuthority.authority_json) as { expiresAt: string }).expiresAt,
    };
  } finally {
    await fabric.close();
  }
}

describe("MCP peer provision daemon lifecycle", () => {
  it("enters the roster rebind path when an installed peer has an explicit expiry", async () => {
    const value = await fixture({ originKind: "provisioned" });
    mocks.connect.mockRejectedValueOnce(new Error("explicit expiry reached renewal"));

    await expect(provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "claude",
      "--expires-at", new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
    ], value.paths)).rejects.toThrow("explicit expiry reached renewal");
    expect(mocks.connect).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("preserves explicit provisioned origins while renewing a provisioned roster", async () => {
    const value = await registeredPeerFixture({ installPeer: true, chairOriginKind: "provisioned" });
    const close = vi.fn();
    mocks.connect.mockResolvedValueOnce({ close });
    mocks.bind.mockResolvedValueOnce({ generation: "renewed" });

    await provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "agy",
      "--expires-at", new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
    ], value.paths);

    expect(mocks.bind).toHaveBeenCalledWith(expect.objectContaining({
      originKinds: {
        agy: "provisioned",
        codex: "provisioned",
      },
    }), value.paths);
    expect(close).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("refuses explicit peer renewal for a bootstrap-managed mixed roster", async () => {
    const value = await registeredPeerFixture({ installPeer: true });

    await expect(provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "agy",
      "--expires-at", new Date(Date.now() + 25 * 60 * 60 * 1_000).toISOString(),
    ], value.paths)).rejects.toThrow(/refuses to renew.*bootstrap --seat codex/iu);
    expect(mocks.connect).not.toHaveBeenCalled();
    expect(mocks.bind).not.toHaveBeenCalled();
    expect(mocks.release).not.toHaveBeenCalled();
  });

  it("refuses to rebind omitted origin metadata without a matching legacy marker", async () => {
    const value = await fixture({ originKind: null });

    await expect(provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "claude",
      "--expires-at", new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
    ], value.paths)).rejects.toThrow(/origin is unknown.*bootstrap --seat codex/iu);
    expect(mocks.connect).not.toHaveBeenCalled();
    expect(mocks.bind).not.toHaveBeenCalled();
    expect(mocks.release).not.toHaveBeenCalled();
  });

  it.each([null, "unknown"])(
    "refuses invalid explicit origin metadata %j before daemon startup",
    async (originKind) => {
      const value = await fixture({ originKind: "provisioned" });
      const location = await resolveSeatPaths({
        stateDirectory: value.paths.stateDirectory,
        project: value.project,
        seat: "codex",
      });
      const metadata = JSON.parse(await readFile(location.metadataPath, "utf8")) as Record<string, unknown>;
      metadata.originKind = originKind;
      await writeFile(location.metadataPath, JSON.stringify(metadata), { mode: 0o600 });

      await expect(provisionMcpPeerSeats([
        "--project", value.project,
        "--seat", "claude",
        "--expires-at", new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
      ], value.paths)).rejects.toThrow("MCP seat metadata is invalid");
      expect(mocks.connect).not.toHaveBeenCalled();
      expect(mocks.bind).not.toHaveBeenCalled();
      expect(mocks.release).not.toHaveBeenCalled();
    },
  );

  it.each([
    ["past", () => new Date(Date.now() - 1_000).toISOString()],
    ["over 31 days", () => new Date(Date.now() + 32 * 24 * 60 * 60 * 1_000).toISOString()],
    ["after the chair authority", (authority: string) => new Date(Date.parse(authority) + 1).toISOString()],
  ])("rejects an explicit expiry that is %s", async (_case, expiresAt) => {
    const value = await registeredPeerFixture({ installPeer: true, chairOriginKind: "provisioned" });
    const close = vi.fn();
    mocks.connect.mockResolvedValueOnce({ close });

    await expect(provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "agy",
      "--expires-at", expiresAt(value.chairAuthorityExpiresAt),
    ], value.paths)).rejects.toThrow(
      "must be in the future, no more than 31 days away, and not outlive the chair authority",
    );
    expect(mocks.bind).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("recovers an expired provisioned roster without a chair connection", async () => {
    const value = await registeredPeerFixture({
      installPeer: true,
      chairOriginKind: "provisioned",
      installedExpiresAt: new Date(Date.now() - 60_000).toISOString(),
    });
    mocks.bind.mockResolvedValueOnce({ generation: "recovered" });
    const requestedExpiry = new Date(Date.now() + 30 * 60 * 1_000).toISOString();

    await provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "agy",
      "--expires-at", requestedExpiry,
    ], value.paths);

    // The expired chair credential is unusable by definition, so recovery
    // never opens a chair connection; the daemon's own bootstrap-capability
    // gate authorises the rebind, exactly as it does for mcp provision.
    expect(mocks.connect).not.toHaveBeenCalled();
    expect(mocks.bind).toHaveBeenCalledWith(expect.objectContaining({
      expiresAt: requestedExpiry,
      sessionRevision: value.sessionRevision,
      expectedActiveGeneration: value.generation,
      requireProvisionedOrigins: true,
      originKinds: { agy: "provisioned", codex: "provisioned" },
    }), value.paths);
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("refuses to register a new seat while the roster is expired", async () => {
    const value = await registeredPeerFixture({
      installPeer: true,
      chairOriginKind: "provisioned",
      installedExpiresAt: new Date(Date.now() - 60_000).toISOString(),
    });

    await expect(provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "cursor",
      "--expires-at", new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
    ], value.paths)).rejects.toThrow(/roster expired at .*recover the roster first/u);
    expect(mocks.connect).not.toHaveBeenCalled();
    expect(mocks.bind).not.toHaveBeenCalled();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("renews with the live session revision rather than the stored one", async () => {
    const value = await registeredPeerFixture({ installPeer: true, chairOriginKind: "provisioned" });
    const database = new Database(value.paths.databasePath);
    try {
      database.prepare("UPDATE project_sessions SET revision=revision+1").run();
    } finally {
      database.close();
    }
    const close = vi.fn();
    mocks.connect.mockResolvedValueOnce({ close });
    mocks.bind.mockResolvedValueOnce({ generation: "renewed" });

    await provisionMcpPeerSeats([
      "--project", value.project,
      "--seat", "agy",
      "--expires-at", new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
    ], value.paths);

    // The stored metadata still records the mint-time revision; the daemon
    // compares against the live one, so the bind must carry the live value.
    expect(mocks.bind).toHaveBeenCalledWith(expect.objectContaining({
      sessionRevision: value.sessionRevision + 1,
    }), value.paths);
    expect(close).toHaveBeenCalledOnce();
    expect(mocks.release).toHaveBeenCalledOnce();
  });

  it("advises recovery when a bare request meets an expired provisioned roster", async () => {
    const past = new Date(Date.now() - 60_000).toISOString();
    const value = await fixture({ originKind: "provisioned", expiresAt: past, peerExpiresAt: past });

    await expect(provisionMcpPeerSeats([
      "--project", value.project, "--seat", "agy",
    ], value.paths)).rejects.toThrow(
      /requires an active chair seat.*expired at .*can be recovered with .*--seat claude --expires-at/u,
    );
    expect(mocks.connect).not.toHaveBeenCalled();
    expect(mocks.bind).not.toHaveBeenCalled();
    expect(mocks.release).not.toHaveBeenCalled();
  });

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
