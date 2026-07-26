import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createConnection, createServer, type Server } from "node:net";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  FABRIC_OPERATIONS,
  NATIVE_NOTIFICATION_PROJECTION_FEATURE,
  NdjsonRpcTransport,
  createOperatorClient,
  type OperatorMutationContext,
} from "@local/agent-fabric-protocol";

import {
  FabricDaemonClient,
  startFabricDaemon,
  type FabricDaemonHandle,
} from "../../../src/index.ts";
import {
  CURRENT_CONSOLE_OPTIONAL_FEATURES,
  openLocalOperatorConsoleSession,
} from "../../../src/operator/local-console-session.ts";
import { runWorkspaceTrust } from "../../../src/cli/workspace-trust.ts";

const roots: string[] = [];
const daemons: FabricDaemonHandle[] = [];
const servers: Server[] = [];

afterEach(async () => {
  await Promise.allSettled(daemons.splice(0).reverse().map(async (daemon) => daemon.stop()));
  await Promise.allSettled(servers.splice(0).map(async (server) => await new Promise<void>((resolvePromise, reject) => {
    server.close((error) => error === undefined ? resolvePromise() : reject(error));
  })));
  await Promise.allSettled(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "fabric-console-session-"));
  roots.push(root);
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(root, "runtime");
  const projectA = join(root, "project-a");
  const projectB = join(root, "project-b");
  await Promise.all([
    mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
    mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
    mkdir(projectA),
    mkdir(projectB),
  ]);
  const paths = {
    stateDirectory,
    runtimeDirectory,
    databasePath: join(stateDirectory, "fabric.sqlite3"),
    socketPath: join(runtimeDirectory, "fabric.sock"),
  };
  await Promise.all([
    runWorkspaceTrust(["trust", projectA], paths),
    runWorkspaceTrust(["trust", projectB], paths),
  ]);
  const daemon = await startFabricDaemon({
    ...paths,
    workspaceRoots: [projectA, projectB],
    executionProfile: "headless",
  });
  daemons.push(daemon);
  return { paths, projectA, projectB, daemon };
}

async function regularFiles(root: string): Promise<string[]> {
  const files: string[] = [];
  const visit = async (path: string): Promise<void> => {
    let entries;
    try {
      entries = await readdir(path, { withFileTypes: true });
    } catch (error: unknown) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") return;
      throw error;
    }
    for (const entry of entries) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) await visit(child);
      else if (entry.isFile()) files.push(child);
    }
  };
  await visit(root);
  return files;
}

async function expectSecretsAbsent(
  rootsToScan: readonly string[],
  secrets: readonly string[],
  read: (path: string) => Promise<Buffer> = readFile,
): Promise<void> {
  const files = (await Promise.all(rootsToScan.map(regularFiles))).flat();
  for (const path of files) {
    let bytes;
    try {
      bytes = await read(path);
    } catch (error: unknown) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") continue;
      throw error;
    }
    for (const secret of secrets) {
      expect(bytes.includes(Buffer.from(secret))).toBe(false);
    }
  }
}

describe("secret-absence scan", () => {
  it("tolerates a file disappearing after enumeration and scans stable files", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-secret-scan-"));
    roots.push(root);
    const disappearing = join(root, "fabric.sqlite3-wal");
    const stable = join(root, "fabric.sqlite3");
    await Promise.all([
      writeFile(disappearing, "transient"),
      writeFile(stable, "stable"),
    ]);
    const readPaths: string[] = [];

    await expect(expectSecretsAbsent([root], ["credential-token"], async (path) => {
      readPaths.push(path);
      if (path === disappearing) await rm(path);
      return readFile(path);
    })).resolves.toBeUndefined();
    expect(readPaths).toEqual(expect.arrayContaining([disappearing, stable]));
  });

  it("continues after the first enumerated read disappears and rejects a later credential", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-secret-scan-"));
    roots.push(root);
    await Promise.all([
      writeFile(join(root, "first"), "enumerated"),
      writeFile(join(root, "second"), "enumerated"),
    ]);
    const secret = "credential-token-after-enoent";
    let reads = 0;

    await expect(expectSecretsAbsent([root], [secret], async () => {
      reads += 1;
      if (reads === 1) {
        throw Object.assign(new Error("injected disappearing file"), { code: "ENOENT" });
      }
      return Buffer.from(secret);
    })).rejects.toThrow();
    expect(reads).toBe(2);
  });

  it.each(["credential-token-a", "credential-token-b"])(
    "still fails when %s persists in a stable file",
    async (persistedSecret) => {
      const root = await mkdtemp(join(tmpdir(), "fabric-secret-scan-"));
      roots.push(root);
      await writeFile(join(root, "fabric.sqlite3"), persistedSecret);

      await expect(expectSecretsAbsent(
        [root],
        ["credential-token-a", "credential-token-b"],
      )).rejects.toThrow();
    },
  );

  it("propagates read failures other than a disappearing file", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-secret-scan-"));
    roots.push(root);
    await writeFile(join(root, "fabric.sqlite3"), "stable");
    const denial = Object.assign(new Error("injected read denial"), { code: "EACCES" });

    await expect(expectSecretsAbsent([root], ["credential-token"], async () => {
      throw denial;
    })).rejects.toBe(denial);
  });
});

