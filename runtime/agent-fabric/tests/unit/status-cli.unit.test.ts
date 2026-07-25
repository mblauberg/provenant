import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { afterEach, describe, expect, it } from "vitest";
import { parse, stringify } from "yaml";

import { fabricDoctor as realFabricDoctor, fabricStatus } from "../../src/cli/status.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import { probeProviderInterface as realProbeProviderInterface } from "../../src/adapters/provider-interface.ts";
import { FLOCK_ELECTION_LOCK_PORT } from "../../src/daemon/bootstrap-election.ts";
import { FabricError } from "../../src/errors.ts";
import { openFabric, startFabricDaemon } from "../../src/index.ts";
import { createPortableActivatedPrimaryFixture } from "../support/primary-adapter-testkit.ts";

const cleanup: string[] = [];
afterEach(async () => Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true }))));

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

type DoctorDependencies = NonNullable<Parameters<typeof realFabricDoctor>[2]>;

async function fabricDoctor(
  arguments_: string[],
  value: FabricPaths,
  dependencies: Partial<DoctorDependencies> = {},
): ReturnType<typeof realFabricDoctor> {
  return realFabricDoctor(arguments_, value, {
    verifyProvider: async ({ adapterId, executable }) => ({
      identity: {
        adapterId,
        executable,
        canonicalPath: executable,
        regularFile: true,
        ownerUid: process.getuid?.() ?? 0,
        mode: 0o755,
        sha256: "a".repeat(64),
        assurance: "full-vendor-identity",
        signing: [],
      },
      interface: { adapterId, conformant: true, probe: "fixture", version: "fixture" },
    }),
    verifyProviderIdentity: async ({ adapterId, executable }) => ({
      adapterId,
      canonicalPath: executable,
      regularFile: true,
      ownerUid: process.getuid?.() ?? 0,
      mode: 0o755,
      sha256: createHash("sha256").update(await readFile(executable)).digest("hex"),
      assurance: "full-vendor-identity",
      signing: [],
    }),
    probeProviderInterface: async ({ adapterId }) => ({
      adapterId,
      conformant: true,
      probe: "fixture",
      version: "different-from-installed-package-version",
    }),
    now: () => Date.parse("2026-07-25T00:00:00Z"),
    ...dependencies,
  });
}

async function paths(): Promise<FabricPaths> {
  const root = await mkdtemp(join(tmpdir(), "fabric-status-"));
  cleanup.push(root);
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(stateDirectory, "runtime");
  const databasePath = join(stateDirectory, "fabric-v1.sqlite3");
  await mkdir(runtimeDirectory, { recursive: true, mode: 0o700 });
  const fabric = await openFabric({ databasePath, workspaceRoots: [root] });
  await fabric.close();
  return { stateDirectory, runtimeDirectory, databasePath, socketPath: join(runtimeDirectory, "fabric-v1.sock") };
}

