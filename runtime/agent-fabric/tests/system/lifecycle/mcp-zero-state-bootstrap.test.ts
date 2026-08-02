import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { access, copyFile, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { runWorkspaceTrust } from "../../../src/cli/workspace-trust.ts";
import type { FabricPaths } from "../../../src/cli/paths.ts";
import { projectKey, readLegacyBootstrapSeatGeneration } from "../../../src/cli/seat-store.ts";
import { connectFabricDaemon, FABRIC_OPERATIONS } from "../../../src/index.ts";
import type { AuthorityInput } from "../../../src/domain/types.ts";
import { callTool, spawnMcpProxy, type McpProxy } from "../../support/mcp-testkit.ts";
import { terminateTrackedTestProcess, trackTestProcess } from "../../support/test-process-registry.ts";

const roots: string[] = [];
const daemonPids = new Set<number>();
const mcpMain = fileURLToPath(new URL("../../../src/mcp/main.ts", import.meta.url));
const cliMain = fileURLToPath(new URL("../../../src/cli/main.ts", import.meta.url));
const tsxLoader = fileURLToPath(import.meta.resolve("tsx"));
const compatibilitySource = fileURLToPath(new URL("../../../../../config/adapter-compatibility.yaml", import.meta.url));
const compatibilitySchemaSource = fileURLToPath(new URL("../../../schemas/adapter-compatibility.schema.json", import.meta.url));
const execFileAsync = promisify(execFile);

async function fixture(): Promise<{ projectRoot: string; paths: FabricPaths; agentsHome: string }> {
  const temporaryRoot = await mkdtemp("/tmp/afb-");
  roots.push(temporaryRoot);
  const root = await realpath(temporaryRoot);
  const projectRoot = join(root, "project");
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(root, "runtime");
  const agentsHome = join(root, "agents-home");
  const configDirectory = join(agentsHome, "config");
  const schemaDirectory = join(agentsHome, "runtime", "agent-fabric", "schemas");
  await Promise.all([
    mkdir(projectRoot),
    mkdir(stateDirectory, { mode: 0o700 }),
    mkdir(configDirectory, { recursive: true, mode: 0o700 }),
    mkdir(schemaDirectory, { recursive: true, mode: 0o700 }),
  ]);
  await Promise.all([
    writeFile(join(configDirectory, "agent-fabric.yaml"), [
      "schemaVersion: 1",
      "allowedAdapters: []",
      "activeAdapters: []",
      "allowedProfiles: [headless]",
      "adapters: {}",
      "workspaceRoots:",
      '  - "${AGENTS_HOME}"',
      "limits:",
      "  maximumConcurrentProviderTurns: 8",
      "",
    ].join("\n"), { mode: 0o600 }),
    copyFile(compatibilitySource, join(configDirectory, "adapter-compatibility.yaml")),
    copyFile(compatibilitySchemaSource, join(schemaDirectory, "adapter-compatibility.schema.json")),
  ]);
  const paths: FabricPaths = {
    stateDirectory,
    runtimeDirectory,
    databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
    socketPath: join(runtimeDirectory, "fabric-v1.sock"),
  };
  await runWorkspaceTrust(["trust", projectRoot], paths);
  await expect(access(paths.databasePath)).rejects.toMatchObject({ code: "ENOENT" });
  await expect(access(join(paths.runtimeDirectory, "fabric-v1.discovery.json"))).rejects.toMatchObject({ code: "ENOENT" });
  return { projectRoot, paths, agentsHome };
}

async function openMcpClient(
  seat: "claude" | "codex",
  projectRoot: string,
  paths: FabricPaths,
  agentsHome: string,
  suffix: string = seat,
): Promise<Client> {
  const stderr: string[] = [];
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: ["--import", tsxLoader, mcpMain],
    cwd: projectRoot,
    env: {
      AGENT_FABRIC_SOCKET_PATH: paths.socketPath,
      AGENT_FABRIC_STATE_DIRECTORY: paths.stateDirectory,
      AGENT_FABRIC_RUNTIME_DIRECTORY: paths.runtimeDirectory,
      AGENT_FABRIC_SEAT: seat,
      AGENTS_HOME: agentsHome,
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      TMPDIR: process.env.TMPDIR ?? "/tmp",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
    },
    stderr: "pipe",
  });
  transport.stderr?.on("data", (chunk: Buffer | string) => stderr.push(String(chunk)));
  const client = new Client({ name: `bootstrap-${suffix}`, version: "0.1.0" });
  try {
    await client.connect(transport);
  } catch (cause: unknown) {
    throw new Error(`MCP proxy ${suffix} failed to connect: ${stderr.join("").trim() || "no stderr"}`, { cause });
  }
  return client;
}

