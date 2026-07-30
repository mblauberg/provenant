import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, symlink, writeFile } from "node:fs/promises";
import type { Socket } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { Duplex } from "node:stream";

import { afterEach, describe, expect, it, vi } from "vitest";
import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

import {
  fabricDoctor as realFabricDoctor,
  fabricStatus,
  resolveStatusPaths,
} from "../../src/cli/status.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import { probeProviderInterface as realProbeProviderInterface } from "../../src/adapters/provider-interface.ts";
import { FLOCK_ELECTION_LOCK_PORT } from "../../src/daemon/bootstrap-election.ts";
import { FabricDaemonClient } from "../../src/daemon/rpc-client.ts";
import { FabricError } from "../../src/errors.ts";
import { openFabric, startFabricDaemon } from "../../src/index.ts";
import { PIN_OBSERVATION_CACHE_FILE } from "../../src/review/profile/pin-observer.ts";
import { digestCanonical } from "../../src/review/canonical/index.ts";
import { deployReviewProfileCatalogue } from "../../scripts/deploy-review-profile-catalogue.ts";
import { FABRIC_PROTOCOL_LIMITS } from "../../src/transport/bounded-ndjson.ts";
import { createPortableActivatedPrimaryFixture } from "../support/primary-adapter-testkit.ts";
import { runSourceCli } from "../support/cli-process.ts";
import { installSeatGeneration, projectKey } from "../../src/cli/seat-store.ts";
import { parseMcpPeerProvisionArguments } from "../../src/cli/mcp-peer-provision.ts";
import { shellCommandArguments } from "../support/shell-command-arguments.ts";

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

async function writeReviewProfileDeploymentRecord(directory: string): Promise<void> {
  const relativeProfile = "config/review-profiles/certifying-review-four-slot-v1.json";
  const profile: unknown = JSON.parse(await readFile(join(directory, relativeProfile), "utf8"));
  await writeFile(
    join(directory, "config", "review-profiles", "certifying-review-four-slot-v1.deployment-digest.json"),
    `${JSON.stringify({
      schemaVersion: 1,
      profile: relativeProfile,
      digest: digestCanonical(profile),
    }, null, 2)}\n`,
  );
}

async function writeCapabilityProducer(
  directory: string,
  name: "claude_capabilities.py" | "codex_capabilities.py",
  body: string,
): Promise<void> {
  const scripts = join(directory, "skills", "orchestrate", "scripts");
  await mkdir(scripts, { recursive: true });
  await writeFile(join(scripts, name), body, { mode: 0o700 });
}

async function writePoisonedCapabilityProducers(directory: string, callLog: string): Promise<void> {
  const body = `#!/bin/sh
printf '%s\\n' POISONED_CAPABILITY_PRODUCER_INVOKED >> ${JSON.stringify(callLog)}
printf '%s\\n' POISONED_CAPABILITY_PRODUCER_INVOKED >&2
exit 97
`;
  await Promise.all([
    writeCapabilityProducer(directory, "claude_capabilities.py", body),
    writeCapabilityProducer(directory, "codex_capabilities.py", body),
  ]);
}

async function directoryByteSnapshot(directory: string): Promise<Record<string, string>> {
  const snapshot: Record<string, string> = {};
  const visit = async (current: string, relative: string): Promise<void> => {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const entryRelative = relative.length === 0 ? entry.name : `${relative}/${entry.name}`;
      if (entry.isDirectory()) {
        snapshot[`${entryRelative}/`] = "directory";
        await visit(join(current, entry.name), entryRelative);
      } else {
        snapshot[entryRelative] = createHash("sha256")
          .update(await readFile(join(current, entry.name)))
          .digest("hex");
      }
    }
  };
  await visit(directory, "");
  return snapshot;
}

