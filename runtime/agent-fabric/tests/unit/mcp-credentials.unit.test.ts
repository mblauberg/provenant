import { chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { resolveMcpCapability, resolveRenewableMcpCapability } from "../../src/mcp/credentials.ts";
import { markLegacyBootstrapSeatGeneration } from "../../src/cli/seat-store.ts";

const cleanup: string[] = [];
const GENERATION_NEAREST = "a".repeat(64);
const GENERATION_EXPIRY = "b".repeat(64);
const GENERATION_EXPLICIT = "c".repeat(64);
const GENERATION_MIGRATED = "d".repeat(64);

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function createCurrentSeatDirectory(
  stateDirectory: string,
  projectPath: string,
  generation: string,
  previousGeneration: string | null = null,
): Promise<{ key: string; directory: string }> {
  const key = createHash("sha256").update(projectPath).digest("hex").slice(0, 24);
  const seatRoot = join(stateDirectory, "seats", key);
  const directory = join(seatRoot, "generations", generation);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  await writeFile(join(seatRoot, "current.json"), `${JSON.stringify({
    schemaVersion: 1,
    projectKey: key,
    previousGeneration,
    generation,
  })}\n`, { mode: 0o600 });
  return { key, directory };
}

describe("MCP capability loading", () => {
  it("names the project-seat registry variables when capability sources are ambiguous", async () => {
    await expect(resolveMcpCapability({})).rejects.toThrow(
      /AGENT_FABRIC_STATE_DIRECTORY.*AGENT_FABRIC_SEAT.*AGENT_FABRIC_CLIENT_LABEL/u,
    );
  });

  it("loads a capability from a private regular file without placing it in client configuration", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-credential-"));
    cleanup.push(directory);
    const path = join(directory, "codex.cap");
    const capability = `afc_${"a".repeat(43)}`;
    await writeFile(path, `${capability}\n`, { mode: 0o600 });

    await expect(resolveMcpCapability({ AGENT_FABRIC_CAPABILITY_FILE: path })).resolves.toBe(capability);
  });

  it("rejects ambiguous, broadly readable and symlinked credential sources", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-credential-invalid-"));
    cleanup.push(directory);
    const path = join(directory, "peer.cap");
    const link = join(directory, "peer-link.cap");
    const capability = `afc_${"b".repeat(43)}`;
    await writeFile(path, `${capability}\n`, { mode: 0o600 });
    await symlink(path, link);

    await expect(resolveMcpCapability({
      AGENT_FABRIC_CAPABILITY: capability,
      AGENT_FABRIC_CAPABILITY_FILE: path,
    })).rejects.toThrow(/exactly one/u);
    await expect(resolveMcpCapability({ AGENT_FABRIC_CAPABILITY_FILE: link })).rejects.toThrow(/regular file/u);
    await chmod(path, 0o640);
    await expect(resolveMcpCapability({ AGENT_FABRIC_CAPABILITY_FILE: path })).rejects.toThrow(/private/u);
  });

  it("derives distinct project keys from two working directories and never crosses their capabilities", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-project-seat-"));
    cleanup.push(directory);
    const stateDirectory = join(directory, "state");
    const project = join(directory, "project-a");
    const nested = join(project, "nested");
    const otherProject = join(directory, "project-b");
    const unprovisioned = join(directory, "project-c");
    await Promise.all([
      mkdir(nested, { recursive: true }),
      mkdir(otherProject, { recursive: true }),
      mkdir(unprovisioned, { recursive: true }),
      mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
    ]);
    const projectPath = await realpath(project);
    const otherProjectPath = await realpath(otherProject);
    const { key, directory: seatDirectory } = await createCurrentSeatDirectory(
      stateDirectory,
      projectPath,
      GENERATION_NEAREST,
    );
    const { key: otherKey, directory: otherSeatDirectory } = await createCurrentSeatDirectory(
      stateDirectory,
      otherProjectPath,
      GENERATION_EXPIRY,
    );
    const credentialPath = join(seatDirectory, "codex.cap");
    const capability = `afc_${"c".repeat(43)}`;
    await writeFile(credentialPath, `${capability}\n`, { mode: 0o600 });
    await writeFile(join(seatDirectory, "codex.json"), `${JSON.stringify({
      schemaVersion: 1,
      projectKey: key,
      projectPath,
      generation: GENERATION_NEAREST,
      previousGeneration: null,
      runId: "run-project-a",
      seat: "codex",
      agentId: "codex",
      role: "chair",
      credentialPath,
      expiresAt: "2026-08-01T00:00:00.000Z",
    })}\n`, { mode: 0o600 });
    const otherCredentialPath = join(otherSeatDirectory, "codex.cap");
    const otherCapability = `afc_${"f".repeat(43)}`;
    await writeFile(otherCredentialPath, `${otherCapability}\n`, { mode: 0o600 });
    await writeFile(join(otherSeatDirectory, "codex.json"), `${JSON.stringify({
      schemaVersion: 1,
      projectKey: otherKey,
      projectPath: otherProjectPath,
      generation: GENERATION_EXPIRY,
      previousGeneration: null,
      runId: "run-project-b",
      seat: "codex",
      agentId: "codex",
      role: "chair",
      credentialPath: otherCredentialPath,
      expiresAt: "2026-08-01T00:00:00.000Z",
    })}\n`, { mode: 0o600 });
    const environment = {
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
    };

    expect(key).not.toBe(otherKey);
    await expect(resolveMcpCapability(environment, nested)).resolves.toBe(capability);
    await expect(resolveMcpCapability(environment, otherProject)).resolves.toBe(otherCapability);
    await expect(resolveMcpCapability(environment, unprovisioned)).rejects.toThrow(/not provisioned/u);
  });

  it("names the runnable peer provisioning path for an absent optional seat", async () => {
    const directory = await realpath(await mkdtemp(join(tmpdir(), "fabric-mcp-peer-remedy-")));
    cleanup.push(directory);
    const stateDirectory = join(directory, "state");
    const project = join(directory, "project root");
    await Promise.all([mkdir(stateDirectory, { mode: 0o700 }), mkdir(project)]);

    await expect(resolveMcpCapability({
      AGENT_FABRIC_SEAT: "agy",
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
    }, project)).rejects.toThrow(
      `"$HOME/.agents/scripts/agent-fabric" mcp peer-provision --project '${project}' --seat agy`,
    );
  });

  it("automatically renews a near-expiry or expired project seat", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-project-seat-expiry-"));
    cleanup.push(directory);
    const stateDirectory = join(directory, "state");
    const project = join(directory, "project");
    await Promise.all([mkdir(project), mkdir(stateDirectory, { mode: 0o700 })]);
    const projectPath = await realpath(project);
    const { key, directory: seatDirectory } = await createCurrentSeatDirectory(
      stateDirectory,
      projectPath,
      GENERATION_EXPIRY,
    );
    const credentialPath = join(seatDirectory, "codex.cap");
    await writeFile(credentialPath, `afc_${"d".repeat(43)}\n`, { mode: 0o600 });
    const metadataPath = join(seatDirectory, "codex.json");
    const metadata = (expiresAt: string) => ({
      schemaVersion: 1,
      projectKey: key,
      projectPath,
      generation: GENERATION_EXPIRY,
      previousGeneration: null,
      originKind: "bootstrap",
      projectSessionId: `session_bootstrap_${"a".repeat(32)}`,
      runId: "run",
      seat: "codex",
      agentId: "codex",
      role: "chair",
      credentialPath,
      expiresAt,
    });
    const environment = { AGENT_FABRIC_SEAT: "codex", AGENT_FABRIC_STATE_DIRECTORY: stateDirectory };
    for (const expiresAt of [
      new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
      new Date(Date.now() - 1_000).toISOString(),
    ]) {
      await writeFile(metadataPath, `${JSON.stringify(metadata(expiresAt))}\n`, { mode: 0o600 });
      const renew = vi.fn(async () => {
        await writeFile(metadataPath, `${JSON.stringify(metadata(new Date(Date.now() + 24 * 60 * 60 * 1_000).toISOString()))}\n`, { mode: 0o600 });
      });
      const warn = vi.fn();
      await expect(resolveRenewableMcpCapability(environment, projectPath, renew, warn)).resolves.toMatch(/^afc_/u);
      expect(renew).toHaveBeenCalledOnce();
      expect(warn).not.toHaveBeenCalled();
    }
  });

  it.each([
    ["before the renewal window", 2 * 60 * 60 * 1_000, false],
    ["near expiry", 30 * 60 * 1_000, true],
    ["expired", -1_000, true],
  ])("handles a verified legacy bootstrap seat %s", async (_label, remainingMs, shouldRenew) => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-legacy-seat-expiry-"));
    cleanup.push(directory);
    const stateDirectory = join(directory, "state");
    const project = join(directory, "project");
    await Promise.all([mkdir(project), mkdir(stateDirectory, { mode: 0o700 })]);
    const projectPath = await realpath(project);
    const legacy = await createCurrentSeatDirectory(stateDirectory, projectPath, GENERATION_EXPIRY);
    const legacyCredentialPath = join(legacy.directory, "codex.cap");
    const legacyMetadataPath = join(legacy.directory, "codex.json");
    const legacyMetadata = `${JSON.stringify({
      schemaVersion: 1,
      projectKey: legacy.key,
      projectPath,
      generation: GENERATION_EXPIRY,
      previousGeneration: null,
      projectSessionId: "opaque-session",
      runId: "opaque-run",
      seat: "codex",
      agentId: "codex",
      role: "chair",
      credentialPath: legacyCredentialPath,
      expiresAt: new Date(Date.now() + remainingMs).toISOString(),
    })}\n`;
    await writeFile(legacyCredentialPath, `afc_${"d".repeat(43)}\n`, { mode: 0o600 });
    await writeFile(legacyMetadataPath, legacyMetadata, { mode: 0o600 });
    await markLegacyBootstrapSeatGeneration({ stateDirectory, projectPath, generation: GENERATION_EXPIRY });
    const migratedCapability = `afc_${"e".repeat(43)}`;
    const renew = vi.fn(async () => {
      const migrated = await createCurrentSeatDirectory(
        stateDirectory,
        projectPath,
        GENERATION_MIGRATED,
        GENERATION_EXPIRY,
      );
      const migratedCredentialPath = join(migrated.directory, "codex.cap");
      await writeFile(migratedCredentialPath, `${migratedCapability}\n`, { mode: 0o600 });
      await writeFile(join(migrated.directory, "codex.json"), `${JSON.stringify({
        schemaVersion: 1,
        projectKey: migrated.key,
        projectPath,
        generation: GENERATION_MIGRATED,
        previousGeneration: GENERATION_EXPIRY,
        originKind: "bootstrap",
        projectSessionId: "opaque-session",
        runId: "opaque-run",
        seat: "codex",
        agentId: "codex",
        role: "chair",
        credentialPath: migratedCredentialPath,
        expiresAt: new Date(Date.now() + 24 * 60 * 60 * 1_000).toISOString(),
      })}\n`, { mode: 0o600 });
    });

    const warn = vi.fn();
    await expect(resolveRenewableMcpCapability({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
    }, projectPath, renew, warn)).resolves.toBe(shouldRenew ? migratedCapability : `afc_${"d".repeat(43)}`);
    expect(renew).toHaveBeenCalledTimes(shouldRenew ? 1 : 0);
    expect(warn).not.toHaveBeenCalled();
    await expect(readFile(legacyMetadataPath, "utf8")).resolves.toBe(legacyMetadata);
  });

  it("keeps provisioned and unverified legacy seats non-renewing", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-operator-seat-expiry-"));
    cleanup.push(directory);
    const stateDirectory = join(directory, "state");
    const project = join(directory, "project");
    await Promise.all([mkdir(project), mkdir(stateDirectory, { mode: 0o700 })]);
    const projectPath = await realpath(project);
    const { key, directory: seatDirectory } = await createCurrentSeatDirectory(
      stateDirectory,
      projectPath,
      GENERATION_EXPIRY,
    );
    const credentialPath = join(seatDirectory, "codex.cap");
    await writeFile(credentialPath, `afc_${"d".repeat(43)}\n`, { mode: 0o600 });
    const metadataPath = join(seatDirectory, "codex.json");
    const metadata = (expiresAt: string, originKind: "provisioned" | null = "provisioned") => ({
      schemaVersion: 1,
      projectKey: key,
      projectPath,
      generation: GENERATION_EXPIRY,
      previousGeneration: null,
      ...(originKind === null ? {} : { originKind }),
      projectSessionId: `session_bootstrap_${"f".repeat(32)}`,
      runId: "run",
      seat: "codex",
      agentId: "codex",
      role: "chair",
      credentialPath,
      expiresAt,
    });
    const environment = { AGENT_FABRIC_SEAT: "codex", AGENT_FABRIC_STATE_DIRECTORY: stateDirectory };
    const renew = vi.fn(async () => undefined);
    const warn = vi.fn();
    await writeFile(metadataPath, `${JSON.stringify(metadata(new Date(Date.now() + 30 * 60 * 1_000).toISOString()))}\n`, { mode: 0o600 });
    await expect(resolveRenewableMcpCapability(environment, projectPath, renew, warn)).resolves.toMatch(/^afc_/u);
    expect(renew).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledOnce();
    await writeFile(metadataPath, `${JSON.stringify(metadata(new Date(Date.now() - 1_000).toISOString()))}\n`, { mode: 0o600 });
    await expect(resolveRenewableMcpCapability(environment, projectPath, renew, warn)).rejects.toThrow(/expired/u);
    expect(renew).not.toHaveBeenCalled();
    await writeFile(metadataPath, `${JSON.stringify(metadata(
      new Date(Date.now() + 30 * 60 * 1_000).toISOString(),
      null,
    ))}\n`, { mode: 0o600 });
    await expect(resolveRenewableMcpCapability(environment, projectPath, renew, warn)).resolves.toMatch(/^afc_/u);
    expect(renew).not.toHaveBeenCalled();
    await writeFile(metadataPath, `${JSON.stringify(metadata(
      new Date(Date.now() - 1_000).toISOString(),
      null,
    ))}\n`, { mode: 0o600 });
    await expect(resolveRenewableMcpCapability(environment, projectPath, renew, warn)).rejects.toThrow(/expired/u);
    expect(renew).not.toHaveBeenCalled();
  });

  it("binds a global registry process to an explicit provisioned project path", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mcp-explicit-project-"));
    cleanup.push(directory);
    const stateDirectory = join(directory, "state");
    const project = join(directory, "project");
    const unrelatedCwd = join(directory, "unrelated");
    await Promise.all([mkdir(project), mkdir(unrelatedCwd), mkdir(stateDirectory, { mode: 0o700 })]);
    const projectPath = await realpath(project);
    const { key, directory: seatDirectory } = await createCurrentSeatDirectory(
      stateDirectory,
      projectPath,
      GENERATION_EXPLICIT,
    );
    const credentialPath = join(seatDirectory, "codex.cap");
    await writeFile(credentialPath, `afc_${"e".repeat(43)}\n`, { mode: 0o600 });
    await writeFile(join(seatDirectory, "codex.json"), `${JSON.stringify({
      schemaVersion: 1, projectKey: key, projectPath, generation: GENERATION_EXPLICIT,
      previousGeneration: null, runId: "run", seat: "codex", agentId: "codex", role: "chair",
      credentialPath, expiresAt: "2099-01-01T00:00:00.000Z",
    })}\n`, { mode: 0o600 });
    await expect(resolveMcpCapability({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
      AGENT_FABRIC_PROJECT_PATH: projectPath,
    }, unrelatedCwd)).resolves.toMatch(/^afc_/u);
  });
});
