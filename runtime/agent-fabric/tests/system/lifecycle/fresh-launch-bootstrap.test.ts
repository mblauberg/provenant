import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { FABRIC_OPERATIONS, type OperatorMutationContext } from "@local/agent-fabric-protocol";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { runWorkspaceTrust } from "../../../src/cli/workspace-trust.ts";
import { provisionMcpSeats } from "../../../src/cli/mcp-provision.ts";
import type { FabricPaths } from "../../../src/cli/paths.ts";
import {
  openLocalOperatorConsoleSession,
  type LocalOperatorConsoleSession,
} from "../../../src/operator/local-console-session.ts";
import {
  normaliseLaunchChairAuthority,
} from "../../../src/project-session/launch-custody.ts";
import { canonicalJson } from "../../../src/persistence/row-codec.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "../../support/authority-v2-testkit.ts";
import {
  terminateTrackedTestProcess,
  trackTestProcess,
} from "../../support/test-process-registry.ts";

const roots: string[] = [];
const consoles: LocalOperatorConsoleSession[] = [];
const daemonPids: number[] = [];
const freshAdapter = fileURLToPath(new URL("../../support/fresh-launch-adapter.ts", import.meta.url));
const mcpMain = fileURLToPath(new URL("../../../src/mcp/main.ts", import.meta.url));
// Spawn from the package root so the bare `tsx` --import specifier resolves;
// the served project stays bound through AGENT_FABRIC_PROJECT_PATH.
const packageRoot = fileURLToPath(new URL("../../..", import.meta.url));
const launchContract = {
  schemaVersion: 1,
  method: "launch_chair",
  oneUse: true,
  secretTransport: "private-environment",
  environment: {
    capability: "AGENT_FABRIC_CAPABILITY",
    socketPath: "AGENT_FABRIC_SOCKET_PATH",
    attestationChallenge: "AGENT_FABRIC_ATTESTATION_CHALLENGE",
  },
  inputSchemaId: "fresh-launch-input.v1",
  publicPayloadSchema: {
    type: "object",
    additionalProperties: false,
    required: ["model"],
    properties: { model: { type: "string", minLength: 1 } },
  },
  noEffectProofSchemas: {
    "provider-no-effect.v1": {
      type: "object",
      additionalProperties: false,
      required: ["effectCount"],
      properties: { effectCount: { const: 0 } },
    },
  },
  attestation: {
    method: "provider-session-random-challenge-v1",
    bridgeContract: "agent-fabric-session-bridge-v1",
    origin: "provider-session-tool-call",
    oneUse: true,
    bridgeLifetime: "provider-session",
    digestAlgorithm: "sha256",
    nativeAttribution: "claude-sdk-assistant-request-tool-use-v1",
  },
} as const;