afterEach(async () => {
  const currentRoots = roots.splice(0);
  const cleanupPids = new Set(daemonPids);
  await Promise.allSettled(currentRoots.map(async (root) => {
    const discovery = JSON.parse(await readFile(join(root, "runtime", "fabric-v1.discovery.json"), "utf8")) as { pid?: unknown };
    if (typeof discovery.pid === "number") {
      cleanupPids.add(discovery.pid);
      trackTestProcess(discovery.pid, "zero-state-bootstrap-discovered-daemon");
    }
  }));
  await Promise.allSettled([...cleanupPids].map(async (pid) => terminateTrackedTestProcess(pid)));
  for (const pid of cleanupPids) {
    expect(() => process.kill(pid, 0)).toThrow();
  }
  daemonPids.clear();
  await Promise.allSettled(currentRoots.map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("fresh Agent Fabric launch bootstrap", () => {
  it("boots the CLI from no database, discovery receipt, or daemon", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const { stdout } = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      {
        cwd: projectRoot,
        env: {
          AGENT_FABRIC_STATE_DIRECTORY: paths.stateDirectory,
          AGENT_FABRIC_RUNTIME_DIRECTORY: paths.runtimeDirectory,
          AGENT_FABRIC_SEAT: "codex",
          AGENTS_HOME: agentsHome,
          PATH: process.env.PATH ?? "/usr/bin:/bin",
          TMPDIR: process.env.TMPDIR ?? "/tmp",
          ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
        },
        timeout: 15_000,
      },
    );
    const discovery = JSON.parse(await readFile(join(paths.runtimeDirectory, "fabric-v1.discovery.json"), "utf8")) as { pid: number };
    trackTestProcess(discovery.pid, "zero-state-bootstrap-cli-daemon");
    daemonPids.add(discovery.pid);
    expect(stdout).not.toMatch(/"capability"\s*:/u);
    expect(stdout).not.toContain("afc_");
    const output = JSON.parse(stdout) as { canonicalRoot: string; authorityId: string; generation: string; credentials: Array<{ seat: string }> };
    expect(output.canonicalRoot).toBe(projectRoot);
    expect(output.authorityId).toMatch(/^bootstrap-authority:[a-f0-9]{64}:codex$/u);
    expect(output.credentials.map(({ seat }) => seat)).toEqual(["codex"]);
    await expect(readFile(join(
      paths.stateDirectory,
      "seats",
      projectKey(projectRoot),
      "generations",
      output.generation,
      "codex.json",
    ), "utf8").then(JSON.parse)).resolves.toMatchObject({ originKind: "bootstrap" });
    await access(paths.databasePath);
  });

  it("replays an active tagged roster byte-for-byte after session revision advances", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const environment = {
      AGENT_FABRIC_STATE_DIRECTORY: paths.stateDirectory,
      AGENT_FABRIC_RUNTIME_DIRECTORY: paths.runtimeDirectory,
      AGENTS_HOME: agentsHome,
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      TMPDIR: process.env.TMPDIR ?? "/tmp",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
    };
    await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );
    const peer = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "claude"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );
    const active = JSON.parse(peer.stdout) as { generation: string; projectSessionId: string };
    const generationDirectory = join(
      paths.stateDirectory,
      "seats",
      projectKey(projectRoot),
      "generations",
      active.generation,
    );
    const before = await Promise.all(["codex.json", "claude.json", "codex.cap", "claude.cap"].map(
      async (file) => readFile(join(generationDirectory, file), "utf8"),
    ));
    const database = new Database(paths.databasePath);
    try {
      database.prepare("UPDATE project_sessions SET revision=revision+1 WHERE project_session_id=?")
        .run(active.projectSessionId);
    } finally {
      database.close();
    }

    const replay = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );

    expect(JSON.parse(replay.stdout)).toMatchObject({ generation: active.generation });
    await expect(Promise.all(["codex.json", "claude.json", "codex.cap", "claude.cap"].map(
      async (file) => readFile(join(generationDirectory, file), "utf8"),
    ))).resolves.toEqual(before);
  });

  it("replays a legacy bootstrap generation without rewriting its immutable metadata", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const environment = {
      AGENT_FABRIC_STATE_DIRECTORY: paths.stateDirectory,
      AGENT_FABRIC_RUNTIME_DIRECTORY: paths.runtimeDirectory,
      AGENT_FABRIC_SEAT: "codex",
      AGENTS_HOME: agentsHome,
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      TMPDIR: process.env.TMPDIR ?? "/tmp",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
    };
    const first = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );
    const bootstrapped = JSON.parse(first.stdout) as { generation: string };
    const metadataPath = join(
      paths.stateDirectory,
      "seats",
      projectKey(projectRoot),
      "generations",
      bootstrapped.generation,
      "codex.json",
    );
    const legacyMetadata = JSON.parse(await readFile(metadataPath, "utf8")) as Record<string, unknown>;
    delete legacyMetadata.originKind;
    const legacyText = `${JSON.stringify(legacyMetadata, null, 2)}\n`;
    await writeFile(metadataPath, legacyText, { mode: 0o600 });

    const replay = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );

    expect(JSON.parse(replay.stdout)).toMatchObject({ generation: bootstrapped.generation });
    await expect(readFile(metadataPath, "utf8")).resolves.toBe(legacyText);
    await expect(readLegacyBootstrapSeatGeneration({
      stateDirectory: paths.stateDirectory,
      projectPath: projectRoot,
    })).resolves.toBe(bootstrapped.generation);

    const crossedLegacy = { ...legacyMetadata, agentId: "crossed-agent" };
    await writeFile(metadataPath, `${JSON.stringify(crossedLegacy, null, 2)}\n`, { mode: 0o600 });
    await expect(execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    )).rejects.toThrow(/existing MCP seat generation differs/u);
  });

  it("restores a missing active pointer when replaying an exact legacy bootstrap generation", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const environment = {
      AGENT_FABRIC_STATE_DIRECTORY: paths.stateDirectory,
      AGENT_FABRIC_RUNTIME_DIRECTORY: paths.runtimeDirectory,
      AGENT_FABRIC_SEAT: "codex",
      AGENTS_HOME: agentsHome,
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      TMPDIR: process.env.TMPDIR ?? "/tmp",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
    };
    const first = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );
    const bootstrapped = JSON.parse(first.stdout) as { generation: string };
    const seatRoot = join(paths.stateDirectory, "seats", projectKey(projectRoot));
    const metadataPath = join(seatRoot, "generations", bootstrapped.generation, "codex.json");
    const legacyMetadata = JSON.parse(await readFile(metadataPath, "utf8")) as Record<string, unknown>;
    delete legacyMetadata.originKind;
    const legacyText = `${JSON.stringify(legacyMetadata, null, 2)}\n`;
    await writeFile(metadataPath, legacyText, { mode: 0o600 });
    await rm(join(seatRoot, "current.json"));

    const replay = await execFileAsync(
      process.execPath,
      ["--import", tsxLoader, cliMain, "bootstrap", "--seat", "codex"],
      { cwd: projectRoot, env: environment, timeout: 15_000 },
    );

    expect(JSON.parse(replay.stdout)).toMatchObject({ generation: bootstrapped.generation });
    await expect(readFile(metadataPath, "utf8")).resolves.toBe(legacyText);
    await expect(readLegacyBootstrapSeatGeneration({
      stateDirectory: paths.stateDirectory,
      projectPath: projectRoot,
    })).resolves.toBe(bootstrapped.generation);
    await expect(readFile(join(seatRoot, "current.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      generation: bootstrapped.generation,
    });
  });

  it("converges concurrent Claude and Codex zero-state calls and hot-switches both MCP clients", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const clients = await Promise.all((["codex", "claude"] as const).map(async (seat) => ({
      seat,
      client: await openMcpClient(seat, projectRoot, paths, agentsHome),
    })));
    try {
      for (const { client } of clients) {
        expect((await client.listTools()).tools.map(({ name }) => name)).toEqual(["fabric_bootstrap"]);
      }
      const results = await Promise.all(clients.map(async ({ client }) => callTool(client, "fabric_bootstrap", {})));
      if (!results.every((result) => result.isError === false)) throw new Error(JSON.stringify(results));
      const discovery = JSON.parse(await readFile(join(paths.runtimeDirectory, "fabric-v1.discovery.json"), "utf8")) as { pid: number };
      trackTestProcess(discovery.pid, "zero-state-bootstrap-daemon");
      daemonPids.add(discovery.pid);
      for (const { seat, client } of clients) {
        const names = (await client.listTools()).tools.map(({ name }) => name);
        expect(names).not.toContain("fabric_bootstrap");
        expect(names).toContain("fabric_whoami");
        expect(names).toContain("fabric_run_status_read");
        expect(names).toContain("fabric_evidence_publish");
        expect(await callTool(client, "fabric_whoami", {})).toMatchObject({
          isError: false,
          structured: {
            seat,
            authorityId: expect.stringMatching(/^bootstrap-authority:[a-f0-9]{64}:(?:claude|codex)$/u),
            lease: { state: "active" },
          },
        });
      }
      const database = new Database(paths.databasePath, { readonly: true });
      try {
        expect(database.prepare("SELECT count(*) AS count FROM runs").get()).toEqual({ count: 1 });
        expect(database.prepare("SELECT count(*) AS count FROM agents").get()).toEqual({ count: 2 });
        expect(database.prepare("SELECT count(*) AS count FROM mcp_active_seat_generations").get()).toEqual({ count: 1 });
      } finally {
        database.close();
      }
    } finally {
      await Promise.allSettled(clients.map(async ({ client }) => client.close()));
    }
  });

  it("admits a newly trusted exact project through an incumbent daemon", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const first = await openMcpClient("codex", projectRoot, paths, agentsHome, "first-project");
    let second: Client | undefined;
    try {
      expect((await callTool(first, "fabric_bootstrap", {})).isError).toBe(false);
      const discovery = JSON.parse(await readFile(join(paths.runtimeDirectory, "fabric-v1.discovery.json"), "utf8")) as { pid: number };
      trackTestProcess(discovery.pid, "multi-project-bootstrap-daemon");
      daemonPids.add(discovery.pid);

      const secondRoot = join(dirname(projectRoot), "project-b");
      await mkdir(secondRoot);
      await runWorkspaceTrust(["trust", secondRoot], paths);
      second = await openMcpClient("claude", secondRoot, paths, agentsHome, "second-project");
      expect((await second.listTools()).tools.map(({ name }) => name)).toEqual(["fabric_bootstrap"]);
      const result = await callTool(second, "fabric_bootstrap", {});
      if (result.isError) throw new Error(result.text);

      const database = new Database(paths.databasePath, { readonly: true });
      try {
        expect(database.prepare("SELECT count(*) AS count FROM projects").get()).toEqual({ count: 2 });
        expect(database.prepare("SELECT count(*) AS count FROM runs").get()).toEqual({ count: 2 });
      } finally {
        database.close();
      }
    } finally {
      await Promise.allSettled([first.close(), second?.close() ?? Promise.resolve()]);
    }
  });

  it("refreshes stale first-seat proxies once for both tool and resource reads after peer rotation", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const codexTool = await openMcpClient("codex", projectRoot, paths, agentsHome, "codex-tool");
    const clients = [codexTool];
    try {
      expect((await codexTool.listTools()).tools.map(({ name }) => name)).toEqual(["fabric_bootstrap"]);
      expect((await callTool(codexTool, "fabric_bootstrap", {})).isError).toBe(false);
      const discovery = JSON.parse(await readFile(join(paths.runtimeDirectory, "fabric-v1.discovery.json"), "utf8")) as { pid: number };
      trackTestProcess(discovery.pid, "zero-state-bootstrap-daemon");
      daemonPids.add(discovery.pid);
      const codexResource = await openMcpClient("codex", projectRoot, paths, agentsHome, "codex-resource");
      clients.push(codexResource);
      const claude = await openMcpClient("claude", projectRoot, paths, agentsHome);
      clients.push(claude);
      expect((await claude.listTools()).tools.map(({ name }) => name)).toEqual(["fabric_bootstrap"]);
      expect((await callTool(claude, "fabric_bootstrap", {})).isError).toBe(false);

      const database = new Database(paths.databasePath, { readonly: true });
      const run = database.prepare("SELECT run_id FROM runs").get() as { run_id: string };
      database.close();
      const toolResult = await callTool(codexTool, "fabric_run_status_read", { runId: run.run_id });
      expect(toolResult.isError).toBe(false);
      const resource = await codexResource.readResource({ uri: `fabric://runs/${run.run_id}/status` });
      expect(resource.contents).toHaveLength(1);
    } finally {
      await Promise.allSettled(clients.map(async (client) => client.close()));
    }
  });

  it("completes a bootstrap-scoped task request with a correlated artifact-bound reply", async () => {
    const { projectRoot, paths, agentsHome } = await fixture();
    const chair = await openMcpClient("codex", projectRoot, paths, agentsHome, "bootstrap-chair");
    let chairDaemon: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
    let requester: McpProxy | undefined;
    let worker: McpProxy | undefined;
    let forgedOwner: McpProxy | undefined;
    try {
      const bootstrapped = await callTool(chair, "fabric_bootstrap", {});
      if (bootstrapped.isError) throw new Error(bootstrapped.text);
      const discovery = JSON.parse(await readFile(join(paths.runtimeDirectory, "fabric-v1.discovery.json"), "utf8")) as { pid: number };
      trackTestProcess(discovery.pid, "paired-completion-bootstrap-daemon");
      daemonPids.add(discovery.pid);

      const identityResult = await callTool(chair, "fabric_whoami", {});
      expect(identityResult.isError).toBe(false);
      const identity = identityResult.structured as {
        authorityId: string;
        generation: string;
        runId: string;
      };
      const chairCapability = (await readFile(join(
        paths.stateDirectory,
        "seats",
        projectKey(projectRoot),
        "generations",
        identity.generation,
        "codex.cap",
      ), "utf8")).trim();
      chairDaemon = await connectFabricDaemon({ socketPath: paths.socketPath, capability: chairCapability });

      const authorityDatabase = new Database(paths.databasePath, { readonly: true });
      let rootAuthority: AuthorityInput;
      let projectSessionId: string;
      try {
        const authorityRow = authorityDatabase.prepare(
          "SELECT authority_json FROM authorities WHERE authority_id=?",
        ).get(identity.authorityId) as { authority_json: string } | undefined;
        if (authorityRow === undefined) throw new Error("bootstrap authority was not persisted");
        rootAuthority = JSON.parse(authorityRow.authority_json) as AuthorityInput;
        const runRow = authorityDatabase.prepare(
          "SELECT project_session_id FROM runs WHERE run_id=?",
        ).get(identity.runId) as { project_session_id: string } | undefined;
        if (runRow === undefined) throw new Error("bootstrap run identity was not persisted");
        projectSessionId = runRow.project_session_id;
      } finally {
        authorityDatabase.close();
      }

      const deriveProxy = async (
        agentId: string,
        providerSessionRef: string,
        actions: AuthorityInput["actions"],
      ): Promise<McpProxy> => {
        if (chairDaemon === undefined) throw new Error("bootstrap chair daemon is unavailable");
        const authority = await chairDaemon.delegateAuthority({
          parentAuthorityId: identity.authorityId,
          commandId: `bootstrap-paired:${agentId}:authority`,
          authority: { ...rootAuthority, actions },
        });
        const registration = await chairDaemon.registerAgent({
          agentId,
          authorityId: authority.authorityId,
          providerSessionRef,
        });
        return spawnMcpProxy({
          socketPath: paths.socketPath,
          capability: registration.capability,
          label: agentId,
        });
      };

      requester = await deriveProxy(
        "bootstrap-requester",
        "provider-bootstrap-requester",
        [FABRIC_OPERATIONS.taskRequest],
      );
      worker = await deriveProxy(
        "bootstrap-worker",
        "provider-bootstrap-worker",
        [FABRIC_OPERATIONS.taskCompleteWithReply],
      );
      forgedOwner = await deriveProxy(
        "bootstrap-forged-owner",
        "provider-bootstrap-forged-owner",
        [FABRIC_OPERATIONS.taskCompleteWithReply],
      );
      const chairToolNames = (await chair.listTools()).tools.map(({ name }) => name);
      expect(chairToolNames).toContain("fabric_task_request");
      expect(chairToolNames).toContain("fabric_task_complete_with_reply");
      expect(chairToolNames).not.toContain("fabric_task_claim");
      expect((await requester.client.listTools()).tools.map(({ name }) => name)).toContain("fabric_task_request");
      expect((await requester.client.listTools()).tools.map(({ name }) => name)).not.toContain("fabric_task_claim");
      expect((await worker.client.listTools()).tools.map(({ name }) => name)).toContain("fabric_task_complete_with_reply");
      expect((await worker.client.listTools()).tools.map(({ name }) => name)).not.toContain("fabric_task_claim");
      expect((await chair.listTools()).tools.map(({ name }) => name)).not.toContain("fabric_task_claim");

      const taskId = "bootstrap-paired-task";
      const requestMessageId = "bootstrap-paired-request";
      const callbackId = "bootstrap-paired-callback";
      const artifactPath = "paired-reply.md";
      const artifactBytes = "Paired completion evidence.\n";
      const artifactDigest = `sha256:${createHash("sha256").update(artifactBytes).digest("hex")}`;
      const artifactRoot = rootAuthority.artifactPaths[0];
      if (artifactRoot === undefined) throw new Error("bootstrap artifact authority is empty");
      const artifactAbsolutePath = join(projectRoot, artifactRoot, artifactPath);
      await mkdir(dirname(artifactAbsolutePath), { recursive: true });
      await writeFile(artifactAbsolutePath, artifactBytes);
      const request = {
        commandId: "bootstrap-paired:request",
        projectSessionId,
        coordinationRunId: identity.runId,
        task: {
          taskId,
          taskRevision: 1,
          objective: "Return the paired completion evidence.",
          baseRevision: "baseline-integration-6b85f653",
          expectedArtifactPaths: [artifactPath],
        },
        request: {
          requestRevision: 1,
          messageId: requestMessageId,
          conversationId: "bootstrap-paired-conversation",
          targetAgentId: "bootstrap-worker",
          targetProviderSessionRef: "provider-bootstrap-worker",
          requiresAck: true,
          dedupeKey: "bootstrap-paired:request",
          responseDeadline: "2099-01-01T00:00:00.000Z",
          callbackId,
          callbackGeneration: 1,
          dependentBarrierId: "bootstrap-paired-barrier",
        },
      };
      const requested = await callTool(requester.client, "fabric_task_request", request);
      expect(requested.isError).toBe(false);
      expect(requested.structured).toMatchObject({
        taskRevision: 1,
        requestRevision: 1,
        callbackId,
        callbackGeneration: 1,
      });
      const requestReplay = await callTool(requester.client, "fabric_task_request", request);
      expect(requestReplay.isError).toBe(false);
      expect(requestReplay.structured).toEqual(requested.structured);

      await expect(chair.callTool({
        name: "fabric_task_claim",
        arguments: {
          commandId: "bootstrap-paired:raw-claim",
          taskId,
          expectedTaskRevision: 1,
        },
      })).rejects.toThrow(/unknown tool/iu);
      await expect(chairDaemon.delegateAuthority({
        parentAuthorityId: identity.authorityId,
        commandId: "bootstrap-paired:widen-claim",
        authority: {
          ...rootAuthority,
          actions: [...rootAuthority.actions, FABRIC_OPERATIONS.claimTask],
        },
      })).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });

      const completion = {
        commandId: "bootstrap-paired:complete",
        taskId,
        expectedTaskRevision: 1,
        ownerLeaseId: `task-owner:${identity.runId}:${taskId}:1`,
        ownerLeaseGeneration: 1,
        requestMessageId,
        expectedRequestRevision: 1,
        callbackId,
        callbackGeneration: 1,
        reply: {
          messageId: "bootstrap-paired-reply",
          conversationId: "bootstrap-paired-conversation",
          replyToMessageId: requestMessageId,
          body: "Paired completion evidence is ready.",
          artifactRefs: [{ path: artifactPath, digest: artifactDigest }],
        },
        terminalResult: {
          status: "complete" as const,
          summary: "Paired completion succeeded.",
          completedAt: "2099-01-01T00:00:00.000Z",
        },
      };
      const forgedCases = [
        ["lease", worker.client, { ownerLeaseId: `task-owner:${identity.runId}:${taskId}:99` }, "STALE_LEASE_GENERATION"],
        ["callback", worker.client, { callbackId: "bootstrap-forged-callback" }, "STALE_REVISION"],
        ["task", worker.client, { taskId: "bootstrap-forged-task" }, "NOT_FOUND"],
        ["owner", forgedOwner?.client, {}, "STALE_REVISION"],
        ["conversation", worker.client, { reply: { ...completion.reply, conversationId: "bootstrap-forged-conversation" } }, "STALE_REVISION"],
      ] as const;
      for (const [label, client, overrides, code] of forgedCases) {
        if (client === undefined) throw new Error("forged owner client is unavailable");
        const forged = await callTool(client, "fabric_task_complete_with_reply", {
          ...completion,
          ...overrides,
          commandId: `bootstrap-paired:forged:${label}`,
          reply: "reply" in overrides ? { ...completion.reply, ...overrides.reply } : completion.reply,
        });
        expect(forged.isError).toBe(true);
        expect(forged.structured).toMatchObject({ code });
      }
      const uncorrelated = await callTool(worker.client, "fabric_task_complete_with_reply", {
        ...completion,
        commandId: "bootstrap-paired:uncorrelated",
        requestMessageId: "bootstrap-paired-missing-request",
        reply: { ...completion.reply, replyToMessageId: "bootstrap-paired-missing-request" },
      });
      expect(uncorrelated.isError).toBe(true);
      expect(uncorrelated.structured).toMatchObject({ code: "NOT_FOUND" });
      const forgedDatabase = new Database(paths.databasePath, { readonly: true });
      try {
        expect(forgedDatabase.prepare("SELECT state FROM tasks WHERE run_id=? AND task_id=?").get(identity.runId, taskId)).toEqual({ state: "active" });
        expect(forgedDatabase.prepare("SELECT status FROM task_owner_leases WHERE run_id=? AND task_id=?").get(identity.runId, taskId)).toEqual({ status: "active" });
        expect(forgedDatabase.prepare("SELECT COUNT(*) AS count FROM task_results WHERE run_id=? AND task_id=?").get(identity.runId, taskId)).toEqual({ count: 0 });
        expect(forgedDatabase.prepare("SELECT COUNT(*) AS count FROM messages WHERE run_id=? AND kind='response'").get(identity.runId)).toEqual({ count: 0 });
      } finally {
        forgedDatabase.close();
      }

      const completed = await callTool(worker.client, "fabric_task_complete_with_reply", completion);
      expect(completed.isError).toBe(false);
      expect(completed.structured).toMatchObject({
        taskRevision: 2,
        replyRevision: 1,
        resultDelivery: {
          state: "pending",
          projectSessionId,
          taskId,
          requestMessageId,
          replyMessageId: "bootstrap-paired-reply",
          targetAgentId: "bootstrap-requester",
          targetProviderSessionRef: "provider-bootstrap-requester",
          callbackId,
          callbackGeneration: 1,
          payloadDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
        },
      });
      const completionReplay = await callTool(worker.client, "fabric_task_complete_with_reply", completion);
      expect(completionReplay.isError).toBe(false);
      expect(completionReplay.structured).toEqual(completed.structured);

      const evidenceDatabase = new Database(paths.databasePath, { readonly: true });
      try {
        expect(evidenceDatabase.prepare(
          "SELECT relative_path, sha256 FROM artifacts WHERE run_id=? AND task_id=?",
        ).get(identity.runId, taskId)).toEqual({ relative_path: artifactPath, sha256: artifactDigest });
        const storedResult = evidenceDatabase.prepare(
          "SELECT artifacts_json FROM task_results WHERE run_id=? AND task_id=?",
        ).get(identity.runId, taskId) as { artifacts_json: string } | undefined;
        expect(storedResult).toBeDefined();
        expect(JSON.parse(storedResult?.artifacts_json ?? "null")).toEqual([
          { path: artifactPath, digest: artifactDigest },
        ]);
      } finally {
        evidenceDatabase.close();
      }
    } finally {
      await Promise.allSettled([
        requester?.close() ?? Promise.resolve(),
        worker?.close() ?? Promise.resolve(),
        forgedOwner?.close() ?? Promise.resolve(),
        chairDaemon?.close() ?? Promise.resolve(),
        chair.close(),
      ]);
    }
  });
});
