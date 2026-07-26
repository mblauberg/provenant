import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import type { Socket } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Duplex } from "node:stream";

import { afterEach, describe, expect, it } from "vitest";
import { parse, stringify } from "yaml";
import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

import { fabricDoctor as realFabricDoctor, fabricStatus } from "../../src/cli/status.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import { probeProviderInterface as realProbeProviderInterface } from "../../src/adapters/provider-interface.ts";
import { FLOCK_ELECTION_LOCK_PORT } from "../../src/daemon/bootstrap-election.ts";
import { FabricDaemonClient } from "../../src/daemon/rpc-client.ts";
import { FabricError } from "../../src/errors.ts";
import { openFabric, startFabricDaemon } from "../../src/index.ts";
import { FABRIC_PROTOCOL_LIMITS } from "../../src/transport/bounded-ndjson.ts";
import { createPortableActivatedPrimaryFixture } from "../support/primary-adapter-testkit.ts";

const cleanup: string[] = [];
afterEach(async () => Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true }))));

class DoctorFixtureDaemonSocket extends Duplex {
  readonly methods: string[] = [];

  constructor(
    private readonly dropProbe = false,
    private readonly capabilities: readonly string[] = ["rpc"],
  ) {
    super();
    queueMicrotask(() => this.emit("connect"));
  }

  override _read(): void {}

  override _write(
    chunk: Buffer,
    _encoding: BufferEncoding,
    callback: (error?: Error | null) => void,
  ): void {
    const request = JSON.parse(chunk.toString("utf8")) as { id: string; method: string };
    this.methods.push(request.method);
    if (request.method === "initialize") {
      this.push(`${JSON.stringify({
        id: request.id,
        result: {
          protocolVersion: 1,
          daemonVersion: "pre-0636854",
          capabilities: this.capabilities,
          limits: FABRIC_PROTOCOL_LIMITS,
          activeAdapters: [],
        },
      })}\n`);
      callback();
      return;
    }
    if (this.dropProbe) {
      this.destroy();
      callback();
      return;
    }
    this.push(`${JSON.stringify({
      id: request.id,
      error: {
        name: "DaemonProtocolError",
        code: "BOOTSTRAP_SCOPE_VIOLATION",
        message: "bootstrap capability is limited to private local bootstrap control methods",
      },
    })}\n`);
    callback();
  }

  override _final(callback: (error?: Error | null) => void): void {
    this.push(null);
    this.destroy();
    callback();
  }
}

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