async function leaveHotRollbackJournal(databasePath: string): Promise<void> {
  const child = spawn(process.execPath, ["-e", `
const Database = require("better-sqlite3");
const database = new Database(process.argv[1]);
database.pragma("journal_mode = DELETE");
database.pragma("synchronous = FULL");
database.pragma("cache_size = 1");
database.exec("BEGIN IMMEDIATE");
database.prepare("UPDATE fabric_schema SET baseline_sha256 = ?").run("f".repeat(64));
database.exec("CREATE TABLE hot_journal_spill(value BLOB)");
const insert = database.prepare("INSERT INTO hot_journal_spill(value) VALUES (?)");
for (let index = 0; index < 256; index += 1) insert.run(Buffer.alloc(8192, index % 251));
process.stdout.write("ready\\n");
setInterval(() => {}, 1000);
`, databasePath], {
    cwd: resolve(import.meta.dirname, "../.."),
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (child.stdout === null || child.stderr === null) throw new Error("hot-journal fixture stdio unavailable");
  let stderr = "";
  child.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8"); });
  await new Promise<void>((resolvePromise, reject) => {
    const timeout = setTimeout(() => reject(new Error("hot-journal fixture did not become ready")), 5_000);
    child.stdout!.once("data", (chunk: Buffer) => {
      clearTimeout(timeout);
      chunk.toString("utf8").includes("ready") ? resolvePromise() : reject(new Error(`unexpected fixture output: ${chunk.toString("utf8")}`));
    });
    child.once("exit", (code) => {
      clearTimeout(timeout);
      reject(new Error(`hot-journal fixture exited early with ${String(code)}: ${stderr}`));
    });
  });
  child.kill("SIGKILL");
  await new Promise<void>((resolvePromise) => child.once("exit", () => resolvePromise()));
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
  it("derives instance configuration and product assets from separate roots", () => {
    expect(resolveStatusPaths([
      "--product-root", "/fixture/product",
      "--instance-root", "/fixture/instance",
    ])).toEqual({
      agentsHome: resolve("/fixture/product"),
      instanceRoot: resolve("/fixture/instance"),
      config: resolve("/fixture/instance/config/agent-fabric.yaml"),
      compatibility: resolve("/fixture/instance/config/adapter-compatibility.yaml"),
      compatibilitySchema: resolve(
        "/fixture/product/runtime/agent-fabric/schemas/adapter-compatibility.schema.json",
      ),
      modelRouting: resolve("/fixture/instance/config/model-routing.json"),
      reviewProfile: resolve(
        "/fixture/instance/config/review-profiles/certifying-review-four-slot-v1.json",
      ),
    });
  });

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

  it("reports an exact bootstrap remedy when the project has no chair seat", async () => {
    const value = await paths();
    const agentsHome = resolve(import.meta.dirname, "../../../..");
    const requestedProject = join(dirname(value.stateDirectory), "project root");
    await mkdir(requestedProject);
    const project = await realpath(requestedProject);

    const status = await fabricStatus(["--agents-home", agentsHome, "--project", project], value);

    expect(status.project).toEqual({
      path: project,
      seats: expect.arrayContaining([{
        seat: "agy",
        registered: false,
        active: false,
        reason: "PROJECT_NOT_BOOTSTRAPPED",
        remedy: `cd '${project}' && '${agentsHome}/scripts/agent-fabric' bootstrap --seat codex`,
      }]),
    });
  });

  it("reports registered provenance and an exact peer provisioning remedy without credentials", async () => {
    const value = await paths();
    const agentsHome = resolve(import.meta.dirname, "../../../..");
    const requestedProject = join(dirname(value.stateDirectory), "project root");
    await mkdir(requestedProject);
    const project = await realpath(requestedProject);
    const generation = "a".repeat(64);
    const expiresAt = "2099-01-01T00:00:00.000Z";
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
      expiresAt,
    };
    await installSeatGeneration({
      stateDirectory: value.stateDirectory,
      projectPath: project,
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

    const status = await fabricStatus(["--agents-home", agentsHome, "--project", project], value);
    const seats = (status.project as { seats: Array<Record<string, unknown>> }).seats;

    expect(seats).toContainEqual(expect.objectContaining({
      seat: "codex",
      registered: true,
      active: true,
      role: "chair",
      originKind: "bootstrap",
    }));
    expect(seats).toContainEqual({
      seat: "agy",
      registered: false,
      active: false,
      reason: "PEER_SEAT_NOT_PROVISIONED",
      remedy: `'${agentsHome}/scripts/agent-fabric' mcp peer-provision --project '${project}' --seat agy`,
    });
    expect(JSON.stringify(status)).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}|credentialPath/u);
  });

  it("reports a parser-valid renewal remedy for a provisioned roster nearing expiry", async () => {
    const value = await paths();
    const agentsHome = resolve(import.meta.dirname, "../../../..");
    const requestedProject = join(dirname(value.stateDirectory), "renewal project");
    await mkdir(requestedProject);
    const project = await realpath(requestedProject);
    const generation = "b".repeat(64);
    const expiresAt = new Date(Date.now() + 30 * 60 * 1_000).toISOString();
    const common = {
      schemaVersion: 1 as const,
      projectKey: projectKey(project),
      projectPath: project,
      generation,
      previousGeneration: null,
      originKind: "provisioned" as const,
      projectSessionId: "session-renewal",
      sessionRevision: 1,
      sessionGeneration: 1,
      runId: "run-renewal",
      runRevision: 1,
      chairAgentId: "codex-chair",
      chairGeneration: 1,
      chairLeaseId: "chair:run-renewal:1",
      principalGeneration: 1,
      expiresAt,
    };
    await installSeatGeneration({
      stateDirectory: value.stateDirectory,
      projectPath: project,
      generation,
      expectedPreviousGeneration: null,
      seats: [
        {
          credential: `afc_${"a".repeat(43)}`,
          metadata: { ...common, seat: "agy", agentId: "agy-peer", role: "peer" },
        },
        {
          credential: `afc_${"b".repeat(43)}`,
          metadata: { ...common, seat: "codex", agentId: "codex-chair", role: "chair" },
        },
      ],
    });

    const status = await fabricStatus(["--agents-home", agentsHome, "--project", project], value);
    const seats = (status.project as { seats: Array<Record<string, unknown>> }).seats;
    const chair = seats.find(({ seat }) => seat === "codex");
    expect(chair).toMatchObject({
      registered: true,
      active: true,
      originKind: "provisioned",
      remedy: expect.stringContaining("mcp peer-provision"),
    });
    const commandArguments = await shellCommandArguments(String(chair?.remedy), dirname(value.stateDirectory));
    expect(commandArguments.slice(0, 2)).toEqual(["mcp", "peer-provision"]);
    expect(parseMcpPeerProvisionArguments(commandArguments.slice(2))).toEqual({
      project,
      seats: ["agy"],
      expiresAt: new Date(Date.parse(expiresAt) + 23 * 24 * 60 * 60 * 1_000).toISOString(),
    });
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

  it("uses cache-only pin evidence by default without invoking a producer or changing any state byte", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const producerCallLog = join(fixture.directory, "producer-calls.log");
    await writePoisonedCapabilityProducers(fixture.directory, producerCallLog);
    const before = await directoryByteSnapshot(value.stateDirectory);

    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);

    await expect(readFile(producerCallLog, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    expect(await directoryByteSnapshot(value.stateDirectory)).toStrictEqual(before);
    const check = (result.checks as Array<{
      id: string;
      status: string;
      code: string;
      detail: string;
      precondition: string;
    }>).find((item) => item.id === "review-profile-pins")!;
    expect(check).toStrictEqual({
      id: "review-profile-pins",
      status: "idle",
      code: "REVIEW_PROFILE_PIN_UNKNOWN",
      precondition: "each certifying-profile model pin in the automated comparison set matches a provider capability result cached within the last six hours",
      detail: [
        "openai/gpt-5.6-sol=unknown: no provider capability result cached within the last six hours; live provider capability probe was not run",
        "anthropic/claude-opus-5=unknown: no provider capability result cached within the last six hours; live provider capability probe was not run",
        "xai/cursor-grok-4.5-high=attested observed_on=2026-07-16: xai has no capability observer; identity is attested, not compared",
        "google/Gemini 3.1 Pro (High)=attested observed_on=2026-07-16: google has no capability observer; identity is attested, not compared",
      ].join(" "),
    });
    expect(result).toMatchObject({
      healthy: true,
      cause: {
        checkId: "review-profile-pins",
        satisfied: false,
        code: "REVIEW_PROFILE_PIN_UNKNOWN",
      },
      reviewProfilePins: {
        compared: [
          { providerFamily: "openai", state: "unknown" },
          { providerFamily: "anthropic", state: "unknown" },
        ],
      },
    });
  });

  it("derives the pin report from a deployed profile matching its deployment-owned digest", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    await writeReviewProfileDeploymentRecord(fixture.directory);

    const result = await fabricDoctor([
      "--consume-provider-quota",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
      observeReviewProfilePin: async ({ providerFamily }) => ({
        status: "observed",
        model: providerFamily === "anthropic" ? "claude-opus-5" : "gpt-5.6-sol",
        detail: "fixture",
      }),
    });

    expect(result.healthy, JSON.stringify(result.checks, null, 2)).toBe(true);
    expect(result).toMatchObject({
      reviewProfilePins: {
        catalogueDeployment: {
          status: "verified",
          profile: "config/review-profiles/certifying-review-four-slot-v1.json",
        },
        compared: [
          { providerFamily: "openai", state: "clean" },
          { providerFamily: "anthropic", state: "clean" },
        ],
      },
    });
  });

  it("rejects a deployed profile edited after deployment with a typed repairable digest error", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    await writeReviewProfileDeploymentRecord(fixture.directory);
    const profilePath = join(
      fixture.directory,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    );
    const profile = JSON.parse(await readFile(profilePath, "utf8")) as { profileId: string };
    profile.profileId = "edited-after-deployment";
    await writeFile(profilePath, `${JSON.stringify(profile)}\n`);

    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
    })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_DIGEST_INVALID",
      field: "config/review-profiles/certifying-review-four-slot-v1.json",
      message: expect.stringContaining("npm run profile:catalogue:deploy -- --agents-home"),
    });
  });

  it("reports a pre-deployment install without a deployment record as unverified and keeps diagnosing", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);

    const result = await fabricDoctor([
      "--consume-provider-quota",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
      observeReviewProfilePin: async ({ providerFamily }) => ({
        status: "observed",
        model: providerFamily === "anthropic" ? "claude-opus-5" : "gpt-5.6-sol",
        detail: "fixture",
      }),
    });

    expect(result.healthy, JSON.stringify(result.checks, null, 2)).toBe(true);
    expect(result).toMatchObject({
      reviewProfilePins: {
        catalogueDeployment: {
          status: "unverified",
          profile: "config/review-profiles/certifying-review-four-slot-v1.json",
          record: "config/review-profiles/certifying-review-four-slot-v1.deployment-digest.json",
          repairCommand: expect.stringContaining("npm run profile:catalogue:deploy -- --agents-home"),
        },
        compared: [
          { providerFamily: "openai", state: "clean" },
          { providerFamily: "anthropic", state: "clean" },
        ],
      },
    });
  });

  it("does not let an absent deployment record hide an absent required profile", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await rm(
      join(
        fixture.directory,
        "config",
        "review-profiles",
        "certifying-review-four-slot-v1.json",
      ),
    );

    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
    })).rejects.toMatchObject({
      name: "FabricError",
      code: "NOT_FOUND",
      field: "config/review-profiles/certifying-review-four-slot-v1.json",
    });
  });

  it("rejects a noncanonical in-home review-profile override", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const customDirectory = join(fixture.directory, "config", "review-profiles", "custom");
    const customProfilePath = join(customDirectory, "selected.json");
    await mkdir(customDirectory, { recursive: true });
    await writeFile(customProfilePath, "{}\n");

    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--review-profile", customProfilePath,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
    })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_PATH_FORBIDDEN",
      field: customProfilePath,
    });
  });

  it("rejects a crossed deployment record before invoking a pin observer", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    await writeReviewProfileDeploymentRecord(fixture.directory);
    const recordPath = join(
      fixture.directory,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.deployment-digest.json",
    );
    const record = JSON.parse(await readFile(recordPath, "utf8")) as { profile: string };
    record.profile = "config/review-profiles/crossed.json";
    await writeFile(recordPath, `${JSON.stringify(record)}\n`);
    const observe = vi.fn(async () => ({ status: "observed" as const, model: "unused", detail: "unused" }));

    await expect(fabricDoctor([
      "--consume-provider-quota",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
      observeReviewProfilePin: observe,
    })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_DIGEST_INVALID",
      field: "config/review-profiles/certifying-review-four-slot-v1.deployment-digest.json",
    });
    expect(observe).not.toHaveBeenCalled();
  });

  it("rejects a review-profile override outside agentsHome as outside the deployed catalogue", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const externalDirectory = await mkdtemp(join(tmpdir(), "fabric-external-profile-"));
    cleanup.push(externalDirectory);
    const externalProfile = join(externalDirectory, "profile.json");
    await writeFile(externalProfile, "{}\n");

    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--review-profile", externalProfile,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
    })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_PATH_FORBIDDEN",
      field: externalProfile,
    });
  });

  it("refuses to deploy the canonical source to a noncanonical custom path", async () => {
    const agentsHome = await mkdtemp(join(tmpdir(), "fabric-custom-deployed-profile-"));
    cleanup.push(agentsHome);
    const profilePath = join(agentsHome, "config", "review-profiles", "custom", "selected.json");
    await expect(deployReviewProfileCatalogue({ agentsHome, profilePath })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_PATH_FORBIDDEN",
      field: profilePath,
    });
  });

  it("rejects a deployed profile path that escapes agentsHome through a symlink", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const externalDirectory = await mkdtemp(join(tmpdir(), "fabric-symlinked-profile-"));
    cleanup.push(externalDirectory);
    await writeFile(
      join(externalDirectory, "certifying-review-four-slot-v1.json"),
      "{}\n",
    );
    await mkdir(join(fixture.directory, "config"), { recursive: true });
    await rm(join(fixture.directory, "config", "review-profiles"), { recursive: true });
    await symlink(externalDirectory, join(fixture.directory, "config", "review-profiles"));
    const selected = join(
      fixture.directory,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    );

    await expect(fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      preflightProtocolBuild: async () => undefined,
    })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_PATH_FORBIDDEN",
      field: selected,
    });
  });

  it("does not deploy through a symlinked directory outside agentsHome", async () => {
    const agentsHome = await mkdtemp(join(tmpdir(), "fabric-symlink-deploy-home-"));
    const externalDirectory = await mkdtemp(join(tmpdir(), "fabric-symlink-deploy-target-"));
    cleanup.push(agentsHome, externalDirectory);
    await mkdir(join(agentsHome, "config"), { recursive: true });
    await symlink(externalDirectory, join(agentsHome, "config", "review-profiles"));
    const externalProfile = join(externalDirectory, "certifying-review-four-slot-v1.json");
    await writeFile(externalProfile, "preserved\n");
    const selected = join(
      agentsHome,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    );

    await expect(deployReviewProfileCatalogue({ agentsHome })).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_PATH_FORBIDDEN",
      field: selected,
    });
    expect(await readFile(externalProfile, "utf8")).toBe("preserved\n");
  });

  it("validates source JSON before replacing an existing deployed profile", async () => {
    const repositoryRoot = await mkdtemp(join(tmpdir(), "fabric-malformed-source-"));
    const agentsHome = await mkdtemp(join(tmpdir(), "fabric-preserved-deployment-"));
    cleanup.push(repositoryRoot, agentsHome);
    const sourcePath = join(
      repositoryRoot,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    );
    const profilePath = join(
      agentsHome,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    );
    await mkdir(dirname(sourcePath), { recursive: true });
    await mkdir(dirname(profilePath), { recursive: true });
    await writeFile(sourcePath, "{invalid\n");
    await writeFile(profilePath, "preserved\n");

    await expect(deployReviewProfileCatalogue({
      agentsHome,
      repositoryRoot,
    })).rejects.toThrow();
    expect(await readFile(profilePath, "utf8")).toBe("preserved\n");
  });

  it("deploys the profile and its digest record to a distinct agentsHome through the repair command", async () => {
    const agentsHome = await mkdtemp(join(tmpdir(), "fabric-deployed-profile-"));
    cleanup.push(agentsHome);
    const repositoryRoot = resolve(import.meta.dirname, "../../../..");
    const deployedDirectory = join(agentsHome, "config", "review-profiles");
    await mkdir(deployedDirectory, { recursive: true });
    await writeFile(
      join(deployedDirectory, "certifying-review-four-slot-v1.json"),
      `${JSON.stringify({ handEdited: true })}\n`,
    );
    await writeFile(
      join(deployedDirectory, "certifying-review-four-slot-v1.deployment-digest.json"),
      `${JSON.stringify({
        schemaVersion: 1,
        profile: "config/review-profiles/certifying-review-four-slot-v1.json",
        digest: digestCanonical({ handEdited: true }),
      })}\n`,
    );
    const child = spawn("npm", [
      "run",
      "profile:catalogue:deploy",
      "--",
      "--agents-home",
      agentsHome,
    ], {
      cwd: repositoryRoot,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout?.setEncoding("utf8");
    child.stderr?.setEncoding("utf8");
    child.stdout?.on("data", (chunk: string) => { stdout += chunk; });
    child.stderr?.on("data", (chunk: string) => { stderr += chunk; });
    const exitCode = await new Promise<number | null>((resolvePromise, reject) => {
      child.once("error", reject);
      child.once("exit", resolvePromise);
    });

    expect({ exitCode, stderr }).toStrictEqual({ exitCode: 0, stderr: "" });
    expect(stdout).toContain("config/review-profiles/certifying-review-four-slot-v1.json");
    const profile: unknown = JSON.parse(await readFile(join(
      agentsHome,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    ), "utf8"));
    const record = JSON.parse(await readFile(join(
      agentsHome,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.deployment-digest.json",
    ), "utf8")) as { digest: string };
    const sourceProfile: unknown = JSON.parse(await readFile(join(
      repositoryRoot,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    ), "utf8"));
    expect(profile).toStrictEqual(sourceProfile);
    expect(record.digest).toBe(digestCanonical(profile));
  });

  it("checks a private recovery clone of a hot rollback journal without changing the source state", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await leaveHotRollbackJournal(value.databasePath);
    const before = await directoryByteSnapshot(value.stateDirectory);
    expect(Object.keys(before)).toContain("fabric-v1.sqlite3-journal");

    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value);

    expect((result.checks as Array<{ id: string; status: string }>)).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "database-integrity", status: "pass" }),
    ]));
    expect(await directoryByteSnapshot(value.stateDirectory)).toStrictEqual(before);
  });

  it("documents the provider-quota opt-in in doctor help without running doctor", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const result = await runSourceCli([
      "doctor", "--help",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: value.stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: value.runtimeDirectory,
        AGENT_FABRIC_DATABASE_PATH: value.databasePath,
      },
    });
    expect(result).toMatchObject({ exitCode: 0, signal: null, stderr: "" });
    expect(result.stdout).toContain("--consume-provider-quota");
    expect(result.stdout).toContain("--product-root PATH");
    expect(result.stdout).toContain("--instance-root PATH");
    expect(result.stdout).toContain("run live provider capability probes and refresh the private cache");
  });

  it("does not create an absent state tree at the source CLI boundary", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-doctor-absent-state-"));
    cleanup.push(root);
    const stateDirectory = join(root, "missing-state");
    const runtimeDirectory = join(stateDirectory, "runtime");
    const databasePath = join(stateDirectory, "fabric-v1.sqlite3");
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const producerCallLog = join(fixture.directory, "producer-calls.log");
    await writePoisonedCapabilityProducers(fixture.directory, producerCallLog);

    const result = await runSourceCli([
      "doctor",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
        AGENT_FABRIC_DATABASE_PATH: databasePath,
      },
    });

    expect(result.exitCode).toBe(1);
    await expect(stat(stateDirectory)).rejects.toMatchObject({ code: "ENOENT" });
    await expect(readFile(producerCallLog, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("does not chmod or add entries to pre-existing state directories at the source CLI boundary", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-doctor-existing-state-"));
    cleanup.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(stateDirectory, "runtime");
    const databasePath = join(stateDirectory, "fabric-v1.sqlite3");
    await mkdir(runtimeDirectory, { recursive: true, mode: 0o750 });
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    const before = {
      state: await stat(stateDirectory, { bigint: true }),
      runtime: await stat(runtimeDirectory, { bigint: true }),
      entries: await readdir(stateDirectory),
    };

    const result = await runSourceCli([
      "doctor",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
        AGENT_FABRIC_DATABASE_PATH: databasePath,
      },
    });
    const after = {
      state: await stat(stateDirectory, { bigint: true }),
      runtime: await stat(runtimeDirectory, { bigint: true }),
      entries: await readdir(stateDirectory),
    };

    expect(result.exitCode).toBe(1);
    expect(after.state.mode).toBe(before.state.mode);
    expect(after.state.mtimeNs).toBe(before.state.mtimeNs);
    expect(after.state.ctimeNs).toBe(before.state.ctimeNs);
    expect(after.runtime.mode).toBe(before.runtime.mode);
    expect(after.runtime.mtimeNs).toBe(before.runtime.mtimeNs);
    expect(after.runtime.ctimeNs).toBe(before.runtime.ctimeNs);
    expect(after.entries).toStrictEqual(before.entries);
  });

  it("reports fresh cached pin results as checked and clean without invoking a producer or changing state", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const producerCallLog = join(fixture.directory, "producer-calls.log");
    await writePoisonedCapabilityProducers(fixture.directory, producerCallLog);
    const now = Date.parse("2026-07-25T00:00:00Z");
    await writeFile(join(value.stateDirectory, PIN_OBSERVATION_CACHE_FILE), `${JSON.stringify({
      schemaVersion: 1,
      entries: {
        "openai/flagship/gpt-5.6-sol": { observedModel: "gpt-5.6-sol", observedAtMs: now },
        "anthropic/flagship/opus": { observedModel: "claude-opus-5", observedAtMs: now },
      },
    }, null, 2)}\n`);
    const before = await directoryByteSnapshot(value.stateDirectory);

    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, { now: () => now });

    await expect(readFile(producerCallLog, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    expect(await directoryByteSnapshot(value.stateDirectory)).toStrictEqual(before);
    const check = (result.checks as Array<{
      id: string;
      status: string;
      code: string;
      detail: string;
      precondition: string;
    }>).find((item) => item.id === "review-profile-pins")!;
    expect(check).toMatchObject({
      status: "pass",
      code: "REVIEW_PROFILE_PIN_OK",
      precondition: "each certifying-profile model pin in the automated comparison set matches a provider capability result cached within the last six hours",
    });
    expect(check.detail).toContain("openai/gpt-5.6-sol=clean: flagship resolves to gpt-5.6-sol; cached 0m ago");
    expect(check.detail).toContain("anthropic/claude-opus-5=clean: flagship resolves to claude-opus-5; cached 0m ago");
  });

  it("bypasses a fresh pin cache, invokes stub producers and refreshes the cache only with the quota flag", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
    await writeReviewProfileFixture(fixture.directory);
    const producerCallLog = join(fixture.directory, "producer-calls.log");
    await writeCapabilityProducer(fixture.directory, "codex_capabilities.py", `#!/bin/sh
printf 'codex\\n' >> ${JSON.stringify(producerCallLog)}
printf '%s\\n' '{"schema_version":1,"source":"codex debug models","observed_at":"2026-07-25T00:00:00Z","models":{"gpt-5.6-sol":{"resolved_model":"gpt-5.6-sol"}}}'
`);
    await writeCapabilityProducer(fixture.directory, "claude_capabilities.py", `#!/bin/sh
printf 'claude\\n' >> ${JSON.stringify(producerCallLog)}
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--out" ]; then out=$2; shift 2; continue; fi
  shift
done
printf '%s\\n' '{"schema_version":1,"source":"claude subscription canary","observed_at":"2026-07-25T00:00:00Z","provenance":{"kind":"subscription_runtime_canary","auth_method":"claude.ai","subscription_type":"fixture"},"models":{"opus":{"resolved_model":"claude-opus-5"}}}' > "$out"
`);
    const now = Date.parse("2026-07-25T00:00:00Z");
    const cachePath = join(value.stateDirectory, PIN_OBSERVATION_CACHE_FILE);
    await writeFile(cachePath, `${JSON.stringify({
      schemaVersion: 1,
      entries: {
        "openai/flagship/gpt-5.6-sol": { observedModel: "stale-cache-value", observedAtMs: now },
        "anthropic/flagship/opus": { observedModel: "stale-cache-value", observedAtMs: now },
      },
    }, null, 2)}\n`);

    const result = await fabricDoctor([
      "--consume-provider-quota",
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, { now: () => now });

    expect(await readFile(producerCallLog, "utf8")).toBe("codex\nclaude\n");
    const cache = JSON.parse(await readFile(cachePath, "utf8")) as {
      entries: Record<string, { observedModel: string | null }>;
    };
    expect(cache.entries["openai/flagship/gpt-5.6-sol"]?.observedModel).toBe("gpt-5.6-sol");
    expect(cache.entries["anthropic/flagship/opus"]?.observedModel).toBe("claude-opus-5");
    expect(result.checks).toEqual(expect.arrayContaining([
      expect.objectContaining({
        id: "review-profile-pins",
        status: "pass",
        precondition: "each certifying-profile model pin in the automated comparison set matches alias resolution checked by a live provider capability probe",
        detail: expect.stringContaining("observed live via claude_capabilities.py"),
      }),
    ]));
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
      "--consume-provider-quota",
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
    const checks = result.checks as Array<{ id: string; status: string; code: string; detail: string; precondition: string }>;
    const check = checks.find((item) => item.id === "review-profile-pins")!;
    expect(result.healthy).toBe(healthy);
    expect(check).toMatchObject({
      status,
      code,
      precondition: "each certifying-profile model pin in the automated comparison set matches alias resolution checked by a live provider capability probe",
    });
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
      "--consume-provider-quota",
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
      "--consume-provider-quota",
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

  it("accepts an observed provider version change without comparing versions or digests", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);

    const result = await fabricDoctor([
      "--agents-home", fixture.directory,
      "--trusted-config", fixture.configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ], value, {
      probeProviderInterface: async ({ adapterId }) => ({
        adapterId,
        conformant: true,
        probe: "fixture",
        version: "provider-auto-update-fixture",
      }),
    });
    expect(result).toMatchObject({
      healthy: true,
      providerIdentity: {
        adapters: [
          { adapterId: "claude-agent-sdk", state: "clean" },
          { adapterId: "codex-app-server", state: "clean" },
        ],
      },
    });
    expect(JSON.stringify(result.providerIdentity)).not.toMatch(/version|digest|sha256/u);
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

  it("ignores digest changes while still rejecting an identity assurance mismatch", async () => {
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
        sha256: "0".repeat(64),
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
          { adapterId: "claude-agent-sdk", state: "clean" },
          { adapterId: "codex-app-server", state: "drifted", detail: expect.stringContaining("provider_identity") },
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
          { adapterId: "claude-agent-sdk", state: "drifted", detail: expect.stringContaining("malformed contract response") },
          { adapterId: "codex-app-server", state: "unknown" },
        ],
      },
    });
    const adapters = (result.providerIdentity as { adapters: Array<{ state: string; detail: string }> }).adapters;
    expect(adapters.every((item) => !item.detail.includes("npm run compatibility:pin"))).toBe(true);
  });

  it("reports model catalogue staleness without adapter pin metadata", async () => {
    const value = await paths();
    const fixture = await createPortableActivatedPrimaryFixture();
    cleanup.push(fixture.directory);
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
        modelRouting: { field: "catalog_date", date: "2026-06-24", ageDays: 31, stale: true },
      },
      checks: expect.arrayContaining([
        expect.objectContaining({ id: "source-staleness", status: "pass", code: "SOURCE_STALENESS_ADVISORY" }),
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
        precondition: "each primary provider passes runtime identity and interface conformance checks",
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
        precondition: "a byte-stable copy of the Fabric database is current-schema and passes its invariants",
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
