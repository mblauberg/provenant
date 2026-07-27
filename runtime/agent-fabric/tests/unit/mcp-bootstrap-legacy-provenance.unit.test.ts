import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import type { BootstrapMcpSeatResult } from "../../src/core/contracts.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";

const daemon = vi.hoisted((): { result: BootstrapMcpSeatResult | undefined } => ({
  result: undefined,
}));

vi.mock("../../src/daemon/client.js", () => ({
  startFabricDaemon: vi.fn(async () => ({
    address: { path: join(tmpdir(), "fabric-bootstrap-receipt-no-daemon.sock") },
    bootstrapCapability: "unused-bootstrap-capability",
    ownsProcess: false,
    pid: 4242,
    release: vi.fn(),
  })),
  connectFabricDaemon: vi.fn(async () => ({
    bootstrapMcpSeat: vi.fn(async () => {
      if (daemon.result === undefined) throw new Error("fake bootstrap result is missing");
      return daemon.result;
    }),
    close: vi.fn(async () => undefined),
  })),
}));

import { bootstrapMcpSeat } from "../../src/cli/mcp-bootstrap.ts";
import { projectKey } from "../../src/cli/seat-store.ts";
import { runWorkspaceTrust } from "../../src/cli/workspace-trust.ts";

const roots: string[] = [];
const GENERATION = "a".repeat(64);

afterEach(async () => {
  daemon.result = undefined;
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

async function fixture(): Promise<{ projectRoot: string; paths: FabricPaths }> {
  const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-bootstrap-receipt-unit-")));
  roots.push(root);
  const projectRoot = join(root, "project");
  const stateDirectory = join(root, "state");
  await Promise.all([
    mkdir(projectRoot),
    mkdir(stateDirectory, { mode: 0o700 }),
  ]);
  const paths: FabricPaths = {
    stateDirectory,
    runtimeDirectory: join(root, "runtime"),
    databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
    socketPath: join(root, "runtime", "fabric-v1.sock"),
  };
  await runWorkspaceTrust(["trust", projectRoot], paths);
  daemon.result = {
    projectId: "project-one",
    canonicalRoot: projectRoot,
    bootstrapRunDirectory: ".agent-run/bootstrap-one",
    expectedPreviousGeneration: null,
    generation: GENERATION,
    projectSessionId: "session-one",
    sessionRevision: 1,
    sessionGeneration: 1,
    runId: "run-one",
    runRevision: 1,
    chairAgentId: "codex-agent",
    chairGeneration: 1,
    chairLeaseId: "chair:run-one:1",
    expiresAt: "2099-01-01T00:00:00.000Z",
    credentials: [{
      seat: "codex",
      agentId: "codex-agent",
      expectedPrincipalGeneration: 1,
      capability: `afc_${"c".repeat(43)}`,
      authorityId: "authority-one",
    }],
  };
  return { projectRoot, paths };
}

async function bootstrap(value: Awaited<ReturnType<typeof fixture>>) {
  return await bootstrapMcpSeat({
    environment: { AGENT_FABRIC_SEAT: "codex" },
    cwd: value.projectRoot,
    paths: value.paths,
    smokeDeadlineMs: 1,
  });
}

describe("legacy bootstrap provenance lifecycle action", () => {
  it("emits only for first recording while both legacy replays remain custody-unmutated", async () => {
    const value = await fixture();
    const first = await bootstrap(value);
    const seatRoot = join(value.paths.stateDirectory, "seats", projectKey(value.projectRoot));
    const metadataPath = join(seatRoot, "generations", first.generation, "codex.json");
    const legacy = JSON.parse(await readFile(metadataPath, "utf8")) as Record<string, unknown>;
    delete legacy.originKind;
    await writeFile(metadataPath, `${JSON.stringify(legacy, null, 2)}\n`, { mode: 0o600 });

    const recorded = await bootstrap(value);
    expect(recorded.receipt.mutated).toBe(false);
    expect(recorded.receipt.actions.every(({ mutated }) => !mutated)).toBe(true);
    expect(recorded.receipt.actions.find(({ action }) => action === "legacy-bootstrap-provenance")).toEqual({
      action: "legacy-bootstrap-provenance",
      outcome: "recorded",
      mutated: false,
      generation: GENERATION,
    });

    const alreadyRecorded = await bootstrap(value);
    expect(alreadyRecorded.receipt.mutated).toBe(false);
    expect(alreadyRecorded.receipt.actions.every(({ mutated }) => !mutated)).toBe(true);
    expect(alreadyRecorded.receipt.actions.some(
      ({ action }) => action === "legacy-bootstrap-provenance",
    )).toBe(false);
  });
});