async function writeStoppedGeneration(
  value: FabricPaths,
  outcome: { exitCode: number | null; signal: NodeJS.Signals | null },
): Promise<void> {
  await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery-owner.json"), `${JSON.stringify({
    schemaVersion: 1,
    state: "stopped",
    actionId: "stopped-action",
    electionGeneration: 1,
    daemonInstanceGeneration: 1,
    socketPath: value.socketPath,
    pid: process.pid,
    bootstrapCapabilityHash: "a".repeat(64),
    updatedAt: 2,
    ...outcome,
  })}\n`, { mode: 0o600 });
  await writeFile(join(value.runtimeDirectory, "daemon-election.lease.json"), `${JSON.stringify({
    schemaVersion: 1,
    actionId: "stopped-action",
    electionGeneration: 1,
    status: "succeeded",
    acquiredAt: 1,
    terminalAt: 2,
    code: "BOOTSTRAP_READY",
    message: "generation reached ready",
  })}\n`, { mode: 0o600 });
  await writeFile(join(value.runtimeDirectory, "daemon-election.ready.json"), `${JSON.stringify({
    schemaVersion: 1,
    actionId: "stopped-action",
    electionGeneration: 1,
    daemonInstanceGeneration: 1,
    socketPath: value.socketPath,
    protocolVersion: 1,
    features: ["rpc"],
    readyAt: 2,
    evidence: { databaseOwned: true, migrationsComplete: true, recoveryComplete: true, socketBound: true },
  })}\n`, { mode: 0o600 });
}

describe("machine status and doctor", () => {
  it("reports configured adapters, exact roots and secret-free seat metadata", async () => {
    const value = await paths();
    const agentsHome = resolve(import.meta.dirname, "../../../..");
    const status = await fabricStatus(["--agents-home", agentsHome, "--project", agentsHome], value);
    expect(status).toMatchObject({
      schemaVersion: 1,
      daemon: { reachable: false, protocolVersion: 1 },
      configuredAdapters: ["claude-agent-sdk", "codex-app-server", "agy", "cursor-agent", "opencode-acp", "kiro-acp"],
      activeAdapters: [],
      project: { path: agentsHome },
    });
    expect(JSON.stringify(status)).not.toMatch(/capability|credentialPath|afb_|afc_/u);
  });

  it("reports a healthy typed on-demand idle state when every preflight passes", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);
    expect(result).toMatchObject({
      schemaVersion: 1,
      healthy: true,
      state: "idle",
      code: "DAEMON_ON_DEMAND_IDLE",
      daemon: { status: "idle", pid: null, socketPath: null },
      providerIdentity: {
        adapters: [
          { adapterId: "claude-agent-sdk", state: "clean" },
          { adapterId: "codex-app-server", state: "clean" },
        ],
      },
    });
    const checks = result.checks as Array<{ id: string; status: string }>;
    expect(checks.find((item) => item.id === "configuration")?.status).toBe("pass");
    expect(checks.find((item) => item.id === "adapter-compatibility")?.status).toBe("pass");
    expect(checks.find((item) => item.id === "database-integrity")?.status).toBe("pass");
    expect(checks.find((item) => item.id === "daemon-socket")?.status).toBe("idle");
  });

  it("treats an absent executable pin as clean and never compares the package version", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const compatibility: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
    if (!isRecord(compatibility) || !isRecord(compatibility.adapters)) throw new TypeError("fixture YAML is invalid");
    for (const adapterId of ["claude-agent-sdk", "codex-app-server"]) {
      const adapter = compatibility.adapters[adapterId];
      if (!isRecord(adapter) || !isRecord(adapter.implementation)) throw new TypeError("fixture adapter is invalid");
      delete adapter.implementation.executable_sha256;
      adapter.implementation.installed_version = "never-compare-this-package-version";
    }
    await writeFile(fixture.compatibilityPath, stringify(compatibility));

    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);
    expect(result).toMatchObject({
      healthy: true,
      providerIdentity: {
        adapters: [
          { adapterId: "claude-agent-sdk", state: "clean" },
          { adapterId: "codex-app-server", state: "clean" },
        ],
      },
    });
    expect(JSON.stringify(result.providerIdentity)).not.toMatch(/absent|installed_version|never-compare/u);
  });

  it("keeps a timed-out provider unknown and healthy while reporting the other adapter", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      providerProbeTimeoutMs: 250,
      probeProviderInterface: async ({ adapterId }) => adapterId === "codex-app-server"
        ? await new Promise(() => {})
        : { adapterId, conformant: true, probe: "fixture", version: "fixture" },
    });

    expect(result).toMatchObject({
      healthy: true,
      state: "idle",
      providerIdentity: {
        adapters: [
          { adapterId: "claude-agent-sdk", state: "clean" },
          { adapterId: "codex-app-server", state: "unknown", detail: expect.stringContaining("timed out after 250ms") },
        ],
      },
      checks: expect.arrayContaining([
        expect.objectContaining({ id: "provider-identity", status: "idle", code: "PROVIDER_IDENTITY_UNKNOWN" }),
      ]),
    });
  });

  it.each([
    Object.assign(new Error("provider binary missing"), { code: "ENOENT" }),
    Object.assign(new Error("permission denied"), { code: "EACCES" }),
  ])("maps an incomplete identity probe to unknown without failing health", async (probeError) => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      verifyProviderIdentity: async (input) => {
        if (input.adapterId === "codex-app-server") throw probeError;
        return {
          adapterId: input.adapterId,
          canonicalPath: input.executable,
          regularFile: true,
          ownerUid: process.getuid?.() ?? 0,
          mode: 0o755,
          sha256: createHash("sha256").update(await readFile(input.executable)).digest("hex"),
          assurance: "full-vendor-identity",
          signing: [],
        };
      },
    });
    expect(result).toMatchObject({
      healthy: true,
      providerIdentity: {
        adapters: expect.arrayContaining([
          expect.objectContaining({ adapterId: "codex-app-server", state: "unknown" }),
        ]),
      },
    });
  });

  it("keeps an authentication response unknown after the real interface probe normalises it", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      probeProviderInterface: async (input) => input.adapterId === "codex-app-server"
        ? await realProbeProviderInterface(input, async () => ({
            stdout: `${JSON.stringify({ id: 1, error: { message: "authentication required" } })}\n`,
            stderr: "",
            exitCode: 0,
          }))
        : { adapterId: input.adapterId, conformant: true, probe: "fixture", version: "fixture" },
    });
    expect(result).toMatchObject({
      healthy: true,
      providerIdentity: {
        adapters: expect.arrayContaining([
          expect.objectContaining({ adapterId: "codex-app-server", state: "unknown" }),
        ]),
      },
      checks: expect.arrayContaining([
        expect.objectContaining({ id: "provider-identity", status: "idle", code: "PROVIDER_IDENTITY_UNKNOWN" }),
      ]),
    });
  });

  it("keeps a malformed Codex initialize response drifted through the real interface probe", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      probeProviderInterface: async (input) => input.adapterId === "codex-app-server"
        ? await realProbeProviderInterface(input, async () => ({
            stdout: `${JSON.stringify({ id: 1, result: null })}\n`,
            stderr: "",
            exitCode: 0,
          }))
        : { adapterId: input.adapterId, conformant: true, probe: "fixture", version: "fixture" },
    });
    expect(result).toMatchObject({
      healthy: false,
      code: "PROVIDER_IDENTITY_DRIFT",
      providerIdentity: {
        adapters: expect.arrayContaining([
          expect.objectContaining({ adapterId: "codex-app-server", state: "drifted" }),
        ]),
      },
    });
  });

  it("reports digest and assurance drift with the repair command only on drifted adapters", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      verifyProviderIdentity: async ({ adapterId, executable }) => ({
        adapterId,
        canonicalPath: executable,
        regularFile: true,
        ownerUid: process.getuid?.() ?? 0,
        mode: 0o755,
        sha256: adapterId === "claude-agent-sdk"
          ? "0".repeat(64)
          : createHash("sha256").update(await readFile(executable)).digest("hex"),
        assurance: adapterId === "codex-app-server" ? "partial-signed-helpers" : "full-vendor-identity",
        signing: [],
      }),
    });
    expect(result).toMatchObject({
      healthy: false,
      state: "failed",
      code: "PROVIDER_IDENTITY_DRIFT",
      providerIdentity: {
        adapters: [
          { adapterId: "claude-agent-sdk", state: "drifted", detail: expect.stringContaining("npm run compatibility:pin") },
          { adapterId: "codex-app-server", state: "drifted", detail: expect.stringContaining("npm run compatibility:pin") },
        ],
      },
      checks: expect.arrayContaining([
        expect.objectContaining({ id: "provider-identity", status: "fail", code: "PROVIDER_IDENTITY_DRIFT" }),
      ]),
    });
  });

  it("reports a contract mismatch as drifted even when the other adapter is unknown", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      providerProbeTimeoutMs: 250,
      probeProviderInterface: async ({ adapterId }) => {
        if (adapterId === "claude-agent-sdk") {
          throw new FabricError("ADAPTER_INTERFACE_MISMATCH", "malformed contract response");
        }
        return await new Promise(() => {});
      },
    });
    expect(result).toMatchObject({
      healthy: false,
      code: "PROVIDER_IDENTITY_DRIFT",
      providerIdentity: {
        adapters: [
          { adapterId: "claude-agent-sdk", state: "drifted", detail: expect.stringContaining("npm run compatibility:pin") },
          { adapterId: "codex-app-server", state: "unknown" },
        ],
      },
    });
    const adapters = (result.providerIdentity as { adapters: Array<{ state: string; detail: string }> }).adapters;
    expect(adapters.find((item) => item.state === "unknown")?.detail).not.toContain("npm run compatibility:pin");
  });

  it("reports both dated sources against one advisory threshold without failing doctor", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const compatibility: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
    if (!isRecord(compatibility)) throw new TypeError("fixture YAML is invalid");
    compatibility.verification_date = "2026-06-01";
    await writeFile(fixture.compatibilityPath, stringify(compatibility));
    await mkdir(join(fixture.directory, "config"), { recursive: true });
    await writeFile(
      join(fixture.directory, "config", "model-routing.json"),
      `${JSON.stringify({ schema_version: 1, catalog_date: "2026-06-24" })}\n`,
    );

    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);
    expect(result).toMatchObject({
      healthy: true,
      staleness: {
        advisory: true,
        thresholdDays: 30,
        compatibility: { field: "verification_date", date: "2026-06-01", ageDays: 54, stale: true },
        modelRouting: { field: "catalog_date", date: "2026-06-24", ageDays: 31, stale: true },
      },
      checks: expect.arrayContaining([
        expect.objectContaining({ id: "pin-staleness", status: "pass", code: "PIN_STALENESS_ADVISORY" }),
      ]),
    });
  });

  it("does not call a live unrelated PID plus stale socket metadata reachable", async () => {
    const value = await paths();
    await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery.json"), `${JSON.stringify({
      schemaVersion: 1,
      socketPath: value.socketPath,
      pid: process.pid,
      bootstrapCapability: `afb_${"A".repeat(43)}`,
      lifecycleReceiptAuthorityId: null,
    })}\n`, { mode: 0o600 });
    const agentsHome = resolve(import.meta.dirname, "../../../..");
    await expect(fabricStatus(["--agents-home", agentsHome, "--project", agentsHome], value)).resolves.toMatchObject({
      daemon: { reachable: false }, activeAdapters: [],
    });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: false,
      state: "failed",
      code: "DAEMON_DISCOVERY_AMBIGUOUS",
      daemon: { status: "failed", pid: process.pid, socketPath: value.socketPath },
    });
  });

  it("keeps a recorded bootstrap failure unhealthy instead of calling it idle", async () => {
    const value = await paths();
    await writeFile(join(value.runtimeDirectory, "daemon-election.lease.json"), `${JSON.stringify({
      schemaVersion: 1,
      actionId: "doctor-bootstrap-failure",
      electionGeneration: 1,
      status: "failed",
      acquiredAt: 1,
      terminalAt: 2,
      code: "BOOTSTRAP_TEST_FAILURE",
      message: "bootstrap failed before daemon discovery was published",
    })}\n`, { mode: 0o600 });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: false,
      state: "failed",
      code: "BOOTSTRAP_TEST_FAILURE",
      daemon: { status: "failed", pid: null, socketPath: null },
    });
  });

  it("does not report idle while the kernel election lock is held before artifacts exist", async () => {
    const value = await paths();
    const lock = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(join(value.runtimeDirectory, "daemon-election.lock"));
    expect(lock).toBeDefined();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    try {
      await expect(fabricDoctor([
        "--agents-home", fixture.directory,
        "--trusted-config", fixture.configPath,
        "--compatibility", fixture.compatibilityPath,
        "--compatibility-schema", fixture.schemaPath,
      ], value)).resolves.toMatchObject({
        healthy: false,
        state: "failed",
        code: "BOOTSTRAP_IN_PROGRESS",
        daemon: { status: "failed", pid: null, socketPath: null },
      });
    } finally {
      await lock?.release();
    }
  });

  it("allows concurrent doctors to report the same healthy idle snapshot", async () => {
    const value = await paths();
    const inspection = await FLOCK_ELECTION_LOCK_PORT.probe(join(value.runtimeDirectory, "daemon-election.lock"));
    expect(inspection.status).toBe("acquired");
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    try {
      const arguments_ = [
        "--agents-home", fixture.directory,
        "--trusted-config", fixture.configPath,
        "--compatibility", fixture.compatibilityPath,
        "--compatibility-schema", fixture.schemaPath,
      ];
      const results = await Promise.all([fabricDoctor(arguments_, value), fabricDoctor(arguments_, value)]);
      expect(results).toEqual([
        expect.objectContaining({ healthy: true, state: "idle", code: "DAEMON_ON_DEMAND_IDLE" }),
        expect.objectContaining({ healthy: true, state: "idle", code: "DAEMON_ON_DEMAND_IDLE" }),
      ]);
    } finally {
      if (inspection.status === "acquired") await inspection.handle.release();
    }
  });

  it("reports bootstrap in progress before classifying a stale socket", async () => {
    const value = await paths();
    await writeFile(value.socketPath, "stale\n", { mode: 0o600 });
    const lock = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(join(value.runtimeDirectory, "daemon-election.lock"));
    expect(lock).toBeDefined();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    try {
      await expect(fabricDoctor([
        "--agents-home", fixture.directory,
        "--trusted-config", fixture.configPath,
        "--compatibility", fixture.compatibilityPath,
        "--compatibility-schema", fixture.schemaPath,
      ], value)).resolves.toMatchObject({
        healthy: false,
        state: "failed",
        code: "BOOTSTRAP_IN_PROGRESS",
      });
    } finally {
      await lock?.release();
    }
  });

  it("reports shutdown in progress while the terminal publication fence is held", async () => {
    const value = await paths();
    const daemon = await startFabricDaemon({
      ...value,
      workspaceRoots: [value.stateDirectory],
      adapters: {},
    });
    const shutdown = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(join(value.runtimeDirectory, "daemon-shutdown.lock"));
    expect(shutdown).toBeDefined();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    try {
      await expect(fabricDoctor([
        "--agents-home", fixture.directory,
        "--trusted-config", fixture.configPath,
        "--compatibility", fixture.compatibilityPath,
        "--compatibility-schema", fixture.schemaPath,
      ], value)).resolves.toMatchObject({
        healthy: false,
        state: "failed",
        code: "DAEMON_SHUTDOWN_IN_PROGRESS",
      });
    } finally {
      await shutdown?.release();
      await daemon.stop();
    }
  });

  it("rejects terminal discovery from an older generation than the current ready receipt", async () => {
    const value = await paths();
    await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery-owner.json"), `${JSON.stringify({
      schemaVersion: 1,
      state: "stopped",
      actionId: "old-action",
      electionGeneration: 1,
      daemonInstanceGeneration: 1,
      socketPath: value.socketPath,
      pid: process.pid,
      bootstrapCapabilityHash: "a".repeat(64),
      updatedAt: 1,
      exitCode: 0,
      signal: null,
    })}\n`, { mode: 0o600 });
    await writeFile(join(value.runtimeDirectory, "daemon-election.lease.json"), `${JSON.stringify({
      schemaVersion: 1,
      actionId: "new-action",
      electionGeneration: 2,
      status: "succeeded",
      acquiredAt: 2,
      terminalAt: 3,
      code: "BOOTSTRAP_READY",
      message: "new generation is ready",
    })}\n`, { mode: 0o600 });
    await writeFile(join(value.runtimeDirectory, "daemon-election.ready.json"), `${JSON.stringify({
      schemaVersion: 1,
      actionId: "new-action",
      electionGeneration: 2,
      daemonInstanceGeneration: 2,
      socketPath: value.socketPath,
      protocolVersion: 1,
      features: ["rpc"],
      readyAt: 3,
      evidence: { databaseOwned: true, migrationsComplete: true, recoveryComplete: true, socketBound: true },
    })}\n`, { mode: 0o600 });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: false,
      state: "failed",
      code: "DAEMON_ELECTION_INCONSISTENT",
      daemon: { status: "failed", pid: process.pid, socketPath: null },
    });
  });

  it.each([
    ["crashed", "DAEMON_PROCESS_CRASHED"],
    ["unknown", "DAEMON_DISCOVERY_INVALID"],
  ] as const)("never reports %s terminal discovery as idle", async (state, code) => {
    const value = await paths();
    await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery-owner.json"), `${JSON.stringify({
      schemaVersion: 1,
      state,
      actionId: "terminal-action",
      electionGeneration: 1,
      daemonInstanceGeneration: 1,
      socketPath: value.socketPath,
      pid: process.pid,
      bootstrapCapabilityHash: "a".repeat(64),
      updatedAt: 1,
      exitCode: 1,
      signal: null,
    })}\n`, { mode: 0o600 });
    await writeFile(value.socketPath, "stale\n", { mode: 0o600 });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: false,
      state: "failed",
      code,
      daemon: { status: "failed" },
    });
  });

  it.each([
    ["nonzero", { exitCode: 1, signal: null }],
    ["forced", { exitCode: null, signal: "SIGKILL" as const }],
  ] as const)("rejects a %s stopped outcome as unhealthy", async (_label, outcome) => {
    const value = await paths();
    await writeStoppedGeneration(value, outcome);
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: false,
      state: "failed",
      code: "DAEMON_PROCESS_UNCLEAN_STOP",
      daemon: { status: "failed", pid: process.pid, socketPath: null },
    });
  });

  it.each(["runtime", "state"] as const)("keeps a missing %s directory unhealthy", async (directory) => {
    const value = await paths();
    await rm(directory === "runtime" ? value.runtimeDirectory : value.stateDirectory, { recursive: true });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);
    expect(result).toMatchObject({ healthy: false, state: "failed", daemon: { status: "failed" } });
    expect(result.checks).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: `${directory}-directory`, status: "fail" }),
    ]));
  });

  it("reports idle after an on-demand daemon stops cleanly without retaining its PID or socket", async () => {
    const value = await paths();
    const daemon = await startFabricDaemon({
      ...value,
      workspaceRoots: [value.stateDirectory],
      adapters: {},
    });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: true,
      state: "live",
      code: "DAEMON_LIVE",
      daemon: { status: "live", pid: daemon.pid, socketPath: value.socketPath },
    });
    await daemon.stop();
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      healthy: true,
      state: "idle",
      code: "DAEMON_ON_DEMAND_IDLE",
      daemon: { status: "idle", pid: null, socketPath: null },
      checks: expect.arrayContaining([
        expect.objectContaining({ id: "daemon-socket", status: "idle", code: "DAEMON_ON_DEMAND_IDLE" }),
      ]),
    });
  });

  it("keeps database preflight failure unhealthy while the daemon is idle", async () => {
    const value = await paths();
    await rm(value.databasePath);
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);
    expect(result).toMatchObject({
      healthy: false,
      state: "failed",
      daemon: { status: "idle", pid: null, socketPath: null },
    });
    expect(result.checks).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "database-integrity", status: "fail" }),
      expect.objectContaining({ id: "daemon-socket", status: "idle", code: "DAEMON_ON_DEMAND_IDLE" }),
    ]));
  });

  it("reports adapters loaded by the live daemon rather than a changed config file", async () => {
    const value = await paths();
    const daemon = await startFabricDaemon({
      ...value,
      workspaceRoots: [value.stateDirectory],
      adapters: { "live-only": { command: [process.execPath, "-e", "process.exit(0)"], environment: {} } },
    });
    try {
      await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery.json"), `${JSON.stringify({
        schemaVersion: 1, socketPath: value.socketPath, pid: daemon.pid,
        bootstrapCapability: daemon.bootstrapCapability, lifecycleReceiptAuthorityId: null,
      })}\n`, { mode: 0o600 });
      const agentsHome = resolve(import.meta.dirname, "../../../..");
      await expect(fabricStatus(["--agents-home", agentsHome, "--project", agentsHome], value)).resolves.toMatchObject({
        daemon: { reachable: true, activeAdapters: ["live-only"] },
        configuredAdapters: ["claude-agent-sdk", "codex-app-server", "agy", "cursor-agent", "opencode-acp", "kiro-acp"],
        activeAdapters: ["live-only"],
      });
    } finally { await daemon.stop(); }
  });
});
