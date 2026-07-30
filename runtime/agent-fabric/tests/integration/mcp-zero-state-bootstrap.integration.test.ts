import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { access, mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { Fabric } from "../../src/core/fabric.ts";
import { bootstrapMcpSeat } from "../../src/cli/mcp-bootstrap.ts";
import { peerSeatAuthority } from "../../src/cli/observer-provision.ts";
import { installSeatGeneration, projectKey, readActiveSeatGeneration, resolveSeatPaths } from "../../src/cli/seat-store.ts";
import { FABRIC_OPERATIONS } from "../../src/domain/operations.ts";

const roots: string[] = [];
const execFileAsync = promisify(execFile);
const cliMain = fileURLToPath(new URL("../../src/cli/main.ts", import.meta.url));
const tsxLoader = fileURLToPath(import.meta.resolve("tsx"));

function canonicalFixtureJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalFixtureJson).join(",")}]`;
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map(
      (key) => `${JSON.stringify(key)}:${canonicalFixtureJson(record[key])}`,
    ).join(",")}}`;
  }
  throw new TypeError("fixture value is not JSON-compatible");
}

function fixtureMcpSeatGeneration(identity: {
  bindings: Array<{ seat: string; agentId: string; expectedPrincipalGeneration: number }>;
  [key: string]: unknown;
}): { generation: string; bindingJson: string } {
  const bindingJson = canonicalFixtureJson({
    ...identity,
    bindings: identity.bindings.slice().sort((left, right) => left.seat.localeCompare(right.seat)),
  });
  return {
    generation: createHash("sha256").update(bindingJson).digest("hex"),
    bindingJson,
  };
}

