import { lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it, vi } from "vitest";

import {
  installSeatGeneration,
  markLegacyBootstrapSeatGeneration,
  projectKey,
  resolveSeatPaths,
} from "../../src/cli/seat-store.ts";

const CAPABILITY_A = `afc_${"a".repeat(43)}`;
const CAPABILITY_B = `afc_${"b".repeat(43)}`;
const GENERATION_ONE = "1".repeat(64);
const GENERATION_TWO = "2".repeat(64);
const GENERATION_THREE = "3".repeat(64);

describe("MCP seat generation store", () => {
  it("reports only the first durable recording of legacy bootstrap provenance", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-legacy-provenance-"));
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const key = projectKey(projectPath);
      const seatRoot = join(stateDirectory, "seats", key);
      const markerPath = join(seatRoot, "legacy-bootstrap.json");
      await mkdir(seatRoot, { recursive: true, mode: 0o700 });
      await writeFile(join(seatRoot, "current.json"), `${JSON.stringify({
        schemaVersion: 1,
        projectKey: key,
        previousGeneration: null,
        generation: GENERATION_ONE,
      })}\n`, { mode: 0o600 });

      await expect(markLegacyBootstrapSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
      })).resolves.toBe("recorded");
      const markerBefore = await readFile(markerPath, "utf8");
      const markerMtimeBefore = (await lstat(markerPath)).mtimeMs;

      await expect(markLegacyBootstrapSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
      })).resolves.toBe("already-recorded");
      await expect(readFile(markerPath, "utf8")).resolves.toBe(markerBefore);
      expect((await lstat(markerPath)).mtimeMs).toBe(markerMtimeBefore);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("rejects flat seat files when the active generation pointer is absent", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-flat-rejection-"));
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const flatDirectory = join(stateDirectory, "seats", projectKey(projectPath));
      await mkdir(flatDirectory, { recursive: true, mode: 0o700 });
      await writeFile(join(flatDirectory, "codex.cap"), CAPABILITY_A, { mode: 0o600 });
      await writeFile(join(flatDirectory, "codex.json"), "{}\n", { mode: 0o600 });

      await expect(resolveSeatPaths({
        stateDirectory,
        project: projectPath,
        seat: "codex",
      })).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("keeps the prior complete generation active when renewal fails before cutover", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-generation-"));
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const key = projectKey(projectPath);
      const metadata = {
        schemaVersion: 1 as const,
        projectKey: key,
        projectPath,
        projectSessionId: "session-one",
        sessionRevision: 1,
        sessionGeneration: 1,
        runRevision: 1,
        chairAgentId: "codex",
        chairGeneration: 1,
        chairLeaseId: "chair:run-one:1",
        seat: "codex" as const,
        agentId: "codex",
        principalGeneration: 1,
        role: "chair" as const,
        expiresAt: "2099-01-01T00:00:00.000Z",
      };
      await installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
        expectedPreviousGeneration: null,
        seats: [{ metadata: {
          ...metadata, runId: "run-one", generation: GENERATION_ONE, previousGeneration: null,
        }, credential: CAPABILITY_A }],
      });
      const before = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "codex" });

      await expect(installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_TWO,
        expectedPreviousGeneration: GENERATION_ONE,
        seats: [{ metadata: {
          ...metadata, runId: "run-two", generation: GENERATION_TWO, previousGeneration: GENERATION_ONE,
        }, credential: CAPABILITY_B }],
        beforeActivate: () => {
          throw new Error("injected cutover failure");
        },
      })).rejects.toThrow(/injected cutover failure/u);

      const after = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "codex" });
      expect(after).toEqual(before);
      await expect(readFile(after.credentialPath, "utf8")).resolves.toBe(CAPABILITY_A);
      await expect(readFile(after.metadataPath, "utf8").then(JSON.parse)).resolves.toMatchObject({ runId: "run-one" });

      await expect(installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
        expectedPreviousGeneration: null,
        seats: [{ metadata: {
          ...metadata, runId: "run-one", generation: GENERATION_ONE, previousGeneration: null,
        }, credential: CAPABILITY_B }],
      })).rejects.toThrow(/differs from requested immutable generation/u);
      await expect(readFile(after.credentialPath, "utf8")).resolves.toBe(CAPABILITY_A);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("does not let a delayed older writer roll the active generation backward", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-generation-cas-"));
    let releaseOlder!: () => void;
    const olderCanActivate = new Promise<void>((resolvePromise) => { releaseOlder = resolvePromise; });
    let olderStaged!: () => void;
    const olderIsStaged = new Promise<void>((resolvePromise) => { olderStaged = resolvePromise; });
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const key = projectKey(projectPath);
      const metadata = {
        schemaVersion: 1 as const,
        projectKey: key,
        projectPath,
        projectSessionId: "session-one",
        sessionRevision: 1,
        sessionGeneration: 1,
        runRevision: 1,
        chairAgentId: "codex",
        chairGeneration: 1,
        chairLeaseId: "chair:run-one:1",
        seat: "codex" as const,
        agentId: "codex",
        principalGeneration: 1,
        role: "chair" as const,
        expiresAt: "2099-01-01T00:00:00.000Z",
      };
      await installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
        expectedPreviousGeneration: null,
        seats: [{ metadata: {
          ...metadata, runId: "run-one", generation: GENERATION_ONE, previousGeneration: null,
        }, credential: CAPABILITY_A }],
      });

      const delayedOlder = installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_TWO,
        expectedPreviousGeneration: GENERATION_ONE,
        allowStaleGenerationReconciliation: true,
        seats: [{ metadata: {
          ...metadata, runId: "run-two", generation: GENERATION_TWO, previousGeneration: GENERATION_ONE,
        }, credential: CAPABILITY_B }],
        beforeActivate: async () => {
          olderStaged();
          await olderCanActivate;
        },
      });
      await olderIsStaged;
      await installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_THREE,
        expectedPreviousGeneration: GENERATION_ONE,
        seats: [{ metadata: {
          ...metadata, runId: "run-three", generation: GENERATION_THREE, previousGeneration: GENERATION_ONE,
        }, credential: CAPABILITY_A }],
      });
      releaseOlder();

      await expect(delayedOlder).rejects.toThrow(/active MCP seat generation changed/u);
      const active = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "codex" });
      await expect(readFile(active.metadataPath, "utf8").then(JSON.parse)).resolves.toMatchObject({ runId: "run-three" });
    } finally {
      releaseOlder?.();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("does not reconcile an expired recorded generation to an expired incoming generation", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-expired-reconciliation-"));
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const key = projectKey(projectPath);
      const metadata = {
        schemaVersion: 1 as const,
        projectKey: key,
        projectPath,
        projectSessionId: "session-expired",
        sessionRevision: 1,
        sessionGeneration: 1,
        runRevision: 1,
        chairAgentId: "codex",
        chairGeneration: 1,
        chairLeaseId: "chair:run-expired:1",
        seat: "codex" as const,
        agentId: "codex",
        principalGeneration: 1,
        role: "chair" as const,
        expiresAt: "2026-07-28T00:00:00.000Z",
      };
      await installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
        expectedPreviousGeneration: null,
        seats: [{ metadata: {
          ...metadata, runId: "run-one", generation: GENERATION_ONE, previousGeneration: null,
        }, credential: CAPABILITY_A }],
      });

      await expect(installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_TWO,
        expectedPreviousGeneration: GENERATION_THREE,
        allowStaleGenerationReconciliation: true,
        now: new Date("2026-07-29T00:00:00.000Z"),
        seats: [{ metadata: {
          ...metadata,
          runId: "run-two",
          generation: GENERATION_TWO,
          previousGeneration: GENERATION_THREE,
        }, credential: CAPABILITY_B }],
      })).rejects.toThrow(/active MCP seat generation changed/u);

      const active = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "codex" });
      expect(active.generation).toBe(GENERATION_ONE);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("does not reconcile to an incoming generation that expires while staging", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-staging-expiry-"));
    vi.useFakeTimers();
    vi.setSystemTime("2026-07-29T00:00:00.000Z");
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const key = projectKey(projectPath);
      const common = {
        schemaVersion: 1 as const,
        projectKey: key,
        projectPath,
        projectSessionId: "session-staging-expiry",
        sessionRevision: 1,
        sessionGeneration: 1,
        runRevision: 1,
        chairAgentId: "codex",
        chairGeneration: 1,
        chairLeaseId: "chair:run-staging-expiry:1",
        seat: "codex" as const,
        agentId: "codex",
        principalGeneration: 1,
        role: "chair" as const,
      };
      await installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_ONE,
        expectedPreviousGeneration: null,
        seats: [{ metadata: {
          ...common,
          runId: "run-one",
          generation: GENERATION_ONE,
          previousGeneration: null,
          expiresAt: "2026-07-28T00:00:00.000Z",
        }, credential: CAPABILITY_A }],
      });

      await expect(installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_TWO,
        expectedPreviousGeneration: GENERATION_THREE,
        allowStaleGenerationReconciliation: true,
        beforeActivate: () => {
          vi.setSystemTime("2026-07-29T00:02:00.000Z");
        },
        seats: [{ metadata: {
          ...common,
          runId: "run-two",
          generation: GENERATION_TWO,
          previousGeneration: GENERATION_THREE,
          expiresAt: "2026-07-29T00:01:00.000Z",
        }, credential: CAPABILITY_B }],
      })).rejects.toThrow(/active MCP seat generation changed/u);

      const active = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "codex" });
      expect(active.generation).toBe(GENERATION_ONE);
    } finally {
      vi.useRealTimers();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("installs a daemon-attested newer generation when a crash left no local pointer", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-seat-crash-convergence-"));
    try {
      const stateDirectory = join(root, "state");
      const requestedProjectPath = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(requestedProjectPath);
      const projectPath = await realpath(requestedProjectPath);
      const key = projectKey(projectPath);
      const common = {
        schemaVersion: 1 as const,
        projectKey: key,
        projectPath,
        projectSessionId: "session-crash",
        sessionRevision: 1,
        sessionGeneration: 1,
        runId: "run-crash",
        runRevision: 1,
        chairAgentId: "codex",
        chairGeneration: 1,
        chairLeaseId: "chair:run-crash:1",
        generation: GENERATION_TWO,
        previousGeneration: GENERATION_ONE,
        principalGeneration: 1,
        expiresAt: "2099-01-01T00:00:00.000Z",
      };
      await installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_TWO,
        expectedPreviousGeneration: GENERATION_ONE,
        allowMissingPreviousGeneration: true,
        seats: [
          { metadata: { ...common, seat: "codex", agentId: "codex", role: "chair" }, credential: CAPABILITY_A },
          { metadata: { ...common, seat: "claude", agentId: "claude", role: "peer" }, credential: CAPABILITY_B },
        ],
      });

      const codex = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "codex" });
      const claude = await resolveSeatPaths({ stateDirectory, project: projectPath, seat: "claude" });
      expect(codex.generation).toBe(GENERATION_TWO);
      expect(claude.generation).toBe(GENERATION_TWO);
      await expect(readFile(codex.credentialPath, "utf8")).resolves.toBe(CAPABILITY_A);
      await expect(readFile(claude.credentialPath, "utf8")).resolves.toBe(CAPABILITY_B);

      await expect(installSeatGeneration({
        stateDirectory,
        projectPath,
        generation: GENERATION_THREE,
        expectedPreviousGeneration: GENERATION_ONE,
        allowMissingPreviousGeneration: true,
        seats: [{
          metadata: {
            ...common,
            generation: GENERATION_THREE,
            seat: "codex",
            agentId: "codex",
            role: "chair",
          },
          credential: CAPABILITY_A,
        }],
      })).rejects.toThrow(/active MCP seat generation changed/u);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});