describe("public local operator Console session", () => {
  it("reuses one daemon and one project authority while attaching concurrent projects and clients", async () => {
    const { paths, projectA, projectB, daemon } = await fixture();

    const [first, second, otherProject] = await Promise.all([
      openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "standalone",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_project_a_01",
      }),
      openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "herdr",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_project_a_02",
      }),
      openLocalOperatorConsoleSession({
        projectRoot: projectB,
        surface: "standalone",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_project_b_01",
      }),
    ]);

    expect(first.daemonPid).toBe(daemon.pid);
    expect(second.daemonPid).toBe(daemon.pid);
    expect(otherProject.daemonPid).toBe(daemon.pid);
    expect(first.projectId).toBe(second.projectId);
    expect(otherProject.projectId).not.toBe(first.projectId);
    expect(first.projectSessionId).toBeUndefined();
    expect(first.client.console?.readOnly).toBe(true);
    expect(first.compatibility).toEqual({ mode: "current" });
    expect(first.client.features).toContain(NATIVE_NOTIFICATION_PROJECTION_FEATURE);
    expect(first.client.features).toContain("artifact-content-read.v1");
    expect(first.client.artifacts).toBeDefined();

    await Promise.all([
      first.detach({ reason: "operator" }),
      first.detach({ reason: "operator" }),
      second.close(),
      second.close(),
      otherProject.close(),
    ]);
    await first.close();

    await expectSecretsAbsent(
      [paths.stateDirectory, paths.runtimeDirectory],
      [first.credential.token, second.credential.token, otherProject.credential.token],
    );

    const bootstrap = await import("better-sqlite3").then(({ default: Database }) =>
      new Database(paths.databasePath, { readonly: true, fileMustExist: true }),
    );
    try {
      expect(bootstrap.prepare("SELECT COUNT(*) AS count FROM projects").get()).toEqual({ count: 2 });
      expect(bootstrap.prepare("SELECT COUNT(*) AS count FROM operator_principals").get()).toEqual({ count: 2 });
      expect(bootstrap.prepare("SELECT COUNT(*) AS count FROM operator_client_attachments WHERE state='active'").get()).toEqual({ count: 0 });
    } finally {
      bootstrap.close();
    }
  });

  it("selects an existing session, issues a fresh bounded credential and never creates a session implicitly", async () => {
    const { paths, projectA, projectB } = await fixture();
    const project = await openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_project_create_01",
    });
    const snapshot = await project.client.projection?.snapshot({
      credential: project.credential,
      projectId: project.projectId,
    });
    const createProjectSession = project.client.operations[FABRIC_OPERATIONS.projectSessionCreate];
    if (snapshot === undefined || createProjectSession === undefined) {
      throw new Error("project client did not negotiate project-session creation");
    }
    const command: OperatorMutationContext = {
      credential: project.credential,
      commandId: "console_create_session_01" as never,
      expectedRevision: snapshot.project.revision,
      actor: project.operatorId,
      provenance: {
        kind: "console-direct-input",
        clientId: project.clientId,
        inputEventId: "console_create_session_input_01",
      },
      evidenceRefs: [],
    };
    await createProjectSession({
      command,
      projectSessionId: "session_console_existing_01" as never,
      projectId: project.projectId,
      mode: "coordinated",
      generation: 1,
      authorityRef: `sha256:${"a".repeat(64)}` as never,
      budgetRef: "budget_console_existing_01",
      launchPacketRef: {
        path: "launch/packet.json" as never,
        digest: `sha256:${"b".repeat(64)}` as never,
      },
    });
    const seed = await import("better-sqlite3").then(({ default: Database }) =>
      new Database(paths.databasePath, { fileMustExist: true }),
    );
    try {
      seed.prepare(`
        INSERT INTO attention_items(
          item_id, project_session_id, coordination_run_id, kind, severity,
          revision, state, dedupe_key, payload_json, created_at, updated_at
        ) VALUES (?, ?, NULL, 'approval', 'critical', 1, 'open', ?, ?, ?, ?)
      `).run(
        "attention_console_optional_01",
        "session_console_existing_01",
        "attention:console:optional:01",
        JSON.stringify({ title: "Optional notification status" }),
        Date.parse("2027-01-01T00:00:00Z"),
        Date.parse("2027-01-01T00:00:00Z"),
      );
    } finally {
      seed.close();
    }

    const opened = await Promise.allSettled([
      openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "standalone",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_session_01",
      }),
      openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "herdr",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_session_02",
      }),
    ]);
    const retained = opened.flatMap((result) =>
      result.status === "fulfilled" ? [result.value] : []);

    let first: Awaited<ReturnType<typeof openLocalOperatorConsoleSession>> | undefined;
    let replay: Awaited<ReturnType<typeof openLocalOperatorConsoleSession>> | undefined;
    try {
      const [firstResult, replayResult] = opened;
      if (firstResult.status === "rejected") throw firstResult.reason;
      if (replayResult.status === "rejected") throw replayResult.reason;
      first = firstResult.value;
      replay = replayResult.value;
      expect(first.projectSessionId).toBe("session_console_existing_01");
      expect(replay.projectSessionId).toBe(first.projectSessionId);
      expect(first.credential.capabilityId).not.toBe(replay.credential.capabilityId);
      expect(first.client.console?.readOnly).toBe(false);
      expect(first.client.operations[FABRIC_OPERATIONS.operatorActionPreview]).toBeTypeOf("function");
      expect(first.client.intakes?.submit).toBeTypeOf("function");
      expect(first.client.intakes?.revise).toBeTypeOf("function");
      expect(first.client.gates?.resolve).toBeTypeOf("function");
      expect(first.client.projectSessions?.transition).toBeTypeOf("function");
      expect(first.client.projectSessions?.close).toBeTypeOf("function");
      expect(first.client.console?.launchAvailable).toBe(true);
      const selectedSessionId = first.projectSessionId;
      if (selectedSessionId === undefined) throw new Error("expected selected session");
      const currentSnapshot = await first.client.projection?.snapshot({
        credential: first.credential,
        projectId: first.projectId,
        projectSessionId: selectedSessionId,
      });
      expect(currentSnapshot?.attention).toMatchObject({
        value: [{
          itemId: "attention_console_optional_01",
          nativeNotification: expect.any(Object),
        }],
      });

      const optionalTransport = await NdjsonRpcTransport.connect(
        createConnection(paths.socketPath),
        {
          protocolVersion: 1,
          client: { name: "current-console-without-notifications", version: "0.1.0" },
          authentication: {
            scheme: "capability",
            credential: first.credential.token,
            clientNonce: "optional_client_current_daemon_nonce_01",
          },
          expectedPrincipalKind: "operator",
          requiredFeatures: ["operator-control.v1", "operator-projection.v1"],
          optionalFeatures: CURRENT_CONSOLE_OPTIONAL_FEATURES,
        },
      );
      try {
        const optionalClient = createOperatorClient(optionalTransport);
        const optionalSnapshot = await optionalClient.projection?.snapshot({
          credential: first.credential,
          projectId: first.projectId,
          projectSessionId: selectedSessionId,
        });
        expect(optionalSnapshot?.attention).toMatchObject({
          value: [{ itemId: "attention_console_optional_01" }],
        });
        expect(optionalSnapshot).not.toHaveProperty("attention.value.0.nativeNotification");
        await optionalClient.close();
      } finally {
        await optionalTransport.close();
      }
    } finally {
      const closes = await Promise.allSettled(
        [project, ...retained].map(async (session) => await session.close()),
      );
      expect(closes.every(({ status }) => status === "fulfilled")).toBe(true);
    }
    if (first === undefined || replay === undefined) throw new Error("concurrent Console sessions did not open");
    await expectSecretsAbsent(
      [paths.stateDirectory, paths.runtimeDirectory],
      [
        project.credential.token,
        first.projectCredential.token,
        first.credential.token,
        replay.projectCredential.token,
        replay.credential.token,
      ],
    );
  });

  it("keeps the project client while explicitly switching between multiple independent sessions", async () => {
    const { paths, projectA, projectB } = await fixture();
    const seed = await openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_multi_seed",
    });
    const createProjectSession = seed.client.operations[FABRIC_OPERATIONS.projectSessionCreate];
    const snapshot = await seed.client.projection?.snapshot({
      credential: seed.credential,
      projectId: seed.projectId,
    });
    if (createProjectSession === undefined || snapshot === undefined) {
      throw new Error("project session creation unavailable");
    }
    for (const [index, projectSessionId] of [
      "session_independent_a",
      "session_independent_b",
    ].entries()) {
      await createProjectSession({
        command: {
          credential: seed.credential,
          commandId: `console_multi_create_${String(index)}` as never,
          expectedRevision: snapshot.project.revision + index,
          actor: seed.operatorId,
          provenance: {
            kind: "console-direct-input",
            clientId: seed.clientId,
            inputEventId: `console_multi_create_input_${String(index)}`,
          },
          evidenceRefs: [],
        },
        projectSessionId: projectSessionId as never,
        projectId: seed.projectId,
        mode: "independent",
        generation: 1,
        authorityRef: `sha256:${"a".repeat(64)}` as never,
        budgetRef: `budget_${projectSessionId}`,
        launchPacketRef: {
          path: `launch/${projectSessionId}.json` as never,
          digest: `sha256:${"b".repeat(64)}` as never,
        },
      });
    }
    await seed.close();

    const consoleSession = await openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_multi_selector",
    });
    try {
      expect(consoleSession.projectSessionId).toBeUndefined();
      expect(consoleSession.client).toBe(consoleSession.projectClient);
      expect(consoleSession.client.console?.readOnly).toBe(true);
      expect(consoleSession.attachableProjectSessions.map(({ projectSessionId }) =>
        projectSessionId).sort()).toStrictEqual([
        "session_independent_a",
        "session_independent_b",
      ]);

      await consoleSession.selectProjectSession("session_independent_b" as never);
      expect(consoleSession.projectSessionId).toBe("session_independent_b");
      expect(consoleSession.client).not.toBe(consoleSession.projectClient);
      expect(consoleSession.client.console?.readOnly).toBe(false);
      await expect(consoleSession.projectClient.projection?.discover({
        credential: consoleSession.projectCredential,
        projectId: consoleSession.projectId,
        after: 0,
        limit: 10,
      })).resolves.toMatchObject({
        sessions: { value: { items: expect.arrayContaining([
          expect.objectContaining({ projectSessionId: "session_independent_a" }),
          expect.objectContaining({ projectSessionId: "session_independent_b" }),
        ]) } },
      });

      await consoleSession.selectProject();
      expect(consoleSession.projectSessionId).toBeUndefined();
      expect(consoleSession.client).toBe(consoleSession.projectClient);

      await consoleSession.selectProjectSession("session_independent_a" as never);
      expect(consoleSession.projectSessionId).toBe("session_independent_a");
    } finally {
      await consoleSession.close();
    }
  });

  it("rotates an expired principal generation, revokes prior capabilities and reopens concurrently", async () => {
    const { paths, projectA, projectB } = await fixture();
    const expired = await openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_expiring_01",
    });
    const expiredCapabilityId = expired.credential.capabilityId;
    const expiredToken = expired.credential.token;
    await expired.close();

    const expiryDatabase = await import("better-sqlite3").then(({ default: Database }) =>
      new Database(paths.databasePath, { fileMustExist: true }),
    );
    try {
      expect(expiryDatabase.prepare(`
        UPDATE operator_capabilities SET expires_at=1
         WHERE capability_id=? AND principal_generation=1 AND revoked_at IS NULL
      `).run(expiredCapabilityId).changes).toBe(1);
    } finally {
      expiryDatabase.close();
    }

    const originalProvision = FabricDaemonClient.prototype.openLocalOperatorConsoleCapability;
    let entered = 0;
    const bothEntered = Promise.withResolvers<void>();
    const provisionedGenerations: number[] = [];
    const provisionFailures: unknown[] = [];
    const provision = vi.spyOn(
      FabricDaemonClient.prototype,
      "openLocalOperatorConsoleCapability",
    ).mockImplementation(async function (this: FabricDaemonClient, input) {
      entered += 1;
      if (entered === 2) bothEntered.resolve();
      await bothEntered.promise;
      try {
        const issued = await originalProvision.call(this, input);
        provisionedGenerations.push(issued.principalGeneration);
        return issued;
      } catch (error: unknown) {
        provisionFailures.push(error);
        throw error;
      }
    });

    const opened = await Promise.allSettled([
      openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "standalone",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_rotated_01",
      }),
      openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "herdr",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_rotated_02",
      }),
    ]).finally(() => provision.mockRestore());
    expect(entered).toBe(2);
    expect(provisionFailures).toEqual([]);
    expect(provisionedGenerations).toEqual([2, 2]);
    expect(opened.map(({ status }) => status)).toEqual(["fulfilled", "fulfilled"]);
    if (opened[0].status === "rejected") throw opened[0].reason;
    if (opened[1].status === "rejected") throw opened[1].reason;
    const [first, second] = [opened[0].value, opened[1].value];
    try {
      expect(first.projectId).toBe(expired.projectId);
      expect(second.projectId).toBe(expired.projectId);
      expect(first.credential.capabilityId).not.toBe(second.credential.capabilityId);
      const database = await import("better-sqlite3").then(({ default: Database }) =>
        new Database(paths.databasePath, { readonly: true, fileMustExist: true }),
      );
      try {
        expect(database.prepare(`
          SELECT principal_generation FROM operator_principals WHERE operator_id=?
        `).get(first.operatorId)).toEqual({ principal_generation: 2 });
        expect(database.prepare(`
          SELECT revoked_at IS NOT NULL AS revoked FROM operator_capabilities WHERE capability_id=?
        `).get(expiredCapabilityId)).toEqual({ revoked: 1 });
      } finally {
        database.close();
      }
    } finally {
      await Promise.all([first.close(), second.close()]);
    }
    await expectSecretsAbsent(
      [paths.stateDirectory, paths.runtimeDirectory],
      [expiredToken, first.credential.token, second.credential.token],
    );
  });

  it("paginates past one hundred non-attachable sessions to select an older attachable session", async () => {
    const { paths, projectA, projectB } = await fixture();
    const project = await openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_pagination_seed",
    });
    const database = await import("better-sqlite3").then(({ default: Database }) =>
      new Database(paths.databasePath, { fileMustExist: true }),
    );
    try {
      const insert = database.prepare(`
        INSERT INTO project_sessions(
          project_session_id, project_id, mode, state, revision, generation,
          authority_ref, budget_ref, launch_packet_path, launch_packet_digest,
          membership_revision, origin_kind, origin_operator_id,
          terminal_path_json, created_at, updated_at
        ) VALUES (?, ?, 'coordinated', ?, 1, 1, ?, ?, ?, ?, 1,
                  'operator-launch', ?, NULL, ?, ?)
      `);
      database.transaction(() => {
        insert.run(
          "session_pagination_active",
          project.projectId,
          "draft",
          `sha256:${"a".repeat(64)}`,
          "budget_pagination_active",
          "launch/active.json",
          `sha256:${"b".repeat(64)}`,
          project.operatorId,
          1,
          1,
        );
        for (let index = 0; index < 101; index += 1) {
          insert.run(
            `session_pagination_terminal_${String(index).padStart(3, "0")}`,
            project.projectId,
            "launch_failed",
            `sha256:${"a".repeat(64)}`,
            `budget_pagination_${String(index)}`,
            `launch/${String(index)}.json`,
            `sha256:${"b".repeat(64)}`,
            project.operatorId,
            index + 2,
            index + 2,
          );
        }
      })();
    } finally {
      database.close();
    }

    const projection = project.projectClient.projection;
    if (projection === undefined) throw new Error("project discovery unavailable");
    const firstPage = await projection.discover({
      credential: project.projectCredential,
      projectId: project.projectId,
      after: 0,
      limit: 100,
    });
    expect(firstPage).toMatchObject({
      project: { freshness: "live", value: { projectId: project.projectId } },
      sessions: { freshness: "live", value: { hasMore: true, nextCursor: 100 } },
    });
    if (firstPage.sessions.freshness !== "live") throw new Error("first discovery page unavailable");
    expect(firstPage.sessions.value.items).toHaveLength(100);
    expect(firstPage.sessions.value.items.every(({ state }) => state === "launch_failed")).toBe(true);
    const secondPage = await projection.discover({
      credential: project.projectCredential,
      projectId: project.projectId,
      after: firstPage.sessions.value.nextCursor,
      limit: 100,
    });
    expect(secondPage).toMatchObject({
      project: { freshness: "live", value: { projectId: project.projectId } },
      sessions: { freshness: "live", value: { hasMore: false, nextCursor: 102 } },
    });
    if (secondPage.sessions.freshness !== "live") throw new Error("second discovery page unavailable");
    expect(secondPage.sessions.value.items.map(({ projectSessionId }) => projectSessionId)).toEqual([
      "session_pagination_terminal_000",
      "session_pagination_active",
    ]);

    let selected: Awaited<ReturnType<typeof openLocalOperatorConsoleSession>> | undefined;
    let retrySelected: Awaited<ReturnType<typeof openLocalOperatorConsoleSession>> | undefined;
    try {
      selected = await openLocalOperatorConsoleSession({
        projectRoot: projectA,
        surface: "standalone",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_pagination_selected",
      });
      expect(selected.attachableProjectSessions.map(({ projectSessionId }) => projectSessionId)).toEqual([
        "session_pagination_active",
      ]);
      expect(selected.projectSessionId).toBe("session_pagination_active");
      const selectedSessionId = selected.projectSessionId;
      if (selectedSessionId === undefined) throw new Error("pagination session was not selected");
      await expect(selected.client.projection?.snapshot({
        credential: selected.credential,
        projectId: selected.projectId,
        projectSessionId: selectedSessionId,
      })).resolves.toMatchObject({
        session: {
          freshness: "live",
          value: { projectSessionId: "session_pagination_active" },
        },
      });
      await selected.selectProjectSession("session_pagination_terminal_000" as never);
      expect(selected.projectSessionId).toBe("session_pagination_terminal_000");
      expect(selected.attachableProjectSessions.map(({ projectSessionId }) => projectSessionId)).toEqual([
        "session_pagination_active",
      ]);
      retrySelected = await openLocalOperatorConsoleSession({
        projectRoot: projectA,
        projectSessionId: "session_pagination_terminal_001" as never,
        surface: "standalone",
        paths,
        daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
        clientId: "console_pagination_retry",
      });
      expect(retrySelected.projectSessionId).toBe("session_pagination_terminal_001");
      expect(retrySelected.attachableProjectSessions.map(({ projectSessionId }) => projectSessionId)).toEqual([
        "session_pagination_active",
      ]);
    } finally {
      await Promise.allSettled([
        retrySelected?.close() ?? Promise.resolve(),
        selected?.close() ?? Promise.resolve(),
        project.close(),
      ]);
    }
    await expectSecretsAbsent(
      [paths.stateDirectory, paths.runtimeDirectory],
      [
        project.credential.token,
        ...(selected === undefined ? [] : [selected.projectCredential.token, selected.credential.token]),
        ...(retrySelected === undefined ? [] : [retrySelected.projectCredential.token, retrySelected.credential.token]),
      ],
    );
  });

  it("fails closed for untrusted roots and never creates default authority", async () => {
    const { paths, projectA, projectB } = await fixture();
    const untrusted = join(projectA, "untrusted-child");
    await mkdir(untrusted);

    await expect(openLocalOperatorConsoleSession({
      projectRoot: untrusted,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_untrusted_01",
    })).rejects.toMatchObject({
      code: "CONSOLE_CONFIGURATION_UNAVAILABLE",
      reason: "configuration-missing",
    });
  });

  it("lets an explicit operator daemon-stop drain its own dispatch custody and exit", async () => {
    const barrierRoot = await mkdtemp(join(tmpdir(), "fabric-idle-stop-attempt-"));
    roots.push(barrierRoot);
    const barrierPath = join(barrierRoot, "attempt.sock");
    const barrier = createServer();
    servers.push(barrier);
    const idleStopAttempted = new Promise<void>((resolvePromise, reject) => {
      barrier.once("connection", (socket) => {
        const lines = createInterface({ input: socket, crlfDelay: Infinity });
        lines.once("line", (frame) => {
          lines.close();
          if (frame !== "idle-stop-attempt:operator-detach") {
            reject(new Error(`unexpected idle-stop attempt frame: ${JSON.stringify(frame)}`));
            return;
          }
          resolvePromise();
        });
        socket.once("error", reject);
      });
      barrier.once("error", reject);
    });
    await new Promise<void>((resolvePromise, reject) => {
      barrier.once("error", reject);
      barrier.listen(barrierPath, () => {
        barrier.off("error", reject);
        resolvePromise();
      });
    });
    const previousBarrierPath = process.env.AGENT_FABRIC_TEST_IDLE_STOP_ATTEMPT_SOCKET_PATH;
    process.env.AGENT_FABRIC_TEST_IDLE_STOP_ATTEMPT_SOCKET_PATH = barrierPath;
    const { paths, projectA, projectB, daemon } = await fixture().finally(() => {
      if (previousBarrierPath === undefined) delete process.env.AGENT_FABRIC_TEST_IDLE_STOP_ATTEMPT_SOCKET_PATH;
      else process.env.AGENT_FABRIC_TEST_IDLE_STOP_ATTEMPT_SOCKET_PATH = previousBarrierPath;
    });
    const session = await openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_daemon_stop_01",
    });
    const projectSnapshot = await session.projectClient.projection?.snapshot({
      credential: session.projectCredential,
      projectId: session.projectId,
    });
    const createProjectSession = session.projectClient.operations[FABRIC_OPERATIONS.projectSessionCreate];
    if (projectSnapshot === undefined || createProjectSession === undefined) {
      throw new Error("project client did not negotiate project-session creation");
    }
    await createProjectSession({
      command: {
        credential: session.projectCredential,
        commandId: "console_daemon_stop_session_create" as never,
        expectedRevision: projectSnapshot.project.revision,
        actor: session.operatorId,
        provenance: {
          kind: "console-direct-input",
          clientId: session.clientId,
          inputEventId: "console_daemon_stop_session_create_input",
        },
        evidenceRefs: [],
      },
      projectSessionId: "session_console_daemon_stop_01" as never,
      projectId: session.projectId,
      mode: "coordinated",
      generation: 1,
      authorityRef: `sha256:${"a".repeat(64)}` as never,
      budgetRef: "budget_console_daemon_stop_01",
      launchPacketRef: {
        path: "launch/daemon-stop.json" as never,
        digest: `sha256:${"b".repeat(64)}` as never,
      },
    });
    await session.refreshProjectSessions();
    await session.selectProjectSession("session_console_daemon_stop_01" as never);
    await session.detach({ reason: "operator" });
    await expect(Promise.race([
      idleStopAttempted.then(() => "idle-stop-attempt" as const),
      daemon.waitForExit().then(() => "daemon-exit" as const),
    ])).resolves.toBe("idle-stop-attempt");

    const readEpoch = async () => {
      const { default: Database } = await import("better-sqlite3");
      const database = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
      try {
        const epoch = database.prepare(`
          SELECT instance_generation AS instanceGeneration, state, observed_global_revision AS observedGlobalRevision
            FROM daemon_runtime_epochs ORDER BY instance_generation DESC LIMIT 1
        `).get() as { instanceGeneration: number; state: string; observedGlobalRevision: number | null };
        const global = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1")
          .get() as { revision: number };
        return { ...epoch, globalRevision: global.revision };
      } finally {
        database.close();
      }
    };
    const actions = session.client.console?.actions;
    if (actions === undefined) throw new Error("session client did not negotiate operator actions");
    const command = (commandId: string, expectedRevision: number) => ({
      credential: session.credential,
      commandId: commandId as never,
      expectedRevision,
      actor: session.operatorId,
      provenance: {
        kind: "console-direct-input" as const,
        clientId: session.clientId,
        inputEventId: `${commandId}:input`,
      },
      evidenceRefs: [],
    });

    const beforeDrain = await readEpoch();
    expect(beforeDrain.state).toBe("running");
    const drainPreview = await actions.preview({
      command: command("console_daemon_drain_preview_01", beforeDrain.globalRevision),
      projectId: session.projectId,
      intent: {
        kind: "daemon-drain",
        expectedDaemonGeneration: beforeDrain.instanceGeneration,
        expectedGlobalStateRevision: beforeDrain.globalRevision,
      },
    });
    const drained = await actions.commit({
      command: command("console_daemon_drain_commit_01", beforeDrain.globalRevision),
      projectId: session.projectId,
      previewId: drainPreview.previewId,
      expectedPreviewRevision: drainPreview.previewRevision,
      previewDigest: drainPreview.previewDigest,
      expectedIntentDigest: drainPreview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_console_daemon_drain_01" },
    });
    if (drained.effectRef === undefined) throw new Error("daemon drain did not return its receipt");

    const beforeStop = await readEpoch();
    expect(beforeStop).toMatchObject({
      instanceGeneration: beforeDrain.instanceGeneration,
      state: "quiescing",
      observedGlobalRevision: beforeStop.globalRevision,
    });
    const stopPreview = await actions.preview({
      command: command("console_daemon_stop_preview_01", beforeStop.globalRevision),
      projectId: session.projectId,
      intent: {
        kind: "daemon-stop",
        expectedDaemonGeneration: beforeStop.instanceGeneration,
        expectedGlobalStateRevision: beforeStop.globalRevision,
        drainReceiptRef: drained.effectRef,
      },
    });
    await actions.commit({
      command: command("console_daemon_stop_commit_01", beforeStop.globalRevision),
      projectId: session.projectId,
      previewId: stopPreview.previewId,
      expectedPreviewRevision: stopPreview.previewRevision,
      previewDigest: stopPreview.previewDigest,
      expectedIntentDigest: stopPreview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_console_daemon_stop_01" },
    });

    await daemon.waitForExit();
    const { default: Database } = await import("better-sqlite3");
    const database = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(database.prepare(`
        SELECT state FROM daemon_runtime_epochs WHERE instance_generation=?
      `).get(beforeStop.instanceGeneration)).toEqual({ state: "stopped" });
      expect(database.prepare(`
        SELECT state FROM operator_daemon_stop_custody WHERE command_id='console_daemon_stop_commit_01'
      `).get()).toEqual({ state: "stopped" });
      expect(database.prepare(`
        SELECT state FROM operator_effect_custody WHERE command_id='console_daemon_stop_commit_01'
      `).get()).toEqual({ state: "terminal" });
    } finally {
      database.close();
    }
  });

  it("never direct-stops a ready shared daemon when its first Console fails late", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-console-late-failure-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const projectA = join(root, "project-a");
    const projectB = join(root, "project-b");
    await Promise.all([
      mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
      mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
      mkdir(projectA),
      mkdir(projectB),
    ]);
    const paths = {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric.sock"),
    };
    await runWorkspaceTrust(["trust", projectA], paths);
    await runWorkspaceTrust(["trust", projectB], paths);

    let releaseProvision!: () => void;
    let provisionEntered!: () => void;
    const entered = new Promise<void>((resolvePromise) => { provisionEntered = resolvePromise; });
    const blocked = new Promise<void>((resolvePromise) => { releaseProvision = resolvePromise; });
    const provision = vi.spyOn(
      FabricDaemonClient.prototype,
      "openLocalOperatorConsoleCapability",
    ).mockImplementationOnce(async () => {
      provisionEntered();
      await blocked;
      throw new Error("injected late authority failure");
    });
    const first = openLocalOperatorConsoleSession({
      projectRoot: projectA,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_late_failure_01",
    });
    void first.catch(() => undefined);
    await entered;

    const survivor = await openLocalOperatorConsoleSession({
      projectRoot: projectB,
      surface: "standalone",
      paths,
      daemon: { executionProfile: "headless", workspaceRoots: [projectA, projectB] },
      clientId: "console_survivor_01",
    });
    try {
      releaseProvision();

      await expect(first).rejects.toMatchObject({
        code: "CONSOLE_AUTHORITY_UNAVAILABLE",
      });
      process.kill(survivor.daemonPid, 0);
      await expect(survivor.client.projection?.snapshot({
        credential: survivor.credential,
        projectId: survivor.projectId,
      })).resolves.toMatchObject({
        project: { freshness: "live", value: { projectId: survivor.projectId } },
      });
    } finally {
      provision.mockRestore();
      await survivor.close().catch(() => undefined);
      try { process.kill(survivor.daemonPid, "SIGTERM"); } catch { /* already stopped */ }
    }
  });
});