afterEach(async () => {
  await Promise.allSettled(consoles.splice(0).reverse().map(async (console) => console.close()));
  await Promise.allSettled(daemonPids.splice(0).map(async (pid) => await terminateTrackedTestProcess(pid)));
  await Promise.allSettled(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

function digest(text: string): `sha256:${string}` {
  return `sha256:${createHash("sha256").update(text).digest("hex")}`;
}

function command(
  console: LocalOperatorConsoleSession,
  commandId: string,
  expectedRevision: number,
): OperatorMutationContext {
  return {
    credential: console.credential,
    commandId: commandId as never,
    expectedRevision,
    actor: console.operatorId,
    provenance: {
      kind: "console-direct-input",
      clientId: console.clientId,
      inputEventId: `${commandId}:input`,
    },
    evidenceRefs: [],
  };
}

describe("fresh Agent Fabric launch bootstrap", () => {
  it("prepares the first operator-owned launch from a clean database without seeded rows or caller-authored custody digests", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-fresh-launch-"));
    roots.push(root);
    const projectRoot = join(root, "project");
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    await Promise.all([
      mkdir(projectRoot),
      mkdir(stateDirectory, { mode: 0o700 }),
      mkdir(runtimeDirectory, { mode: 0o700 }),
    ]);
    const paths: FabricPaths = {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric.sock"),
    };
    await runWorkspaceTrust(["trust", projectRoot], paths);
    const peerReadyPath = join(root, "claude-peer-ready");
    const peerTriggerPath = join(root, "claude-peer-trigger");
    const peerAuthority = {
      ...TEST_AUTHORITY_V2_FIELDS,
      workspaceRoots: ["."],
      sourcePaths: ["."],
      artifactPaths: [".agent-run/fresh-launch-01/claude-peer"],
      actions: [FABRIC_OPERATIONS.getRunStatus, FABRIC_OPERATIONS.getMailboxState],
      deniedPaths: [],
      deniedActions: [],
      disclosure: { level: "forbidden" } as const,
      expiresAt: "2099-01-01T00:00:00.000Z",
      budget: { provider_calls: 2 },
    };

    const console = await openLocalOperatorConsoleSession({
      projectRoot,
      surface: "standalone",
      paths,
      daemon: {
        executionProfile: "headless",
        workspaceRoots: [projectRoot],
        adapters: {
          "claude-agent-sdk": {
            command: [process.execPath, "--import", "tsx", freshAdapter],
            environment: {
              FRESH_LAUNCH_CONTRACT_JSON: canonicalJson(launchContract),
              FRESH_LAUNCH_PEER_AUTHORITY_JSON: canonicalJson(peerAuthority),
              FRESH_LAUNCH_PEER_READY_PATH: peerReadyPath,
              FRESH_LAUNCH_PEER_TRIGGER_PATH: peerTriggerPath,
            },
          },
        },
      },
      clientId: "console_fresh_launch_01",
    });
    trackTestProcess(console.daemonPid, "fresh-launch-bootstrap-daemon");
    daemonPids.push(console.daemonPid);
    consoles.push(console);

    const projectSessions = console.projectClient.projectSessions;
    if (projectSessions?.create === undefined) throw new Error("project-session creation unavailable");
    const projectSessionId = "session_fresh_launch_01" as never;
    const chairAuthority = {
      ...TEST_AUTHORITY_V2_FIELDS,
      workspaceRoots: ["."],
      sourcePaths: ["."],
      artifactPaths: [".agent-run/fresh-launch-01"],
      actions: [
        FABRIC_OPERATIONS.getRunStatus,
        FABRIC_OPERATIONS.getMailboxState,
        FABRIC_OPERATIONS.createTeam,
        FABRIC_OPERATIONS.registerAgent,
      ],
      deniedPaths: [],
      deniedActions: [],
      disclosure: { level: "forbidden" } as const,
      expiresAt: "2099-01-01T00:00:00.000Z",
      budget: { provider_calls: 10, concurrent_turns: 1 },
    };
    const normalisedAuthority = normaliseLaunchChairAuthority(chairAuthority, projectRoot);
    const resourcePlan = {
      schemaVersion: 1,
      projectId: console.projectId,
      projectSessionId,
      runId: "run_fresh_launch_01",
      budgetRef: "budget_fresh_launch_01",
      scopes: {
        project: { scopeId: "scope_fresh_project_01", limits: { provider_calls: 10, concurrent_turns: 1 } },
        projectSession: { scopeId: "scope_fresh_session_01", limits: { provider_calls: 10, concurrent_turns: 1 } },
        coordinationRun: { scopeId: "scope_fresh_run_01", limits: { provider_calls: 10, concurrent_turns: 1 } },
      },
      launchReservation: { amounts: { provider_calls: 1, concurrent_turns: 1 } },
    };
    const resourcePlanText = canonicalJson(resourcePlan);
    const resourcePlanRef = {
      path: "launch/resource-plan.json" as never,
      digest: digest(resourcePlanText) as never,
    };
    const launchPacket = {
      schemaVersion: 1,
      projectId: console.projectId,
      projectSessionId,
      runId: "run_fresh_launch_01",
      chairAgentId: "codex_fresh_chair_01",
      projectRunDirectory: ".agent-run/fresh-launch-01",
      topologyMode: "coordinated",
      budgetRef: "budget_fresh_launch_01",
      resourcePlanRef,
      chairAuthority,
      provider: {
        adapterId: "claude-agent-sdk",
        actionId: "provider_fresh_chair_01",
        contractDigest: digest(canonicalJson(launchContract)),
        inputSchemaId: launchContract.inputSchemaId,
        input: { model: "fixture-claude" },
      },
    };
    const packetText = `${JSON.stringify(launchPacket)}\n`;
    const launchPacketRef = {
      path: "launch/packet.json" as never,
      digest: digest(packetText) as never,
    };
    await mkdir(join(projectRoot, "launch"));
    await Promise.all([
      writeFile(join(projectRoot, "launch", "packet.json"), packetText, { mode: 0o600 }),
      writeFile(join(projectRoot, "launch", "resource-plan.json"), resourcePlanText, { mode: 0o600 }),
    ]);

    const created = await projectSessions.create({
      command: command(console, "create_fresh_launch_01", 1),
      projectSessionId,
      projectId: console.projectId,
      mode: "coordinated",
      generation: 1,
      authorityRef: digest(canonicalJson(normalisedAuthority)) as never,
      budgetRef: "budget_fresh_launch_01",
      launchPacketRef,
    });
    expect(created).toMatchObject({ state: "draft", revision: 1, generation: 1 });

    await console.selectProjectSession(projectSessionId);
    const sessionApi = console.client.projectSessions;
    if (sessionApi?.transition === undefined) throw new Error("session project-session transition unavailable");
    const awaitingLaunch = await sessionApi.transition({
      command: command(console, "await_fresh_launch_01", 1),
      projectSessionId,
      expectedGeneration: 1,
      transition: {
        to: "awaiting_launch",
        reason: "operator reviewed launch packet",
        launchPacketRef,
      },
    });
    expect(awaitingLaunch).toMatchObject({ state: "awaiting_launch", revision: 2, generation: 1 });

    if (sessionApi.prepareLaunch === undefined) throw new Error("session launch preparation unavailable");
    const preview = await sessionApi.prepareLaunch({
      command: command(console, "prepare_fresh_launch_01", 2),
      projectId: console.projectId,
      projectSessionId,
      expectedSessionGeneration: 1,
      launchPacketRef,
    });

    expect(preview).toMatchObject({
      intent: {
        kind: "project-session-launch",
        projectId: console.projectId,
        projectSessionId,
        expectedSessionGeneration: 1,
        launchPacketRef,
      },
      consequenceClass: "consequential",
    });

    const replayed = await sessionApi.prepareLaunch({
      command: command(console, "prepare_fresh_launch_01", 2),
      projectId: console.projectId,
      projectSessionId,
      expectedSessionGeneration: 1,
      launchPacketRef,
    });
    expect(replayed).toEqual(preview);

    const operatorActions = console.client.console?.actions;
    if (operatorActions === undefined) throw new Error("operator action API unavailable");
    await operatorActions.commit({
      command: command(console, "commit_fresh_launch_01", 2),
      projectId: console.projectId,
      previewId: preview.previewId,
      expectedPreviewRevision: preview.previewRevision,
      previewDigest: preview.previewDigest,
      expectedIntentDigest: preview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_fresh_launch_01" },
    });
    await writeFile(peerTriggerPath, "create-peer\n", { mode: 0o600 });

    let peerReady = false;
    for (let attempt = 0; attempt < 100; attempt += 1) {
      peerReady = await readFile(peerReadyPath, "utf8").then(
        (value) => value === "ready\n",
        () => false,
      );
      if (peerReady) break;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    expect(peerReady).toBe(true);

    const database = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
    const state = database.prepare(`
      SELECT s.revision AS session_revision, s.generation AS session_generation,
             r.revision AS run_revision, r.chair_generation, r.chair_lease_id
        FROM project_sessions s JOIN runs r ON r.project_session_id=s.project_session_id
       WHERE s.project_session_id=? AND r.run_id=?
    `).get(projectSessionId, "run_fresh_launch_01") as {
      session_revision: number;
      session_generation: number;
      run_revision: number;
      chair_generation: number;
      chair_lease_id: string;
    };
    expect(database.prepare(`
      SELECT dimension.used, dimension.reserved, dimension.usage_unknown
        FROM resource_dimensions dimension
        JOIN resource_scopes scope ON scope.scope_id=dimension.scope_id
       WHERE scope.project_id=? AND scope.scope_kind='project'
         AND dimension.unit_key='provider_calls'
    `).get(console.projectId)).toEqual({ used: 1, reserved: 0, usage_unknown: 0 });
    expect(database.prepare(`
      SELECT dimension.used, dimension.reserved, dimension.usage_unknown
        FROM resource_dimensions dimension
        JOIN resource_scopes scope ON scope.scope_id=dimension.scope_id
       WHERE scope.project_id=? AND scope.scope_kind='project'
         AND dimension.unit_key='concurrent_turns'
    `).get(console.projectId)).toEqual({ used: 0, reserved: 0, usage_unknown: 0 });
    database.close();
    const expiresAt = new Date(Date.now() + 24 * 60 * 60 * 1_000).toISOString();
    const provisioned = await provisionMcpSeats([
      "--project", projectRoot,
      "--project-session-id", projectSessionId,
      "--session-revision", String(state.session_revision),
      "--session-generation", String(state.session_generation),
      "--run-id", "run_fresh_launch_01",
      "--run-revision", String(state.run_revision),
      "--chair-seat", "codex",
      "--chair-agent-id", "codex_fresh_chair_01",
      "--chair-generation", String(state.chair_generation),
      "--chair-lease-id", state.chair_lease_id,
      "--seat-bindings", "claude=claude_fresh_peer_01@1,codex=codex_fresh_chair_01@1",
      "--expires-at", expiresAt,
    ], paths);
    expect(provisioned.seats).toHaveLength(2);

    for (const seat of ["codex", "claude"] as const) {
      const transport = new StdioClientTransport({
        command: process.execPath,
        args: ["--import", "tsx", mcpMain],
        cwd: packageRoot,
        env: {
          AGENT_FABRIC_SOCKET_PATH: paths.socketPath,
          AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
          AGENT_FABRIC_PROJECT_PATH: projectRoot,
          AGENT_FABRIC_SEAT: seat,
          AGENT_FABRIC_CLIENT_LABEL: `fresh-${seat}`,
          PATH: process.env.PATH ?? "/usr/bin:/bin",
          TMPDIR: process.env.TMPDIR ?? "/tmp",
          ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
        },
      });
      const client = new Client({ name: `fresh-${seat}`, version: "0.1.0" });
      try {
        await client.connect(transport);
        const tools = await client.listTools();
        expect(tools.tools.length).toBeGreaterThan(0);
      } finally {
        await client.close();
      }
    }
  });
});