afterEach(async () => {
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("zero-state MCP bootstrap", () => {
  it("keeps the independent generation fixture anchored to a fixed digest", () => {
    expect(fixtureMcpSeatGeneration({ bindings: [] }).generation).toBe(
      "b0005ba41f01f372d3fc486c12cd3ac2279c9e688ba7d12c653b57bc50e7ecb2",
    );
  });

  it("does not create state directories while inspecting a missing bootstrap", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-inspect-missing-"));
    roots.push(temporaryRoot);
    const stateDirectory = join(temporaryRoot, "missing-state");

    await expect(execFileAsync(process.execPath, [
      "--import", tsxLoader, cliMain, "bootstrap", "--inspect", "--seat", "codex",
    ], {
      cwd: temporaryRoot,
      env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: stateDirectory },
    })).rejects.toMatchObject({ code: 1 });
    await expect(access(stateDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects a bootstrap seat expiry beyond the fixed 24-hour bound", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-overlong-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const fabric = new Fabric({
      databasePath: join(root, "fabric.sqlite3"),
      workspaceRoots: [root],
      clock: () => Date.parse("2026-07-18T00:00:00.000Z"),
    });
    try {
      expect(() => fabric.bootstrapCurrentMcpSeat({
        canonicalRoot: root,
        trustRecordDigest: `sha256:${"d".repeat(64)}`,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.001Z",
      })).toThrow(expect.objectContaining({ code: "AUTHENTICATION_FAILED" }));
    } finally {
      await fabric.close();
    }
  });

  it("rejects an untrusted exact root before daemon discovery", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-untrusted-bootstrap-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const stateDirectory = join(root, "state");
    await expect(bootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_PRODUCT_ROOT: join(root, "product"),
      },
      cwd: root,
      paths: {
        stateDirectory,
        runtimeDirectory: join(root, "runtime"),
        databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
        socketPath: join(root, "runtime", "fabric-v1.sock"),
      },
    })).rejects.toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: `Fabric bootstrap requires the exact current project root to be trusted; run '${join(root, "product", "scripts", "agent-fabric")}' workspace trust '${root}'; then retry fabric_bootstrap`,
    });
  });

  it("emits an executable recovery command for a spaced home and exact root", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-spaced-bootstrap-"));
    roots.push(temporaryRoot);
    const home = join(temporaryRoot, "operator home");
    const project = join(temporaryRoot, "project root");
    const launcher = join(home, ".agents", "scripts", "agent-fabric");
    await mkdir(join(home, ".agents", "scripts"), { recursive: true });
    await mkdir(project);
    await writeFile(launcher, "#!/bin/sh\nprintf '%s' \"$3\"\n", { mode: 0o700 });
    let message = "";
    try {
      await bootstrapMcpSeat({
        environment: {
          AGENT_FABRIC_SEAT: "codex",
          AGENT_FABRIC_PRODUCT_ROOT: join(home, ".agents"),
        },
        cwd: project,
        paths: {
          stateDirectory: join(temporaryRoot, "state"),
          runtimeDirectory: join(temporaryRoot, "runtime"),
          databasePath: join(temporaryRoot, "state", "fabric-v1.sqlite3"),
          socketPath: join(temporaryRoot, "runtime", "fabric-v1.sock"),
        },
      });
    } catch (error: unknown) {
      if (error instanceof Error) message = error.message;
    }
    const recovery = /; run (?<command>.+); then retry fabric_bootstrap$/u.exec(message)?.groups?.command;
    expect(recovery).toBeDefined();

    const result = await execFileAsync("/bin/sh", ["-c", recovery!], {
      env: { ...process.env, HOME: home },
    });
    expect(result.stdout).toBe(await realpath(project));
  });

  it("creates one deterministic scoping run and converges a second primary into its peer seat", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const stateDirectory = join(root, "seat-state");
    await mkdir(stateDirectory, { mode: 0o700 });
    const databasePath = join(stateDirectory, "fabric-v1.sqlite3");
    const now = Date.parse("2026-07-18T00:00:00.000Z");
    const fabric = new Fabric({ databasePath, workspaceRoots: [root], clock: () => now });
    const base = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"a".repeat(64)}`,
      expiresAt: "2026-07-19T00:00:00.000Z",
    } as const;

    const first = fabric.bootstrapCurrentMcpSeat({ ...base, seat: "codex" });
    const replay = fabric.bootstrapCurrentMcpSeat({ ...base, seat: "codex" });
    const second = fabric.bootstrapCurrentMcpSeat({ ...base, seat: "claude" });

    expect(replay).toEqual(first);
    expect(first.credentials).toHaveLength(1);
    expect(first.credentials[0]?.authorityId).toMatch(/^bootstrap-authority:[a-f0-9]{64}:codex$/u);
    expect(second.credentials.map(({ seat }) => seat).sort()).toEqual(["claude", "codex"]);
    expect(second.runId).toBe(first.runId);
    expect(second.chairAgentId).toBe(first.chairAgentId);
    expect(second.expectedPreviousGeneration).toBe(first.generation);
    await expect(fabric.connect(second.credentials.find(({ seat }) => seat === "claude")!.capability).whoami())
      .resolves.toEqual({
        seat: "claude",
        agentId: second.credentials.find(({ seat }) => seat === "claude")!.agentId,
        runId: second.runId,
        authorityId: second.credentials.find(({ seat }) => seat === "claude")!.authorityId,
        generation: second.generation,
        lease: {
          leaseId: second.chairLeaseId,
          holderAgentId: second.chairAgentId,
          generation: second.chairGeneration,
          state: "active",
        },
      });

    const key = projectKey(root);
    await installSeatGeneration({
      stateDirectory,
      projectPath: root,
      generation: second.generation,
      expectedPreviousGeneration: second.expectedPreviousGeneration,
      allowMissingPreviousGeneration: true,
      seats: second.credentials.map((binding) => ({
        credential: binding.capability,
        metadata: {
          schemaVersion: 1,
          projectKey: key,
          projectPath: root,
          generation: second.generation,
          previousGeneration: second.expectedPreviousGeneration,
          projectSessionId: second.projectSessionId,
          sessionRevision: second.sessionRevision,
          sessionGeneration: second.sessionGeneration,
          runId: second.runId,
          runRevision: second.runRevision,
          chairAgentId: second.chairAgentId,
          chairGeneration: second.chairGeneration,
          chairLeaseId: second.chairLeaseId,
          seat: binding.seat as "claude" | "codex",
          agentId: binding.agentId,
          principalGeneration: binding.expectedPrincipalGeneration,
          role: binding.agentId === second.chairAgentId ? "chair" : "peer",
          expiresAt: second.expiresAt,
        },
      })),
    });
    expect((await resolveSeatPaths({ stateDirectory, project: root, seat: "codex" })).generation).toBe(second.generation);
    expect((await resolveSeatPaths({ stateDirectory, project: root, seat: "claude" })).generation).toBe(second.generation);
    const beforeInspect = await readActiveSeatGeneration({ stateDirectory, projectPath: root });
    const inspected = await execFileAsync(process.execPath, [
      "--import", tsxLoader, cliMain, "bootstrap", "--inspect", "--seat", "claude",
    ], {
      cwd: root,
      env: {
        ...process.env,
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_SEAT: "claude",
      },
    });
    expect(JSON.parse(inspected.stdout)).toEqual({
      seat: "claude",
      agentId: second.credentials.find(({ seat }) => seat === "claude")!.agentId,
      runId: second.runId,
      authorityId: second.credentials.find(({ seat }) => seat === "claude")!.authorityId,
      generation: second.generation,
      lease: {
        leaseId: second.chairLeaseId,
        holderAgentId: second.chairAgentId,
        generation: second.chairGeneration,
        state: "active",
      },
    });
    await expect(readActiveSeatGeneration({ stateDirectory, projectPath: root })).resolves.toEqual(beforeInspect);

    const database = new Database(databasePath, { readonly: true });
    try {
      expect(database.prepare("SELECT count(*) AS count FROM projects").get()).toEqual({ count: 1 });
      expect(database.prepare("SELECT count(*) AS count FROM project_sessions").get()).toEqual({ count: 1 });
      expect(database.prepare("SELECT count(*) AS count FROM runs").get()).toEqual({ count: 1 });
      expect(database.prepare("SELECT count(*) AS count FROM agents").get()).toEqual({ count: 2 });
      expect(database.prepare("SELECT count(*) AS count FROM mcp_active_seat_generations").get()).toEqual({ count: 1 });
    } finally {
      database.close();
      await fabric.close();
    }
  });

  it("replays an active tagged generation with its stored immutable identity after session revision advances", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-revision-replay-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const databasePath = join(root, "fabric.sqlite3");
    const now = Date.parse("2026-07-18T00:00:00.000Z");
    const fabric = new Fabric({ databasePath, workspaceRoots: [root], clock: () => now });
    const request = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"e".repeat(64)}`,
      seat: "codex" as const,
      expiresAt: "2026-07-19T00:00:00.000Z",
    };

    try {
      const first = fabric.bootstrapCurrentMcpSeat(request);
      const database = new Database(databasePath);
      try {
        database.prepare("UPDATE project_sessions SET revision=revision+1 WHERE project_session_id=?")
          .run(first.projectSessionId);
      } finally {
        database.close();
      }

      expect(fabric.bootstrapCurrentMcpSeat(request)).toEqual(first);
    } finally {
      await fabric.close();
    }
  });

  it("renews an expiring bootstrap roster in place and revokes its predecessor", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-renewal-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    let now = Date.parse("2026-07-18T00:00:00.000Z");
    const databasePath = join(root, "fabric.sqlite3");
    const fabric = new Fabric({
      databasePath,
      workspaceRoots: [root],
      clock: () => now,
    });
    const trust = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"b".repeat(64)}`,
    } as const;

    try {
      const first = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      });
      const roster = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "claude",
        expiresAt: first.expiresAt,
      });
      const predecessor = roster.credentials.find(({ seat }) => seat === "codex")?.capability;
      expect(predecessor).toBeDefined();
      const childAgentId = "delegated-reviewer";
      const childAuthorityId = "delegated-reviewer-authority";
      const childCapability = `afc_${"z".repeat(43)}`;
      const setup = new Database(databasePath);
      try {
        const chairAuthority = setup.prepare(`
          SELECT authority_id,authority_json,authority_hash FROM authorities
           WHERE run_id=? AND parent_authority_id IS NULL
        `).get(roster.runId) as { authority_id: string; authority_json: string; authority_hash: string };
        setup.prepare(`
          INSERT INTO authorities(authority_id,run_id,parent_authority_id,authority_json,authority_hash,created_at)
          VALUES (?,?,?,?,?,?)
        `).run(
          childAuthorityId,
          roster.runId,
          chairAuthority.authority_id,
          chairAuthority.authority_json,
          chairAuthority.authority_hash,
          now,
        );
        setup.prepare(`
          INSERT INTO agents(run_id,agent_id,parent_agent_id,authority_id,provider_session_ref,lifecycle)
          VALUES (?,?,?,?,NULL,'ready')
        `).run(roster.runId, childAgentId, roster.chairAgentId, childAuthorityId);
        setup.prepare(`
          INSERT INTO capabilities(token_hash,run_id,agent_id,principal_generation,expires_at)
          VALUES (?,?,?,1,?)
        `).run(
          createHash("sha256").update(childCapability).digest("hex"),
          roster.runId,
          childAgentId,
          Date.parse((JSON.parse(chairAuthority.authority_json) as { expiresAt: string }).expiresAt),
        );
      } finally {
        setup.close();
      }

      now = Date.parse("2026-07-18T23:30:00.000Z");
      const renewalExpiresAt = "2026-07-19T23:30:00.000Z";
      const legacyTwoSeatIdentity = fixtureMcpSeatGeneration({
        canonicalRoot: root,
        projectSessionId: roster.projectSessionId,
        sessionRevision: roster.sessionRevision,
        sessionGeneration: roster.sessionGeneration,
        runId: roster.runId,
        runRevision: roster.runRevision,
        chairAgentId: roster.chairAgentId,
        chairGeneration: roster.chairGeneration,
        chairLeaseId: roster.chairLeaseId,
        expiresAt: renewalExpiresAt,
        bindings: roster.credentials.map(({ seat, agentId, expectedPrincipalGeneration }) => ({
          seat,
          agentId,
          expectedPrincipalGeneration,
        })),
      });
      const renewed = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: renewalExpiresAt,
      });

      const renewedIdentity = fixtureMcpSeatGeneration({
        canonicalRoot: root,
        projectSessionId: renewed.projectSessionId,
        sessionRevision: renewed.sessionRevision,
        sessionGeneration: renewed.sessionGeneration,
        runId: renewed.runId,
        runRevision: renewed.runRevision,
        chairAgentId: renewed.chairAgentId,
        chairGeneration: renewed.chairGeneration,
        chairLeaseId: renewed.chairLeaseId,
        expiresAt: renewed.expiresAt,
        bindings: renewed.credentials.map(({ seat, agentId, expectedPrincipalGeneration }) => ({
          seat,
          agentId,
          expectedPrincipalGeneration,
        })),
      });
      expect(renewedIdentity.bindingJson).toBe(legacyTwoSeatIdentity.bindingJson);
      expect(renewed.generation).toBe(legacyTwoSeatIdentity.generation);
      expect(renewed.generation).not.toBe(roster.generation);
      expect(renewed.expectedPreviousGeneration).toBe(roster.generation);
      expect(renewed.projectSessionId).toBe(roster.projectSessionId);
      expect(renewed.runId).toBe(roster.runId);
      expect(renewed.chairAgentId).toBe(roster.chairAgentId);
      expect(() => fabric.connect(predecessor!)).toThrow(expect.objectContaining({ code: "AUTHENTICATION_FAILED" }));
      expect(fabric.connect(renewed.credentials.find(({ seat }) => seat === "codex")!.capability)).toBeDefined();

      const database = new Database(databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT count(*) AS count FROM capabilities
           WHERE revoked_at IS NOT NULL AND token_hash IN (
             SELECT token_hash FROM mcp_seat_generation_members WHERE generation=?
           )
        `).get(roster.generation)).toEqual({ count: 2 });
        expect(database.prepare(`
          SELECT authority_id FROM agents WHERE run_id=? AND agent_id=?
        `).get(roster.runId, childAgentId)).toEqual({ authority_id: childAuthorityId });
        expect(database.prepare(`
          SELECT revoked_at FROM capabilities WHERE token_hash=?
        `).get(createHash("sha256").update(childCapability).digest("hex"))).toEqual({ revoked_at: null });
      } finally {
        database.close();
      }
    } finally {
      await fabric.close();
    }
  });

  it("does not resurrect a legacy peer that is absent from the active roster", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-retired-peer-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    let now = Date.parse("2026-07-18T00:00:00.000Z");
    const databasePath = join(root, "fabric.sqlite3");
    const fabric = new Fabric({ databasePath, workspaceRoots: [root], clock: () => now });
    const trust = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"c".repeat(64)}`,
    } as const;
    try {
      const roster = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      });
      const identityDigest = createHash("sha256").update(canonicalFixtureJson({
        kind: "mcp-zero-state-v1",
        canonicalRoot: root,
      })).digest("hex");
      const legacyAgentId = `claude_bootstrap_peer_${identityDigest.slice(0, 16)}`;
      const database = new Database(databasePath);
      try {
        const chairAuthority = database.prepare(`
          SELECT authority_id,authority_json,authority_hash FROM authorities
           WHERE run_id=? AND parent_authority_id IS NULL
        `).get(roster.runId) as { authority_id: string; authority_json: string; authority_hash: string };
        const legacyAuthorityId = `bootstrap-authority:${identityDigest}:claude`;
        database.prepare(`
          INSERT INTO authorities(authority_id,run_id,parent_authority_id,authority_json,authority_hash,created_at)
          VALUES (?,?,?,?,?,?)
        `).run(
          legacyAuthorityId,
          roster.runId,
          chairAuthority.authority_id,
          chairAuthority.authority_json,
          chairAuthority.authority_hash,
          now,
        );
        database.prepare(`
          INSERT INTO agents(run_id,agent_id,parent_agent_id,authority_id,provider_session_ref,lifecycle)
          VALUES (?,?,?,?,NULL,'ready')
        `).run(roster.runId, legacyAgentId, roster.chairAgentId, legacyAuthorityId);
        database.prepare(`
          INSERT INTO capabilities(token_hash,run_id,agent_id,principal_generation,expires_at)
          VALUES (?,?,?,1,?)
        `).run(
          createHash("sha256").update(`legacy-${legacyAgentId}`).digest("hex"),
          roster.runId,
          legacyAgentId,
          Date.parse((JSON.parse(chairAuthority.authority_json) as { expiresAt: string }).expiresAt),
        );
      } finally {
        database.close();
      }

      now = Date.parse("2026-07-18T23:30:00.000Z");
      const renewed = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T23:30:00.000Z",
      });

      expect(renewed.credentials.map(({ seat }) => seat)).toEqual(["codex"]);
      expect(renewed.droppedSeats).toBeUndefined();
    } finally {
      await fabric.close();
    }
  });

  it("keeps the active agent when renewing an explicitly requested occupied seat", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-occupied-seat-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    let now = Date.parse("2026-07-18T00:00:00.000Z");
    const databasePath = join(root, "fabric.sqlite3");
    const fabric = new Fabric({ databasePath, workspaceRoots: [root], clock: () => now });
    const trust = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"d".repeat(64)}`,
    } as const;
    try {
      const roster = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      });
      const database = new Database(databasePath, { readonly: true });
      const chairAuthority = database.prepare(`
        SELECT authority_id,authority_json FROM authorities
         WHERE run_id=? AND parent_authority_id IS NULL
      `).get(roster.runId) as { authority_id: string; authority_json: string };
      database.close();
      const activeClaudeAgentId = `claude_bootstrap_peer_${"a".repeat(16)}`;
      const delegated = fabric.delegateAuthority(roster.runId, roster.chairAgentId, {
        parentAuthorityId: chairAuthority.authority_id,
        authority: JSON.parse(chairAuthority.authority_json),
        commandId: "peer-seat:test:active-claude",
      });
      fabric.registerAgent(roster.runId, roster.chairAgentId, {
        agentId: activeClaudeAgentId,
        authorityId: delegated.authorityId,
      });
      const bindings = [
        ...roster.credentials.map(({ seat, agentId, expectedPrincipalGeneration }) => ({
          seat,
          agentId,
          expectedPrincipalGeneration,
        })),
        { seat: "claude", agentId: activeClaudeAgentId, expectedPrincipalGeneration: 1 },
      ];
      const provisionedIdentity = fixtureMcpSeatGeneration({
        canonicalRoot: root,
        projectSessionId: roster.projectSessionId,
        sessionRevision: roster.sessionRevision,
        sessionGeneration: roster.sessionGeneration,
        runId: roster.runId,
        runRevision: roster.runRevision,
        chairAgentId: roster.chairAgentId,
        chairGeneration: roster.chairGeneration,
        chairLeaseId: roster.chairLeaseId,
        expiresAt: roster.expiresAt,
        bindings,
      });
      fabric.bindCurrentMcpSeats({
        canonicalRoot: root,
        expectedPreviousGeneration: roster.generation,
        generation: provisionedIdentity.generation,
        projectSessionId: roster.projectSessionId,
        expectedSessionRevision: roster.sessionRevision,
        expectedSessionGeneration: roster.sessionGeneration,
        runId: roster.runId,
        expectedRunRevision: roster.runRevision,
        chairAgentId: roster.chairAgentId,
        expectedChairGeneration: roster.chairGeneration,
        chairLeaseId: roster.chairLeaseId,
        expiresAt: roster.expiresAt,
        bindings,
      });

      now = Date.parse("2026-07-18T23:30:00.000Z");
      const renewed = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "claude",
        expiresAt: "2026-07-19T23:30:00.000Z",
      });

      expect(renewed.credentials.find(({ seat }) => seat === "claude")?.agentId).toBe(activeClaudeAgentId);
      const verification = new Database(databasePath, { readonly: true });
      try {
        expect(verification.prepare("SELECT count(*) AS count FROM agents WHERE run_id=?").get(roster.runId))
          .toEqual({ count: 2 });
      } finally {
        verification.close();
      }
    } finally {
      await fabric.close();
    }
  });

  it("preserves a live provisioned peer when renewing the bootstrap pair", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-peer-renewal-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    let now = Date.parse("2026-07-18T00:00:00.000Z");
    const databasePath = join(root, "fabric.sqlite3");
    const fabric = new Fabric({ databasePath, workspaceRoots: [root], clock: () => now });
    const trust = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"e".repeat(64)}`,
    } as const;
    try {
      const first = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      });
      const roster = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "claude",
        expiresAt: first.expiresAt,
      });
      const setup = new Database(databasePath);
      const chairAuthority = setup.prepare(`
        SELECT authority_id,authority_json FROM authorities
         WHERE run_id=? AND parent_authority_id IS NULL
      `).get(roster.runId) as { authority_id: string; authority_json: string };
      setup.close();
      const agyAgentId = `agy_bootstrap_peer_${"e".repeat(16)}`;
      const delegated = fabric.delegateAuthority(roster.runId, roster.chairAgentId, {
        parentAuthorityId: chairAuthority.authority_id,
        authority: JSON.parse(chairAuthority.authority_json),
        commandId: "peer-seat:test:agy",
      });
      fabric.registerAgent(roster.runId, roster.chairAgentId, {
        agentId: agyAgentId,
        authorityId: delegated.authorityId,
      });
      const cursorAgentId = `cursor_bootstrap_peer_${"f".repeat(16)}`;
      const cursorAuthority = {
        ...JSON.parse(chairAuthority.authority_json),
        expiresAt: roster.expiresAt,
      };
      const delegatedCursor = fabric.delegateAuthority(roster.runId, roster.chairAgentId, {
        parentAuthorityId: chairAuthority.authority_id,
        authority: cursorAuthority,
        commandId: "peer-seat:test:cursor",
      });
      fabric.registerAgent(roster.runId, roster.chairAgentId, {
        agentId: cursorAgentId,
        authorityId: delegatedCursor.authorityId,
      });
      const bindings = [
        ...roster.credentials.map(({ seat, agentId, expectedPrincipalGeneration }) => ({
          seat,
          agentId,
          expectedPrincipalGeneration,
        })),
        { seat: "agy", agentId: agyAgentId, expectedPrincipalGeneration: 1 },
        { seat: "cursor", agentId: cursorAgentId, expectedPrincipalGeneration: 1 },
      ].sort((left, right) => left.seat.localeCompare(right.seat));
      const provisionedIdentity = fixtureMcpSeatGeneration({
        canonicalRoot: root,
        projectSessionId: roster.projectSessionId,
        sessionRevision: roster.sessionRevision,
        sessionGeneration: roster.sessionGeneration,
        runId: roster.runId,
        runRevision: roster.runRevision,
        chairAgentId: roster.chairAgentId,
        chairGeneration: roster.chairGeneration,
        chairLeaseId: roster.chairLeaseId,
        expiresAt: roster.expiresAt,
        bindings,
      });
      const provisioned = fabric.bindCurrentMcpSeats({
        canonicalRoot: root,
        expectedPreviousGeneration: roster.generation,
        generation: provisionedIdentity.generation,
        projectSessionId: roster.projectSessionId,
        expectedSessionRevision: roster.sessionRevision,
        expectedSessionGeneration: roster.sessionGeneration,
        runId: roster.runId,
        expectedRunRevision: roster.runRevision,
        chairAgentId: roster.chairAgentId,
        expectedChairGeneration: roster.chairGeneration,
        chairLeaseId: roster.chairLeaseId,
        expiresAt: roster.expiresAt,
        bindings,
      });
      expect(provisioned.credentials.map(({ seat }) => seat)).toEqual(["agy", "claude", "codex", "cursor"]);
      expect(fabric.rotateCapability(roster.runId, roster.chairAgentId, {
        agentId: agyAgentId,
        expectedPrincipalGeneration: 1,
        commandId: "peer-seat:test:rotate-agy",
      }).principalGeneration).toBe(2);

      now = Date.parse("2026-07-18T23:30:00.000Z");
      const renewed = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T23:30:00.000Z",
      });

      expect(renewed.credentials.map(({ seat }) => seat)).toEqual(["agy", "claude", "codex"]);
      expect(renewed.credentials.find(({ seat }) => seat === "agy")?.expectedPrincipalGeneration).toBe(2);
      expect(renewed.expectedPreviousGeneration).toBe(provisioned.generation);
      expect(renewed.droppedSeats).toEqual([{
        seat: "cursor",
        agentId: cursorAgentId,
        reason: "AUTHORITY_EXPIRES_BEFORE_RENEWAL",
      }]);

      const revoke = new Database(databasePath);
      try {
        revoke.prepare("UPDATE capabilities SET revoked_at=? WHERE run_id=? AND agent_id=?")
          .run(now, roster.runId, agyAgentId);
      } finally {
        revoke.close();
      }
      now = Date.parse("2026-07-19T23:00:00.000Z");
      const withoutStalePeer = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-20T23:00:00.000Z",
      });
      expect(withoutStalePeer.credentials.map(({ seat }) => seat)).toEqual(["claude", "codex"]);
      expect(withoutStalePeer.droppedSeats).toEqual([{
        seat: "agy",
        agentId: agyAgentId,
        reason: "AGENT_NOT_LIVE",
      }]);
    } finally {
      await fabric.close();
    }
  });

  it("keeps authority widening, revoked peer capability, and revoked chair lease in daemon enforcement", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-peer-guards-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const now = Date.parse("2026-07-18T00:00:00.000Z");
    const databasePath = join(root, "fabric.sqlite3");
    const fabric = new Fabric({ databasePath, workspaceRoots: [root], clock: () => now });
    try {
      const bootstrap = fabric.bootstrapCurrentMcpSeat({
        canonicalRoot: root,
        trustRecordDigest: `sha256:${"f".repeat(64)}`,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      });
      const database = new Database(databasePath);
      const chairAuthority = database.prepare(`
        SELECT authority_id,authority_json FROM authorities
         WHERE run_id=? AND parent_authority_id IS NULL
      `).get(bootstrap.runId) as { authority_id: string; authority_json: string };
      const parentAuthority = JSON.parse(chairAuthority.authority_json);
      const narrowPeer = peerSeatAuthority(parentAuthority);
      const handWidened = {
        ...narrowPeer,
        actions: [...narrowPeer.actions, FABRIC_OPERATIONS.observeEvents],
      };
      expect(() => fabric.delegateAuthority(bootstrap.runId, bootstrap.chairAgentId, {
        parentAuthorityId: chairAuthority.authority_id,
        authority: handWidened,
        commandId: "peer-seat:test:widened",
      })).toThrow(expect.objectContaining({ code: "AUTHORITY_WIDENING" }));

      const delegated = fabric.delegateAuthority(bootstrap.runId, bootstrap.chairAgentId, {
        parentAuthorityId: chairAuthority.authority_id,
        authority: peerSeatAuthority(parentAuthority),
        commandId: "peer-seat:test:revoked-capability",
      });
      const agyAgentId = `agy_bootstrap_peer_${"f".repeat(16)}`;
      fabric.registerAgent(bootstrap.runId, bootstrap.chairAgentId, {
        agentId: agyAgentId,
        authorityId: delegated.authorityId,
      });
      database.prepare("UPDATE capabilities SET revoked_at=? WHERE run_id=? AND agent_id=?")
        .run(now, bootstrap.runId, agyAgentId);
      const peerBindings = [
        ...bootstrap.credentials.map(({ seat, agentId, expectedPrincipalGeneration }) => ({
          seat,
          agentId,
          expectedPrincipalGeneration,
        })),
        { seat: "agy", agentId: agyAgentId, expectedPrincipalGeneration: 1 },
      ];
      const peerIdentity = fixtureMcpSeatGeneration({
        canonicalRoot: root,
        projectSessionId: bootstrap.projectSessionId,
        sessionRevision: bootstrap.sessionRevision,
        sessionGeneration: bootstrap.sessionGeneration,
        runId: bootstrap.runId,
        runRevision: bootstrap.runRevision,
        chairAgentId: bootstrap.chairAgentId,
        chairGeneration: bootstrap.chairGeneration,
        chairLeaseId: bootstrap.chairLeaseId,
        expiresAt: bootstrap.expiresAt,
        bindings: peerBindings,
      });
      expect(() => fabric.bindCurrentMcpSeats({
        canonicalRoot: root,
        expectedPreviousGeneration: bootstrap.generation,
        generation: peerIdentity.generation,
        projectSessionId: bootstrap.projectSessionId,
        expectedSessionRevision: bootstrap.sessionRevision,
        expectedSessionGeneration: bootstrap.sessionGeneration,
        runId: bootstrap.runId,
        expectedRunRevision: bootstrap.runRevision,
        chairAgentId: bootstrap.chairAgentId,
        expectedChairGeneration: bootstrap.chairGeneration,
        chairLeaseId: bootstrap.chairLeaseId,
        expiresAt: bootstrap.expiresAt,
        bindings: peerBindings,
      })).toThrow(/current MCP agent .* not found/u);

      database.prepare("UPDATE run_chair_leases SET status='revoked' WHERE lease_id=?")
        .run(bootstrap.chairLeaseId);
      const revokedLeaseExpiresAt = "2026-07-18T23:59:00.000Z";
      const revokedLeaseIdentity = fixtureMcpSeatGeneration({
        canonicalRoot: root,
        projectSessionId: bootstrap.projectSessionId,
        sessionRevision: bootstrap.sessionRevision,
        sessionGeneration: bootstrap.sessionGeneration,
        runId: bootstrap.runId,
        runRevision: bootstrap.runRevision,
        chairAgentId: bootstrap.chairAgentId,
        chairGeneration: bootstrap.chairGeneration,
        chairLeaseId: bootstrap.chairLeaseId,
        expiresAt: revokedLeaseExpiresAt,
        bindings: bootstrap.credentials,
      });
      expect(() => fabric.bindCurrentMcpSeats({
        canonicalRoot: root,
        expectedPreviousGeneration: bootstrap.generation,
        generation: revokedLeaseIdentity.generation,
        projectSessionId: bootstrap.projectSessionId,
        expectedSessionRevision: bootstrap.sessionRevision,
        expectedSessionGeneration: bootstrap.sessionGeneration,
        runId: bootstrap.runId,
        expectedRunRevision: bootstrap.runRevision,
        chairAgentId: bootstrap.chairAgentId,
        expectedChairGeneration: bootstrap.chairGeneration,
        chairLeaseId: bootstrap.chairLeaseId,
        expiresAt: revokedLeaseExpiresAt,
        bindings: bootstrap.credentials,
      })).toThrow(expect.objectContaining({ code: "LIFECYCLE_PRECONDITION_FAILED" }));
      database.close();
    } finally {
      await fabric.close();
    }
  });

  it("renews an expired single-seat roster before adding the other primary", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-zero-state-transitional-renewal-"));
    roots.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    let now = Date.parse("2026-07-18T00:00:00.000Z");
    const fabric = new Fabric({
      databasePath: join(root, "fabric.sqlite3"),
      workspaceRoots: [root],
      clock: () => now,
    });
    const trust = {
      canonicalRoot: root,
      trustRecordDigest: `sha256:${"c".repeat(64)}`,
    } as const;

    try {
      const codexOnly = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      });
      const predecessor = codexOnly.credentials[0]!.capability;
      now = Date.parse("2026-07-19T00:30:00.000Z");

      const renewed = fabric.bootstrapCurrentMcpSeat({
        ...trust,
        seat: "claude",
        expiresAt: "2026-07-20T00:30:00.000Z",
      });

      expect(renewed.projectSessionId).toBe(codexOnly.projectSessionId);
      expect(renewed.runId).toBe(codexOnly.runId);
      expect(renewed.chairAgentId).toBe(codexOnly.chairAgentId);
      expect(renewed.expectedPreviousGeneration).toBe(codexOnly.generation);
      expect(renewed.credentials.map(({ seat }) => seat).sort()).toEqual(["claude", "codex"]);
      expect(() => fabric.connect(predecessor)).toThrow(expect.objectContaining({ code: "AUTHENTICATION_FAILED" }));
      expect(fabric.connect(renewed.credentials.find(({ seat }) => seat === "claude")!.capability)).toBeDefined();
    } finally {
      await fabric.close();
    }
  });
});