/** Copy the repository's certifying profile and routing catalogue into a fixture home. */
async function writeReviewProfileFixture(directory: string): Promise<void> {
  const root = resolve(import.meta.dirname, "../../../..");
  await mkdir(join(directory, "config", "review-profiles"), { recursive: true });
  await writeFile(
    join(directory, "config", "review-profiles", "certifying-review-four-slot-v1.json"),
    await readFile(join(root, "config", "review-profiles", "certifying-review-four-slot-v1.json"), "utf8"),
  );
  await writeFile(
    join(directory, "config", "model-routing.json"),
    await readFile(join(root, "config", "model-routing.json"), "utf8"),
  );
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

async function writeActiveGeneration(value: FabricPaths): Promise<void> {
  const actionId = "active-doctor-action";
  const bootstrapCapability = `afb_${"A".repeat(43)}`;
  const bootstrapCapabilityHash = createHash("sha256").update(bootstrapCapability).digest("hex");
  await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery.json"), `${JSON.stringify({
    schemaVersion: 1,
    socketPath: value.socketPath,
    pid: process.pid,
    bootstrapCapability,
    lifecycleReceiptAuthorityId: null,
  })}\n`, { mode: 0o600 });
  await writeFile(join(value.runtimeDirectory, "fabric-v1.discovery-owner.json"), `${JSON.stringify({
    schemaVersion: 1,
    state: "active",
    actionId,
    electionGeneration: 1,
    daemonInstanceGeneration: 1,
    socketPath: value.socketPath,
    pid: process.pid,
    bootstrapCapabilityHash,
    updatedAt: 1,
    exitCode: null,
    signal: null,
  })}\n`, { mode: 0o600 });
  await writeFile(join(value.runtimeDirectory, "daemon-election.lease.json"), `${JSON.stringify({
    schemaVersion: 1,
    actionId,
    electionGeneration: 1,
    status: "succeeded",
    acquiredAt: 1,
    terminalAt: 2,
    code: "BOOTSTRAP_READY",
    message: "generation reached ready",
  })}\n`, { mode: 0o600 });
  await writeFile(join(value.runtimeDirectory, "daemon-election.ready.json"), `${JSON.stringify({
    schemaVersion: 1,
    actionId,
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
    const checks = result.checks as Array<{ id: string; status: string; detail: string }>;
    expect(checks.find((item) => item.id === "configuration")?.status).toBe("pass");
    expect(checks.find((item) => item.id === "adapter-compatibility")?.status).toBe("pass");
    expect(checks.find((item) => item.id === "database-integrity")?.status).toBe("pass");
    expect(checks.find((item) => item.id === "daemon-socket")?.status).toBe("idle");
    // No certifying profile declares a pin here, so nothing is compared and
    // nothing is unknown: an absent expectation is not evidence.
    expect(checks.find((item) => item.id === "review-profile-pins")).toMatchObject({
      status: "pass",
      detail: "no certifying profile pins in the comparison set",
    });
  });

  it.each([
    {
      label: "clean",
      observed: "claude-opus-5",
      status: "pass",
      code: "REVIEW_PROFILE_PIN_OK",
      healthy: true,
    },
    {
      label: "drifted",
      observed: "claude-opus-6",
      status: "fail",
      code: "REVIEW_PROFILE_PIN_DRIFT",
      healthy: false,
    },
  ])("reports the certifying profile pin comparison as $label", async ({ observed, status, code, healthy }) => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      observeReviewProfilePin: async ({ providerFamily }) => ({
        status: "observed",
        model: providerFamily === "anthropic" ? observed : "gpt-5.6-sol",
        detail: "fixture",
      }),
    });
    const checks = result.checks as Array<{ id: string; status: string; code: string; detail: string }>;
    const check = checks.find((item) => item.id === "review-profile-pins")!;
    expect(result.healthy).toBe(healthy);
    expect(check).toMatchObject({ status, code });
    expect(check.detail.includes("npm run profile:pin")).toBe(status === "fail");
    expect(result.reviewProfilePins).toMatchObject({
      repairCommand: "npm run profile:pin",
      attested: [
        expect.objectContaining({ providerFamily: "xai", observedOn: "2026-07-16" }),
        expect.objectContaining({ providerFamily: "google", observedOn: "2026-07-16" }),
      ],
    });
  });

  it("never writes the certifying profile it reports on, even when it is drifted", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const profilePath = join(fixture.directory, "config", "review-profiles", "certifying-review-four-slot-v1.json");
    const before = await readFile(profilePath, "utf8");
    // A refresh moves the digest-bound catalogue, so it must never be a side
    // effect of a report: only an explicit repair may write.
    await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      observeReviewProfilePin: async () => ({ status: "observed", model: "some-newer-model", detail: "fixture" }),
    });
    expect(await readFile(profilePath, "utf8")).toBe(before);
  });

  it("reports an unobservable provider as unknown without failing doctor or naming a repair", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      observeReviewProfilePin: async () => ({
        status: "unobservable",
        detail: "Claude subscription authentication is unavailable",
      }),
    });
    const checks = result.checks as Array<{ id: string; status: string; code: string; detail: string }>;
    const check = checks.find((item) => item.id === "review-profile-pins")!;
    expect(result.healthy).toBe(true);
    expect(check).toMatchObject({ status: "idle", code: "REVIEW_PROFILE_PIN_UNKNOWN" });
    expect(check.detail).not.toContain("npm run profile:pin");
    const compared = (result.reviewProfilePins as { compared: Array<{ state: string }> }).compared;
    expect(compared.map((pin) => pin.state)).toStrictEqual(["unknown", "unknown"]);
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
      state: "blocked",
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
      state: "blocked",
      code: "DAEMON_DISCOVERY_AMBIGUOUS",
      daemon: { status: "failed", pid: process.pid, socketPath: value.socketPath },
    });
  });

  it("reports a responsive legacy credential contract as typed protocol incompatibility", async () => {
    const value = await paths();
    await writeActiveGeneration(value);
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    let socket: DoctorFixtureDaemonSocket | undefined;
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      inspectDaemonSocket: async () => ({
        isSocket: () => true,
        uid: process.getuid?.() ?? 0,
      }),
      connectDaemon: async () => {
        socket = new DoctorFixtureDaemonSocket();
        return FabricDaemonClient.connect(
          value.socketPath,
          `afb_${"A".repeat(43)}`,
          [],
          { connect: () => socket as unknown as Socket },
        );
      },
    });

    expect(result).toMatchObject({
      healthy: false,
      state: "blocked",
      code: "PROTOCOL_INCOMPATIBLE",
      daemon: { status: "failed", pid: process.pid, socketPath: value.socketPath },
      checks: expect.arrayContaining([
        expect.objectContaining({
          id: "daemon-socket",
          status: "fail",
          code: "PROTOCOL_INCOMPATIBLE",
          detail: expect.stringContaining("mcp-bootstrap-credentials.v2"),
        }),
      ]),
    });
    expect((result.checks as Array<{ detail: string }>).find(({ detail }) =>
      detail.includes("mcp-bootstrap-credentials.v2"))?.detail).toContain("retry provenant doctor");
    expect(socket?.methods).toEqual(["initialize"]);
    expect(socket?.destroyed).toBe(true);
  });

  it("reports a dropped contract probe as a handshake failure, not protocol incompatibility", async () => {
    const value = await paths();
    await writeActiveGeneration(value);
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    let socket: DoctorFixtureDaemonSocket | undefined;
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      inspectDaemonSocket: async () => ({
        isSocket: () => true,
        uid: process.getuid?.() ?? 0,
      }),
      connectDaemon: async () => {
        socket = new DoctorFixtureDaemonSocket(
          true,
          ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
        );
        return FabricDaemonClient.connect(
          value.socketPath,
          `afb_${"A".repeat(43)}`,
          [],
          { connect: () => socket as unknown as Socket },
        );
      },
    });

    expect(result).toMatchObject({
      healthy: false,
      state: "blocked",
      code: "DAEMON_HANDSHAKE_FAILED",
      daemon: { status: "failed" },
    });
    expect(result.code).not.toBe("PROTOCOL_INCOMPATIBLE");
    expect(socket?.methods).toEqual(["initialize", "eventsAfter"]);
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
      state: "blocked",
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
        state: "recovering",
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
        state: "recovering",
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
        state: "recovering",
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
      state: "blocked",
      code: "DAEMON_ELECTION_INCONSISTENT",
      daemon: { status: "failed", pid: process.pid, socketPath: null },
    });
  });

  it.each([
    ["crashed", "DAEMON_PROCESS_CRASHED", "recovering"],
    ["unknown", "DAEMON_DISCOVERY_INVALID", "blocked"],
  ] as const)("never reports %s terminal discovery as idle", async (state, code, lifecycleState) => {
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
      state: lifecycleState,
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
      state: "recovering",
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
    expect(result).toMatchObject({ healthy: false, state: "blocked", daemon: { status: "failed" } });
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
      state: "current",
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

  it("names the satisfied precondition that makes an idle lifecycle idle", async () => {
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
      state: "idle",
      healthy: true,
      cause: {
        checkId: "daemon-socket",
        precondition: "daemon discovery, election, process, socket and bootstrap contract agree",
        // An idle lifecycle is healthy precisely because no daemon is expected;
        // the daemon precondition is genuinely not met, and saying so is the
        // causal report. `satisfied` tracks the check, never the health rollup.
        satisfied: false,
        code: "DAEMON_ON_DEMAND_IDLE",
        recoverable: false,
      },
    });
    for (const check of result.checks as Array<{ id: string; precondition: string }>) {
      expect(check.precondition.length).toBeGreaterThan(0);
      expect(check.precondition).not.toBe(check.id);
    }
  });

  it("never reports an unknown precondition as satisfied while staying healthy", async () => {
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
      probeProviderInterface: async () => await new Promise(() => undefined),
    });
    // The idle-on-unknown mapping is #406's design and is preserved.
    expect(result).toMatchObject({
      healthy: true,
      state: "idle",
      cause: {
        checkId: "provider-identity",
        precondition: "each primary provider matches its pinned executable identity",
        satisfied: false,
        code: "PROVIDER_IDENTITY_UNKNOWN",
        recoverable: false,
      },
    });
  });

  it("classifies a stale protocol build itself rather than declaring it unobservable", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => {
        throw Object.assign(new Error("local @local/agent-fabric-protocol dist is missing or older than its build inputs"), {
          code: "AGENT_FABRIC_PROTOCOL_BUILD_STALE",
        });
      },
    });
    // The package bin points straight at dist/cli/main.js, so doctor must also
    // reach and classify this state without a launcher-provided verdict.
    expect(result).toMatchObject({
      healthy: false,
      state: "blocked",
      code: "AGENT_FABRIC_PROTOCOL_BUILD_STALE",
      cause: {
        checkId: "protocol-build",
        precondition: "the local protocol dist is present and current for its build inputs",
        satisfied: false,
        recoverable: false,
      },
    });
  });

  it("reports the stale verdict and exact repair handed down by the launcher", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const previousVerdict = process.env.AGENT_FABRIC_PROTOCOL_BUILD_VERDICT;
    const previousRepair = process.env.AGENT_FABRIC_PROTOCOL_BUILD_REPAIR;
    process.env.AGENT_FABRIC_PROTOCOL_BUILD_VERDICT = "stale";
    process.env.AGENT_FABRIC_PROTOCOL_BUILD_REPAIR =
      'AGENTS_HOME="/fixture/agents" "/fixture/agents/scripts/agent-fabric-protocol-build"';
    try {
      const result = await fabricDoctor([
        "--agents-home", fixture.directory,
        "--trusted-config", fixture.configPath,
        "--compatibility", fixture.compatibilityPath,
        "--compatibility-schema", fixture.schemaPath,
      ], value, {
        preflightProtocolBuild: async () => {
          throw new Error("doctor must consume the launcher verdict without rerunning a blocking preflight");
        },
      });

      expect(result).toMatchObject({
        healthy: false,
        state: "blocked",
        code: "AGENT_FABRIC_PROTOCOL_BUILD_STALE",
        cause: {
          checkId: "protocol-build",
          precondition: "the local protocol dist is present and current for its build inputs",
          satisfied: false,
        },
      });
      expect(result.checks).toEqual(expect.arrayContaining([
        expect.objectContaining({
          id: "protocol-build",
          status: "fail",
          code: "AGENT_FABRIC_PROTOCOL_BUILD_STALE",
          detail: expect.stringContaining(
            'repair: AGENTS_HOME="/fixture/agents" "/fixture/agents/scripts/agent-fabric-protocol-build"',
          ),
        }),
      ]));
    } finally {
      if (previousVerdict === undefined) delete process.env.AGENT_FABRIC_PROTOCOL_BUILD_VERDICT;
      else process.env.AGENT_FABRIC_PROTOCOL_BUILD_VERDICT = previousVerdict;
      if (previousRepair === undefined) delete process.env.AGENT_FABRIC_PROTOCOL_BUILD_REPAIR;
      else process.env.AGENT_FABRIC_PROTOCOL_BUILD_REPAIR = previousRepair;
    }
  });

  it("names the unsatisfied precondition and refuses to call a blocked cause recoverable", async () => {
    const value = await paths();
    await rm(value.databasePath);
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value)).resolves.toMatchObject({
      state: "blocked",
      cause: {
        checkId: "database-integrity",
        precondition: "the Fabric database is current-schema and passes its invariants",
        satisfied: false,
        recoverable: false,
      },
    });
  });

  it("names a converging transition as the recoverable cause of a recovering lifecycle", async () => {
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
        state: "recovering",
        cause: {
          checkId: "daemon-socket",
          code: "BOOTSTRAP_IN_PROGRESS",
          satisfied: false,
          recoverable: true,
          detail: "bootstrap election is active",
        },
      });
    } finally {
      await lock?.release();
    }
  });

  it("does not declare the doctor wrapper as intercepting its protocol-build check", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);
    expect(result.wrapperIntercepted).toEqual([]);
    expect((result.checks as Array<{ id: string }>).map(({ id }) => id)).toContain("protocol-build");
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
      state: "blocked",
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
