import { mkdtemp, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer, type Socket } from "node:net";
import { createInterface } from "node:readline";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";

import { afterEach, describe, expect, it, vi } from "vitest";
import { parse } from "yaml";
import {
  FABRIC_OPERATIONS,
  PROTOCOL_LIMITS,
} from "@local/agent-fabric-protocol";

import {
  claudeProviderOptions,
  claudeReadOnlyOptions,
  createClaudeChairMcpBridge,
  createClaudeAgentSdkAdapter,
  InstalledClaudeAgentSdkBoundary,
  type ClaudeAgentSdkBoundary,
} from "../../src/adapters/providers/claude-agent-sdk.ts";
import {
  codexAppServerCommand,
  codexCompletedTurnResult,
  codexThreadConfiguration,
  createCodexAppServerAdapter,
  InstalledCodexAppServerBoundary,
  type CodexAppServerBoundary,
} from "../../src/adapters/providers/codex-app-server.ts";
import { createChairLaunchFabricBridge } from "../../src/adapters/providers/chair-launch-continuity.ts";
import { AgentSessionFabricBridge } from "../../src/adapters/providers/agent-session-continuity.ts";
import type { ProviderSessionProtocolTransport } from "../../src/adapters/providers/provider-session-fabric-surface.ts";
import { SqliteAdapterActionJournal } from "../../src/adapters/providers/journal.ts";
import * as providerTypes from "../../src/adapters/providers/types.ts";
import {
  chairLaunchAttestationDigest,
  chairLaunchChallengeDigest,
  ProviderAdapterError,
} from "../../src/adapters/providers/types.ts";

const ATTESTATION_CHALLENGE = "ab".repeat(32);
const ATTESTATION_CHALLENGE_DIGEST = chairLaunchChallengeDigest(ATTESTATION_CHALLENGE);
const EXPECTED_CHAIR_PRINCIPAL = {
  agentId: "chair",
  projectSessionId: "session-1",
  runId: "run-1",
  principalGeneration: 1,
} as const;

const temporaryDirectories: string[] = [];
const temporaryClosures: Array<() => Promise<void>> = [];

function hydrateWorkspaceRoot(value: unknown, workspaceRoot: string): unknown {
  if (typeof value === "string") return value.replaceAll("$WORKSPACE_ROOT", workspaceRoot);
  if (Array.isArray(value)) return value.map((entry) => hydrateWorkspaceRoot(entry, workspaceRoot));
  if (typeof value === "object" && value !== null) {
    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, hydrateWorkspaceRoot(entry, workspaceRoot)]),
    );
  }
  return value;
}

function normalizeFunctions(value: unknown): unknown {
  if (typeof value === "function") return "[function]";
  if (Array.isArray(value)) return value.map(normalizeFunctions);
  if (typeof value === "object" && value !== null) {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, normalizeFunctions(entry)]));
  }
  return value;
}

async function providerPermissionGolden(name: "admitted" | "claude" | "codex"): Promise<Record<string, unknown>> {
  const value: unknown = JSON.parse(await readFile(
    new URL(`../fixtures/provider-permissions/review-readonly.${name}.json`, import.meta.url),
    "utf8",
  ));
  return hydrateWorkspaceRoot(value, "/workspace/project") as Record<string, unknown>;
}

function providerSessionProtocolTransport(
  call: ProviderSessionProtocolTransport["call"],
  close: ProviderSessionProtocolTransport["close"],
  principal: ProviderSessionProtocolTransport["principal"] = {
    kind: "agent",
    ...EXPECTED_CHAIR_PRINCIPAL,
  } as ProviderSessionProtocolTransport["principal"],
): ProviderSessionProtocolTransport {
  return {
    features: ["fabric-core.v1", "launch-attestation.v1"],
    principal,
    allowedOperations: new Set([FABRIC_OPERATIONS.getMailboxState]),
    call,
    close,
  };
}

async function journal(): Promise<SqliteAdapterActionJournal> {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-adapter-"));
  temporaryDirectories.push(directory);
  return new SqliteAdapterActionJournal(join(directory, "actions.sqlite3"));
}

function claudeZeroTurnErrorResult(sessionId: string): Record<string, unknown> {
  return {
    type: "result",
    subtype: "error_during_execution",
    session_id: sessionId,
    uuid: `${sessionId}-result`,
    duration_ms: 0,
    duration_api_ms: 0,
    is_error: true,
    num_turns: 0,
    stop_reason: null,
    total_cost_usd: 0,
    usage: { input_tokens: 0, output_tokens: 0 },
    modelUsage: {},
    permission_denials: [],
    errors: ["You've hit your session limit · resets 1:20am (Australia/Brisbane)"],
  };
}

async function fabricSocketFixture(principal: {
  agentId: string;
  projectSessionId: string;
  runId: string;
  principalGeneration: number;
} = EXPECTED_CHAIR_PRINCIPAL): Promise<{
  socketPath: string;
  rpcCall: ReturnType<typeof vi.fn>;
  drop(): Promise<void>;
}> {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-chair-socket-"));
  temporaryDirectories.push(directory);
  const socketPath = join(directory, "fabric.sock");
  const sockets = new Set<Socket>();
  const rpcCall = vi.fn();
  const server = createServer((socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    const lines = createInterface({ input: socket, crlfDelay: Infinity });
    lines.on("line", (line) => {
      const request = JSON.parse(line) as {
        id: string;
        operation: string;
        input?: { authentication?: { clientNonce?: string } };
      };
      if (request.operation === "initialize") {
        socket.write(`${JSON.stringify({
          id: request.id,
          operation: request.operation,
          ok: true,
          result: {
            protocolVersion: 1,
            daemonVersion: "0.1.0",
            daemonInstanceGeneration: 1,
            principal: {
              kind: "agent",
              ...principal,
            },
            clientNonce: request.input?.authentication?.clientNonce,
            connectionNonce: "provider-test-connection",
            features: ["fabric-core.v1", "launch-attestation.v1"],
            allowedOperations: [FABRIC_OPERATIONS.getMailboxState],
            limits: PROTOCOL_LIMITS,
          },
        })}\n`);
        return;
      }
      rpcCall(request.operation);
      socket.write(`${JSON.stringify({
        id: request.id,
        operation: request.operation,
        ok: true,
        result: { contiguousWatermark: 0, acknowledgedAboveWatermark: [] },
      })}\n`);
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  const drop = async (): Promise<void> => {
    await Promise.all([...sockets].map(async (socket) => await new Promise<void>((resolve) => {
      socket.once("close", resolve);
      socket.destroy();
    })));
  };
  temporaryClosures.push(async () => {
    await drop();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });
  return { socketPath, rpcCall, drop };
}

function provedChairLaunch(
  resumeReference: string,
  providerContractDigest: string,
  providerAdapterId: string,
  providerActionId: string,
  challengeDigest = ATTESTATION_CHALLENGE_DIGEST,
  providerInvocationRef = "provider-tool-call-1",
  providerTurnRef = "provider-turn-1",
  providerSessionGeneration = 1,
) {
  const unsigned = {
    schemaVersion: 1 as const,
    kind: "provider-session-fabric-attestation" as const,
    method: "provider-session-random-challenge-v1" as const,
    bridgeContract: "agent-fabric-session-bridge-v1" as const,
    providerAdapterId,
    providerActionId,
    providerContractDigest,
    providerSessionRef: resumeReference,
    providerSessionGeneration,
    providerTurnRef,
    challengeDigest,
    providerInvocationRef,
  };
  return {
    resumeReference,
    providerSessionGeneration,
    fabricContinuity: {
      ...unsigned,
      attestationDigest: chairLaunchAttestationDigest(unsigned),
    },
  } as const;
}

async function expectJournalFilesNotToContain(path: string, canary: string): Promise<void> {
  for (const candidate of [path, `${path}-wal`, `${path}-shm`]) {
    const bytes = await readFile(candidate).catch((error: unknown) => {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") return Buffer.alloc(0);
      throw error;
    });
    expect(bytes.includes(Buffer.from(canary, "utf8")), candidate).toBe(false);
  }
}

afterEach(async () => {
  await Promise.allSettled(temporaryClosures.splice(0).map((close) => close()));
  await Promise.all(temporaryDirectories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

describe("private chair launch environment", () => {
  it("takes the complete handoff once and removes both values from the adapter process environment", () => {
    const takeChairLaunchHandoff: unknown = Reflect.get(providerTypes, "takeChairLaunchHandoff");
    expect(takeChairLaunchHandoff).toBeTypeOf("function");
    const environment: NodeJS.ProcessEnv = {
      AGENT_FABRIC_CAPABILITY: "environment-capability-canary",
      AGENT_FABRIC_SOCKET_PATH: "/private/environment-fabric.sock",
      AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
      AGENT_FABRIC_EXPECTED_AGENT_ID: EXPECTED_CHAIR_PRINCIPAL.agentId,
      AGENT_FABRIC_EXPECTED_PROJECT_SESSION_ID: EXPECTED_CHAIR_PRINCIPAL.projectSessionId,
      AGENT_FABRIC_EXPECTED_RUN_ID: EXPECTED_CHAIR_PRINCIPAL.runId,
      AGENT_FABRIC_EXPECTED_PRINCIPAL_GENERATION: String(EXPECTED_CHAIR_PRINCIPAL.principalGeneration),
      KEEP: "retained",
    };

    expect(Reflect.apply(
      takeChairLaunchHandoff as (...arguments_: unknown[]) => unknown,
      undefined,
      [environment],
    )).toEqual({
      capability: "environment-capability-canary",
      socketPath: "/private/environment-fabric.sock",
      attestationChallenge: ATTESTATION_CHALLENGE,
      expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
    });
    expect(environment).toEqual({ KEEP: "retained" });
    expect(Reflect.apply(
      takeChairLaunchHandoff as (...arguments_: unknown[]) => unknown,
      undefined,
      [environment],
    )).toBeUndefined();
  });

  it("rejects and removes a handoff whose socket path is not absolute", () => {
    const takeChairLaunchHandoff: unknown = Reflect.get(providerTypes, "takeChairLaunchHandoff");
    const environment: NodeJS.ProcessEnv = {
      AGENT_FABRIC_CAPABILITY: "relative-socket-capability-canary",
      AGENT_FABRIC_SOCKET_PATH: "relative/fabric.sock",
      AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
      AGENT_FABRIC_EXPECTED_AGENT_ID: EXPECTED_CHAIR_PRINCIPAL.agentId,
      AGENT_FABRIC_EXPECTED_PROJECT_SESSION_ID: EXPECTED_CHAIR_PRINCIPAL.projectSessionId,
      AGENT_FABRIC_EXPECTED_RUN_ID: EXPECTED_CHAIR_PRINCIPAL.runId,
      AGENT_FABRIC_EXPECTED_PRINCIPAL_GENERATION: String(EXPECTED_CHAIR_PRINCIPAL.principalGeneration),
    };

    expect(() => Reflect.apply(
      takeChairLaunchHandoff as (...arguments_: unknown[]) => unknown,
      undefined,
      [environment],
    )).toThrowError("chair launch private environment must contain a capability, 32-byte challenge, exact agent principal and absolute socket path");
    expect(environment).toEqual({});
  });

});

describe("installed primary chair principal binding", () => {
  const variants = [
    ["agent", { agentId: "wrong-chair" }],
    ["project session", { projectSessionId: "wrong-session" }],
    ["run", { runId: "wrong-run" }],
    ["principal generation", { principalGeneration: 9 }],
  ] as const;

  it.each(["Claude", "Codex"] as const)("rejects every wrong authenticated launch principal in the %s adapter before provider I/O", async (provider) => {
    for (const [_label, principalChange] of variants) {
      const providerIo = vi.fn(() => {
        throw new Error("provider I/O must not run for a substituted Fabric principal");
      });
      const call = vi.fn(async () => ({ contiguousWatermark: 0, acknowledgedAboveWatermark: [] }));
      const close = vi.fn(async () => undefined);
      const bridgeFactory = async (input: Parameters<typeof createChairLaunchFabricBridge>[0]) => (
        await createChairLaunchFabricBridge(input, {
          connect: vi.fn(async () => providerSessionProtocolTransport(call, close, {
            kind: "agent",
            ...EXPECTED_CHAIR_PRINCIPAL,
            ...principalChange,
          } as ProviderSessionProtocolTransport["principal"])),
        })
      );
      const boundary = provider === "Claude"
        ? new InstalledClaudeAgentSdkBoundary({ query: providerIo as never, bridgeFactory })
        : new InstalledCodexAppServerBoundary(providerIo as never, bridgeFactory);

      await expect(boundary.launchChair({
        actionId: `${provider.toLowerCase()}-wrong-principal`,
        providerAdapterId: provider === "Claude" ? "claude-agent-sdk" : "codex-app-server",
        providerContractDigest: `sha256:${"b".repeat(64)}`,
        challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
        payload: {
          cwd: "/workspace/project",
          modelFamily: provider === "Claude" ? "anthropic" : "openai",
          model: provider === "Claude" ? "claude-test" : "gpt-test",
          prompt: "reviewed work",
        },
        environment: {
          AGENT_FABRIC_CAPABILITY: "wrong-principal-capability-canary",
          AGENT_FABRIC_SOCKET_PATH: "/private/wrong-principal.sock",
          AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
        },
      } as never)).rejects.toMatchObject({ code: "CHAIR_PRINCIPAL_MISMATCH" });
      expect(providerIo).not.toHaveBeenCalled();
      expect(call).not.toHaveBeenCalled();
      expect(close).toHaveBeenCalledOnce();
    }
  });
});

function claudeBoundary(): ClaudeAgentSdkBoundary {
  return {
    status: vi.fn(async () => ({ healthy: true })),
    spawn: vi.fn(async () => ({ resumeReference: "claude-session-1" })),
    attach: vi.fn(async ({ resumeReference }) => ({ resumeReference })),
    sendTurn: vi.fn(async () => ({ resumeReference: "claude-session-1", result: "done" })),
    interrupt: vi.fn(async () => ({ interrupted: true })),
    release: vi.fn(async () => ({ released: true, deleted: false })),
    hasLiveChairSession: vi.fn(() => true),
  };
}

function codexBoundary(): CodexAppServerBoundary {
  return {
    status: vi.fn(async () => ({ healthy: true })),
    spawn: vi.fn(async () => ({ resumeReference: "codex-thread-1" })),
    attach: vi.fn(async ({ resumeReference }) => ({ resumeReference })),
    sendTurn: vi.fn(async () => ({ resumeReference: "codex-thread-1", turnId: "turn-1" })),
    steer: vi.fn(async () => ({ turnId: "turn-1", steered: true })),
    interrupt: vi.fn(async () => ({ interrupted: true })),
    compact: vi.fn(async () => ({ compacted: true, resumeReference: "codex-thread-1" })),
    release: vi.fn(async () => ({ released: true, deleted: false })),
    hasLiveChairSession: vi.fn(() => true),
  };
}

describe("primary provider chair recovery adapter", () => {
  it.each(["Claude", "Codex"] as const)(
    "journals one fresh %s rebind, retains its exact live bridge and never replays provider I/O",
    async (provider) => {
      const actionJournal = await journal();
      const boundary = provider === "Claude" ? claudeBoundary() : codexBoundary();
      const recoveryPrincipal = { ...EXPECTED_CHAIR_PRINCIPAL, principalGeneration: 2 } as const;
      const recoverChair = vi.fn(async (input: {
        actionId: string;
        providerAdapterId: string;
        providerContractDigest: string;
        nextProviderSessionGeneration: number;
      }) => provedChairLaunch(
        `${provider.toLowerCase()}-retained-chair`,
        input.providerContractDigest,
        input.providerAdapterId,
        input.actionId,
        ATTESTATION_CHALLENGE_DIGEST,
        `${provider.toLowerCase()}-recovery-tool-call`,
        `${provider.toLowerCase()}-recovery-turn`,
        input.nextProviderSessionGeneration,
      ));
      Reflect.set(boundary, "recoverChair", recoverChair);
      const capability = `${provider.toLowerCase()}-recovery-capability-canary`;
      const socketPath = `/private/${provider.toLowerCase()}-recovery.sock`;
      const adapter = provider === "Claude"
        ? createClaudeAgentSdkAdapter({
            boundary: boundary as ClaudeAgentSdkBoundary,
            journal: actionJournal,
            chairLaunchHandoff: {
              capability,
              socketPath,
              attestationChallenge: ATTESTATION_CHALLENGE,
              expectedPrincipal: recoveryPrincipal,
            },
          })
        : createCodexAppServerAdapter({
            boundary: boundary as CodexAppServerBoundary,
            journal: actionJournal,
            chairLaunchHandoff: {
              capability,
              socketPath,
              attestationChallenge: ATTESTATION_CHALLENGE,
              expectedPrincipal: recoveryPrincipal,
            },
          });
      const request = {
        schemaVersion: 1,
        recoveryId: `${provider.toLowerCase()}-recovery-1`,
        lossId: `${provider.toLowerCase()}-loss-1`,
        actionId: `${provider.toLowerCase()}-recover-chair-1`,
        providerContractDigest: `sha256:${"9".repeat(64)}`,
        resumeReference: `${provider.toLowerCase()}-retained-chair`,
        expectedProviderSessionGeneration: 1,
        nextProviderSessionGeneration: 2,
        bridgeGeneration: 2,
        payload: { cwd: "/workspace/project", prompt: "recover reviewed work" },
      };

      await expect(adapter.request("recover_chair", request)).resolves.toMatchObject({
        resumeReference: request.resumeReference,
        providerSessionGeneration: 2,
      });
      await expect(adapter.request("recover_chair", request)).resolves.toMatchObject({
        resumeReference: request.resumeReference,
        providerSessionGeneration: 2,
      });
      expect(recoverChair).toHaveBeenCalledOnce();
      await expect(adapter.request("lookup_action", { actionId: request.actionId })).resolves.toMatchObject({
        operation: "recover_chair",
        status: "terminal",
        executionCount: 1,
        effectCount: 1,
      });
      const persisted = await adapter.request("lookup_action", { actionId: request.actionId });
      expect(JSON.stringify(persisted)).not.toContain(capability);
      expect(JSON.stringify(persisted)).not.toContain(socketPath);
      actionJournal.close();
    },
  );

  it("redacts fresh recovery handoff material from provider failures", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const capability = "recovery-failure-capability-canary";
    const socketPath = "/private/recovery-failure-socket-canary.sock";
    Reflect.set(boundary, "recoverChair", vi.fn(async () => {
      throw new Error(`provider echoed ${capability} at ${socketPath}`);
    }));
    const adapter = createClaudeAgentSdkAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath,
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: { ...EXPECTED_CHAIR_PRINCIPAL, principalGeneration: 2 },
      },
    });
    const actionId = "claude-recovery-redacted-failure";
    const error = await adapter.request("recover_chair", {
      schemaVersion: 1,
      recoveryId: "recovery-redacted-1",
      lossId: "loss-redacted-1",
      actionId,
      providerContractDigest: `sha256:${"8".repeat(64)}`,
      resumeReference: "claude-recovery-redacted-session",
      expectedProviderSessionGeneration: 1,
      nextProviderSessionGeneration: 2,
      bridgeGeneration: 2,
      payload: { cwd: "/workspace/project", prompt: "recover" },
    }).catch((cause: unknown) => cause);
    expect(error).toMatchObject({
      code: "CHAIR_RECOVERY_FAILED",
      message: "chair recovery provider handoff failed",
      details: { actionId },
    });
    expect(JSON.stringify(error)).not.toContain(capability);
    expect(JSON.stringify(error)).not.toContain(socketPath);
    const persisted = await adapter.request("lookup_action", { actionId });
    expect(persisted).toMatchObject({ status: "ambiguous", effectCount: 0 });
    expect(JSON.stringify(persisted)).not.toContain(capability);
    expect(JSON.stringify(persisted)).not.toContain(socketPath);
    actionJournal.close();
  });
});

describe("Claude Agent SDK fabric adapter", () => {
  it("translates the fabric boundary into explicit SDK plan and path-bounded read-only isolation", () => {
    expect(claudeReadOnlyOptions({
      cwd: "/workspace/src",
      model: "claude-sonnet-4-5",
      effort: "max",
      readOnlyRoot: "/workspace/src",
      allowedTools: ["Bash"],
      disallowedTools: [],
      sandbox: "read-only",
      approvalPolicy: "never",
    }, undefined, "/trusted/claude")).toMatchObject({
      cwd: "/workspace/src",
      model: "claude-sonnet-4-5",
      effort: "max",
      tools: ["Read", "Glob", "Grep"],
      permissionMode: "plan",
      settingSources: [],
      skills: [],
      plugins: [],
      pathToClaudeCodeExecutable: "/trusted/claude",
    });
  });

  it("renders only a compiled write-offline projection as a fail-closed SDK sandbox", () => {
    expect(claudeProviderOptions({
      cwd: "/workspace/src",
      readOnlyRoot: "/workspace/src",
      writeRoot: "/workspace/src",
      executionProfile: "workspace-write-offline",
      networkAccess: "none",
      sandbox: "workspace-write",
      approvalPolicy: "never",
      allowedTools: ["Read", "Glob", "Grep", "Write", "Edit", "Bash"],
    })).toMatchObject({
      cwd: "/workspace/src",
      tools: ["Read", "Glob", "Grep", "Write", "Edit", "Bash"],
      permissionMode: "acceptEdits",
      sandbox: {
        enabled: true,
        failIfUnavailable: true,
        autoAllowBashIfSandboxed: true,
        allowUnsandboxedCommands: false,
        filesystem: {
          allowWrite: ["/workspace/src"],
          denyWrite: ["/workspace/src/.git"],
          allowRead: ["/workspace/src"],
          allowManagedReadPathsOnly: true,
        },
        network: {
          allowedDomains: [],
          allowManagedDomainsOnly: true,
          allowUnixSockets: [],
          allowAllUnixSockets: false,
          allowLocalBinding: false,
        },
      },
      settingSources: [],
      skills: [],
      plugins: [],
    });
  });

  it("passes only the PATH, TMPDIR and HOME allowlist to a write-offline SDK child", async () => {
    const sentinel = "must-not-cross-provider-boundary";
    const previous = process.env.AGENT_FABRIC_TEST_SECRET;
    process.env.AGENT_FABRIC_TEST_SECRET = sentinel;
    const query = vi.fn((input: unknown) => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        void input;
        yield {
          type: "result",
          subtype: "success",
          session_id: "claude-write-offline-environment",
          result: "bounded answer",
          usage: { input_tokens: 1, output_tokens: 1 },
          num_turns: 1,
          total_cost_usd: 0,
        };
      },
    }));
    const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });

    try {
      await boundary.spawn({
        prompt: "run the admitted offline task",
        cwd: "/workspace/src",
        readOnlyRoot: "/workspace/src",
        writeRoot: "/workspace/src",
        executionProfile: "workspace-write-offline",
        networkAccess: "none",
        sandbox: "workspace-write",
        approvalPolicy: "never",
        allowedTools: ["Read", "Glob", "Grep", "Write", "Edit", "Bash"],
      });

      const call = query.mock.calls[0]?.[0] as { options: { env: Record<string, string> } } | undefined;
      const environment = call?.options.env;
      expect(environment).toEqual({
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        TMPDIR: process.env.TMPDIR ?? "/tmp",
        ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
      });
      expect(environment).not.toHaveProperty("AGENT_FABRIC_TEST_SECRET");
      expect(JSON.stringify(environment)).not.toContain(sentinel);
    } finally {
      if (previous === undefined) delete process.env.AGENT_FABRIC_TEST_SECRET;
      else process.env.AGENT_FABRIC_TEST_SECRET = previous;
    }
  });

  it("rejects an unrecognised Claude effort before provider work", () => {
    expect(() => claudeReadOnlyOptions({
      cwd: "/workspace/src",
      model: "opus",
      effort: "ultra",
    })).toThrowError(/effort must be one of low, medium, high, xhigh, max/u);
  });

  it("passes exact model and effort controls to fresh and resumed Claude turns", async () => {
    let call = 0;
    const query = vi.fn(() => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        call += 1;
        yield {
          type: "result",
          subtype: "success",
          session_id: `claude-controls-session-${call}`,
          result: "bounded answer",
          usage: { input_tokens: 1, output_tokens: 1 },
          num_turns: 1,
          total_cost_usd: 0,
        };
      },
    }));
    const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });

    await boundary.spawn({ prompt: "fresh", model: "claude-sonnet-current", effort: "medium" });
    await boundary.sendTurn({
      resumeReference: "claude-controls-session-1",
      prompt: "resume",
      model: "claude-opus-current",
      effort: "max",
    });

    expect(query).toHaveBeenNthCalledWith(1, expect.objectContaining({
      options: expect.objectContaining({ model: "claude-sonnet-current", effort: "medium" }),
    }));
    expect(query).toHaveBeenNthCalledWith(2, expect.objectContaining({
      options: expect.objectContaining({
        resume: "claude-controls-session-1",
        model: "claude-opus-current",
        effort: "max",
      }),
    }));
  });

  it("normalises billed Claude usage into conservative integer budget units", async () => {
    const query = vi.fn((input: unknown) => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        yield {
          type: "result",
          subtype: "success",
          session_id: "claude-usage-session",
          result: "bounded answer",
          usage: {
            input_tokens: 7,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
            output_tokens: 5,
          },
          num_turns: 1,
          total_cost_usd: 0.0000001,
        };
      },
      input,
    }));
    const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });

    await expect(boundary.spawn({ prompt: "review", maxTurns: 1 })).resolves.toEqual({
      resumeReference: "claude-usage-session",
      result: "bounded answer",
      resourceUsage: {
        "cost:USD": 1,
        "input_tokens:anthropic": 12,
        "output_tokens:anthropic": 5,
        turns: 1,
      },
    });
  });

  it("consumes the lifecycle checkpoint prompt in the resumed Claude turn", async () => {
    const query = vi.fn((input: unknown) => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        yield {
          type: "result",
          subtype: "success",
          session_id: "claude-rotated-session",
          result: "checkpoint consumed",
          usage: { input_tokens: 1, output_tokens: 1 },
          num_turns: 1,
          total_cost_usd: 0,
        };
      },
      input,
    }));
    const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });

    await expect(boundary.spawn({
      priorResumeReference: "claude-prior-session",
      prompt: "verified lifecycle checkpoint handoff",
      generation: 2,
    })).resolves.toMatchObject({
      resumeReference: "claude-rotated-session",
      result: "checkpoint consumed",
    });
    expect(query).toHaveBeenCalledWith(expect.objectContaining({
      prompt: "verified lifecycle checkpoint handoff",
      options: expect.objectContaining({ resume: "claude-prior-session" }),
    }));
  });

  it.each([undefined, 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])(
    "rejects invalid Claude terminal num_turns evidence %s",
    async (numTurns) => {
      const query = vi.fn(() => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield {
            type: "result",
            subtype: "success",
            session_id: "claude-invalid-turns-session",
            result: "bounded answer",
            usage: { input_tokens: 1, output_tokens: 1 },
            ...(numTurns === undefined ? {} : { num_turns: numTurns }),
            total_cost_usd: 0,
          };
        },
      }));
      const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });

      await expect(boundary.spawn({ prompt: "review", maxTurns: 2 })).rejects.toMatchObject({
        code: "PROVIDER_RESPONSE_INVALID",
      });
    },
  );

  it("matches the exact current Claude review-readonly golden without SDK sandbox or a network fence", async () => {
    const admitted = await providerPermissionGolden("admitted");
    const options = claudeReadOnlyOptions({
      ...admitted,
      model: "claude-opus-4-6",
      modelFamily: "anthropic",
      maxTurns: 3,
      effort: "high",
      tools: ["Bash"],
      allowedTools: ["Bash"],
    });
    expect(normalizeFunctions(options)).toStrictEqual(await providerPermissionGolden("claude"));
    expect(options).not.toHaveProperty("sandbox");
    expect(options).not.toHaveProperty("network");
  });

  it("allows only path-bounded read tools for a delegated review root", async () => {
    const root = await mkdtemp(join(tmpdir(), "claude-review-root-"));
    const outside = await mkdtemp(join(tmpdir(), "claude-review-outside-"));
    temporaryDirectories.push(root, outside);
    const insideFile = join(root, "inside.txt");
    const outsideFile = join(outside, "outside.txt");
    const escapingLink = join(root, "escaping-link.txt");
    await Promise.all([
      writeFile(insideFile, "inside\n"),
      writeFile(outsideFile, "outside\n"),
    ]);
    await symlink(outsideFile, escapingLink);
    const options = claudeReadOnlyOptions({ cwd: root, readOnlyRoot: root, model: "opus" });
    expect(options.tools).toEqual(["Read", "Glob", "Grep"]);
    expect(options.canUseTool).toBeTypeOf("function");
    const permissionContext = {
      signal: AbortSignal.timeout(5_000),
      toolUseID: "tool-use-1",
      requestId: "request-1",
    };
    await expect(options.canUseTool?.("Read", { file_path: insideFile }, permissionContext)).resolves.toMatchObject({
      behavior: "allow",
    });
    await expect(options.canUseTool?.("Read", { file_path: outsideFile }, permissionContext)).resolves.toMatchObject({
      behavior: "deny",
    });
    await expect(options.canUseTool?.("Read", { file_path: escapingLink }, permissionContext)).resolves.toMatchObject({
      behavior: "deny",
    });
    await expect(options.canUseTool?.("Glob", { pattern: "../**" }, permissionContext)).resolves.toMatchObject({
      behavior: "deny",
    });
    await expect(options.canUseTool?.("Bash", { command: "pwd" }, permissionContext)).resolves.toMatchObject({
      behavior: "deny",
    });
  });

  it("journals a provider effect before returning it and replays the terminal result without a second effect", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const adapter = createClaudeAgentSdkAdapter({ boundary, journal: actionJournal });
    const params = {
      actionId: "claude-spawn-1",
      payload: { prompt: "bounded task", cwd: ".", model: "claude-test" },
    };

    const first = await adapter.request("spawn", params);
    const replay = await adapter.request("spawn", params);

    expect(first).toEqual({ resumeReference: "claude-session-1" });
    expect(replay).toEqual(first);
    expect(boundary.spawn).toHaveBeenCalledTimes(1);
    expect(await adapter.request("lookup_action", { actionId: "claude-spawn-1" })).toMatchObject({
      actionId: "claude-spawn-1",
      operation: "spawn",
      status: "terminal",
      history: ["prepared", "dispatched", "accepted", "terminal"],
      executionCount: 1,
      effectCount: 1,
    });
    actionJournal.close();
  });

  it("classifies the exact Claude subscription-limit no-answer result as retryable no-effect", async () => {
    const resetAt = 1_784_229_600_000;
    const query = vi.fn(() => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        yield {
          type: "assistant",
          session_id: "claude-limited-session",
          uuid: "claude-rate-limit-assistant",
          parent_tool_use_id: null,
          error: "rate_limit",
          message: {
            role: "assistant",
            content: [{ type: "text", text: "You've hit your session limit · resets 1:20am (Australia/Brisbane)" }],
            usage: {
              input_tokens: 0,
              output_tokens: 0,
              cache_creation_input_tokens: 0,
              cache_read_input_tokens: 0,
              server_tool_use: { web_search_requests: 0, web_fetch_requests: 0 },
            },
          },
        };
        yield {
          type: "rate_limit_event",
          session_id: "claude-limited-session",
          uuid: "claude-rate-limit-event",
          rate_limit_info: {
            status: "rejected",
            resetsAt: resetAt,
            rateLimitType: "five_hour",
          },
        };
        yield claudeZeroTurnErrorResult("claude-limited-session");
        throw new Error(
          "Claude Code returned an error result: You've hit your session limit · resets 1:20am (Australia/Brisbane)",
        );
      },
    }));
    const actionJournal = await journal();
    const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });
    const adapter = createClaudeAgentSdkAdapter({ boundary, journal: actionJournal });
    const params = { actionId: "claude-subscription-limit", payload: { prompt: "review" } };

    await expect(adapter.request("spawn", params)).rejects.toMatchObject({
      code: "PROVIDER_SUBSCRIPTION_LIMIT",
      details: {
        retryable: true,
        noEffect: true,
        resetsAt: resetAt,
        rateLimitType: "five_hour",
      },
    });
    await expect(adapter.request("spawn", params)).rejects.toMatchObject({
      code: "PROVIDER_SUBSCRIPTION_LIMIT",
    });
    expect(query).toHaveBeenCalledOnce();
    await expect(adapter.request("lookup_action", { actionId: params.actionId })).resolves.toMatchObject({
      status: "terminal",
      history: ["prepared", "dispatched", "terminal"],
      executionCount: 1,
      effectCount: 0,
      idempotencyProven: true,
      result: {
        status: "no-effect",
        retryable: true,
        errorCode: "PROVIDER_SUBSCRIPTION_LIMIT",
        resetsAt: resetAt,
        rateLimitType: "five_hour",
      },
    });
    actionJournal.close();
  });

  it.each([
    {
      scenario: "partial answer",
      actionId: "claude-limit-partial-answer",
      query: () => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield {
            type: "assistant",
            session_id: "claude-partial-session",
            uuid: "claude-partial-answer",
            parent_tool_use_id: null,
            message: { content: [{ type: "text", text: "partial" }] },
          };
          yield {
            type: "rate_limit_event",
            session_id: "claude-partial-session",
            uuid: "claude-partial-limit",
            rate_limit_info: { status: "rejected", resetsAt: 1_784_229_600_000, rateLimitType: "five_hour" },
          };
          yield claudeZeroTurnErrorResult("claude-partial-session");
          throw new Error("Claude Code returned an error result after partial answer");
        },
      }),
    },
    {
      scenario: "timeout",
      actionId: "claude-limit-timeout",
      query: () => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          throw new Error("provider timeout");
          yield undefined;
        },
      }),
    },
    {
      scenario: "disconnect",
      actionId: "claude-limit-disconnect",
      query: () => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield {
            type: "rate_limit_event",
            session_id: "claude-disconnected-session",
            uuid: "claude-disconnected-limit",
            rate_limit_info: { status: "rejected", resetsAt: 1_784_229_600_000, rateLimitType: "five_hour" },
          };
          throw new Error("provider disconnected before terminal result");
        },
      }),
    },
    {
      scenario: "unproven error",
      actionId: "claude-limit-unproven",
      query: () => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield claudeZeroTurnErrorResult("claude-unproven-session");
          throw new Error("Claude Code returned an uncorroborated error result");
        },
      }),
    },
    {
      scenario: "rejected event without the matching rate-limit assistant",
      actionId: "claude-limit-missing-assistant",
      query: () => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield {
            type: "rate_limit_event",
            session_id: "claude-missing-assistant-session",
            uuid: "claude-missing-assistant-limit",
            rate_limit_info: { status: "rejected", resetsAt: 1_784_229_600_000, rateLimitType: "five_hour" },
          };
          yield claudeZeroTurnErrorResult("claude-missing-assistant-session");
          throw new Error("Claude Code returned an unrelated zero-turn error result");
        },
      }),
    },
    {
      scenario: "rate-limit evidence from a different session",
      actionId: "claude-limit-session-mismatch",
      query: () => ({
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield {
            type: "assistant",
            session_id: "claude-other-session",
            uuid: "claude-other-session-assistant",
            parent_tool_use_id: null,
            error: "rate_limit",
            message: {
              role: "assistant",
              content: [{ type: "text", text: "structured limit" }],
              usage: { input_tokens: 0, output_tokens: 0 },
            },
          };
          yield {
            type: "rate_limit_event",
            session_id: "claude-terminal-session",
            uuid: "claude-terminal-session-limit",
            rate_limit_info: { status: "rejected", resetsAt: 1_784_229_600_000, rateLimitType: "five_hour" },
          };
          yield claudeZeroTurnErrorResult("claude-terminal-session");
          throw new Error("Claude Code returned mismatched session evidence");
        },
      }),
    },
  ])("keeps a Claude $scenario ambiguous", async ({ actionId, query }) => {
    const actionJournal = await journal();
    const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never });
    const adapter = createClaudeAgentSdkAdapter({ boundary, journal: actionJournal });

    await expect(adapter.request("spawn", { actionId, payload: { prompt: "review" } }))
      .rejects.not.toMatchObject({ code: "PROVIDER_SUBSCRIPTION_LIMIT" });
    await expect(adapter.request("lookup_action", { actionId })).resolves.toMatchObject({
      status: "ambiguous",
      history: ["prepared", "dispatched", "ambiguous"],
      executionCount: 1,
      effectCount: 0,
      idempotencyProven: false,
    });
    actionJournal.close();
  });

  it("does not apply Claude subscription no-effect policy to another provider adapter", async () => {
    const actionJournal = await journal();
    const boundary = codexBoundary();
    vi.mocked(boundary.spawn).mockRejectedValueOnce(new ProviderAdapterError(
      "PROVIDER_SUBSCRIPTION_LIMIT",
      "foreign provider reused an open error code",
      { retryable: true, noEffect: true },
    ));
    const adapter = createCodexAppServerAdapter({ boundary, journal: actionJournal });
    const actionId = "codex-foreign-subscription-limit";

    await expect(adapter.request("spawn", { actionId, payload: { prompt: "review" } }))
      .rejects.toThrowError("foreign provider reused an open error code");
    await expect(adapter.request("lookup_action", { actionId })).resolves.toMatchObject({
      status: "ambiguous",
      history: ["prepared", "dispatched", "ambiguous"],
      effectCount: 0,
      idempotencyProven: false,
    });
    actionJournal.close();
  });

  it("allows one distinct fresh Claude action after a proven subscription-limit no-effect", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    vi.mocked(boundary.spawn)
      .mockRejectedValueOnce(new ProviderAdapterError(
        "PROVIDER_SUBSCRIPTION_LIMIT",
        "Claude subscription limit rejected the turn before any answer or provider effect",
        { retryable: true, noEffect: true, resetsAt: 1_784_229_600_000, rateLimitType: "five_hour" },
      ))
      .mockResolvedValueOnce({ resumeReference: "claude-after-reset", result: "clean" });
    const adapter = createClaudeAgentSdkAdapter({ boundary, journal: actionJournal });

    await expect(adapter.request("spawn", {
      actionId: "claude-before-reset",
      payload: { prompt: "review" },
    })).rejects.toMatchObject({ code: "PROVIDER_SUBSCRIPTION_LIMIT" });
    await expect(adapter.request("spawn", {
      actionId: "claude-after-reset",
      payload: { prompt: "review" },
    })).resolves.toEqual({ resumeReference: "claude-after-reset", result: "clean" });
    expect(boundary.spawn).toHaveBeenCalledTimes(2);
    await expect(adapter.request("lookup_action", { actionId: "claude-before-reset" })).resolves.toMatchObject({
      status: "terminal",
      effectCount: 0,
      result: { status: "no-effect" },
    });
    await expect(adapter.request("lookup_action", { actionId: "claude-after-reset" })).resolves.toMatchObject({
      status: "terminal",
      effectCount: 1,
    });
    actionJournal.close();
  });

  it("records a compiled write-offline attempt in the existing adapter action journal", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const adapter = createClaudeAgentSdkAdapter({ boundary, journal: actionJournal });
    const payload = {
      prompt: "Run the approved offline case.",
      cwd: "/workspace/src",
      readOnlyRoot: "/workspace/src",
      writeRoot: "/workspace/src",
      executionProfile: "workspace-write-offline",
      networkAccess: "none",
      sandbox: "workspace-write",
      approvalPolicy: "never",
      allowedTools: ["Read", "Glob", "Grep", "Write", "Edit", "Bash"],
    };

    await adapter.request("spawn", { actionId: "claude-write-offline-1", payload });

    expect(boundary.spawn).toHaveBeenCalledWith(payload);
    expect(await adapter.request("lookup_action", { actionId: "claude-write-offline-1" })).toMatchObject({
      actionId: "claude-write-offline-1",
      operation: "spawn",
      status: "terminal",
      history: ["prepared", "dispatched", "accepted", "terminal"],
      executionCount: 1,
      effectCount: 1,
    });
    actionJournal.close();
  });

  it("rejects an incomplete write-offline projection before provider dispatch or journal preparation", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const adapter = createClaudeAgentSdkAdapter({ boundary, journal: actionJournal });

    await expect(adapter.request("spawn", {
      actionId: "claude-write-offline-incomplete",
      payload: {
        cwd: "/workspace/src",
        executionProfile: "workspace-write-offline",
      },
    })).rejects.toMatchObject({ code: "INVALID_PARAMS" });
    expect(boundary.spawn).not.toHaveBeenCalled();
    await expect(adapter.request("lookup_action", {
      actionId: "claude-write-offline-incomplete",
    })).rejects.toMatchObject({ code: "ACTION_NOT_FOUND" });
    actionJournal.close();
  });

  it("replays a terminal action after the wrapper process journal is reopened", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-adapter-reopen-"));
    temporaryDirectories.push(directory);
    const path = join(directory, "actions.sqlite3");
    const firstJournal = new SqliteAdapterActionJournal(path);
    const firstBoundary = claudeBoundary();
    await createClaudeAgentSdkAdapter({ boundary: firstBoundary, journal: firstJournal }).request("spawn", {
      actionId: "claude-reopen-1",
      payload: { prompt: "persist me" },
    });
    firstJournal.close();

    const reopenedJournal = new SqliteAdapterActionJournal(path);
    const replacementBoundary = claudeBoundary();
    const replay = await createClaudeAgentSdkAdapter({ boundary: replacementBoundary, journal: reopenedJournal }).request(
      "spawn",
      { actionId: "claude-reopen-1", payload: { prompt: "persist me" } },
    );

    expect(replay).toEqual({ resumeReference: "claude-session-1" });
    expect(replacementBoundary.spawn).not.toHaveBeenCalled();
    reopenedJournal.close();
  });

  it("rejects changed payloads and unsupported interactive controls with typed errors", async () => {
    const actionJournal = await journal();
    const adapter = createClaudeAgentSdkAdapter({ boundary: claudeBoundary(), journal: actionJournal });
    await adapter.request("spawn", { actionId: "claude-spawn-2", payload: { prompt: "one" } });

    await expect(
      adapter.request("spawn", { actionId: "claude-spawn-2", payload: { prompt: "two" } }),
    ).rejects.toMatchObject({ code: "ACTION_CONFLICT" });
    await expect(
      adapter.request("dispatch", {
        actionId: "claude-steer-1",
        operation: "steer",
        payload: { resumeReference: "claude-session-1", prompt: "change course" },
      }),
    ).rejects.toMatchObject({ code: "CAPABILITY_UNAVAILABLE" });
    actionJournal.close();
  });

  it("advertises managed SDK capabilities without claiming an interactive TUI or in-place compaction", async () => {
    const actionJournal = await journal();
    const adapter = createClaudeAgentSdkAdapter({ boundary: claudeBoundary(), journal: actionJournal });

    await expect(adapter.request("capabilities", {})).resolves.toMatchObject({
      adapterId: "claude-agent-sdk",
      protocolVersion: 1,
      actionJournal: true,
      answerBearingSpawn: true,
      answerBearingSpawnTurns: "payload-max-turns",
      answerBearingUsageUnits: ["cost:USD", "input_tokens:anthropic", "output_tokens:anthropic"],
      controlModes: ["managed"],
      inboxDeliveryModes: ["structured-push"],
      compactInPlace: false,
    });
    actionJournal.close();
  });

  it("accepts one advertised private-environment chair launch without journalling its credential", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const launchChair = vi.fn(async (input: { providerContractDigest: string; providerAdapterId: string; actionId: string }) => (
      provedChairLaunch("claude-chair-session-1", input.providerContractDigest, input.providerAdapterId, input.actionId)
    ));
    Reflect.set(boundary, "launchChair", launchChair);
    const capability = "chair-capability-secret-canary";
    const socketPath = "/private/agent-fabric.sock";
    const adapterOptions = {
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath,
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    };
    const adapter = createClaudeAgentSdkAdapter(adapterOptions);
    const request = {
      schemaVersion: 1,
      actionId: "claude-chair-launch-1",
      providerContractDigest: `sha256:${"a".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "continue the reviewed project session",
      },
    };

    await expect(adapter.request("capabilities", {})).resolves.toMatchObject({
      operations: expect.arrayContaining(["launch_chair"]),
      chairLaunch: {
        schemaVersion: 1,
        method: "launch_chair",
        inputSchemaId: "claude-agent-sdk.chair-launch.v1",
        oneUse: true,
        secretTransport: "private-environment",
        environment: {
          capability: "AGENT_FABRIC_CAPABILITY",
          socketPath: "AGENT_FABRIC_SOCKET_PATH",
          attestationChallenge: "AGENT_FABRIC_ATTESTATION_CHALLENGE",
        },
        noEffectProofSchemas: {},
        attestation: {
          method: "provider-session-random-challenge-v1",
          bridgeContract: "agent-fabric-session-bridge-v1",
          origin: "provider-session-tool-call",
          oneUse: true,
          bridgeLifetime: "provider-session",
          digestAlgorithm: "sha256",
          nativeAttribution: "claude-sdk-assistant-request-tool-use-v1",
        },
      },
    });
    const advertised = await adapter.request("capabilities", {});
    const chairLaunch = Reflect.get(advertised as object, "chairLaunch");
    const parseChairLaunchCapability: unknown = Reflect.get(providerTypes, "parseChairLaunchCapability");
    expect(parseChairLaunchCapability).toBeTypeOf("function");
    expect(Reflect.apply(
      parseChairLaunchCapability as (...arguments_: unknown[]) => unknown,
      undefined,
      [chairLaunch],
    )).toEqual(chairLaunch);
    expect(() => Reflect.apply(
      parseChairLaunchCapability as (...arguments_: unknown[]) => unknown,
      undefined,
      [{ ...chairLaunch, providerContractDigest: request.providerContractDigest }],
    )).toThrowError("chair launch capability does not match its closed schema");
    await expect(adapter.request("launch_chair", request)).resolves.toEqual(
      provedChairLaunch("claude-chair-session-1", request.providerContractDigest, "claude-agent-sdk", request.actionId),
    );
    expect(launchChair).toHaveBeenCalledWith({
      actionId: "claude-chair-launch-1",
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: request.providerContractDigest,
      challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
      expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      payload: request.payload,
      environment: {
        AGENT_FABRIC_CAPABILITY: capability,
        AGENT_FABRIC_SOCKET_PATH: socketPath,
        AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
      },
    });
    const persisted = await adapter.request("lookup_action", { actionId: request.actionId });
    expect(JSON.stringify(persisted)).not.toContain(capability);
    expect(JSON.stringify(persisted)).not.toContain(socketPath);
    await expect(adapter.request("launch_chair", request)).resolves.toEqual(
      provedChairLaunch("claude-chair-session-1", request.providerContractDigest, "claude-agent-sdk", request.actionId),
    );
    expect(launchChair).toHaveBeenCalledOnce();
    await adapter.request("release", {
      actionId: "claude-chair-release-1",
      payload: { resumeReference: "claude-chair-session-1" },
    });
    await expect(adapter.request("lookup_action", { actionId: request.actionId })).rejects.toMatchObject({
      code: "CHAIR_BRIDGE_LOST",
    });
    actionJournal.close();
  });

  it("does not recreate chair continuity from a durable result after wrapper restart", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-chair-journal-restart-"));
    temporaryDirectories.push(directory);
    const path = join(directory, "actions.sqlite3");
    const request = {
      schemaVersion: 1,
      actionId: "claude-chair-restart-1",
      providerContractDigest: `sha256:${"b".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "start chair",
      },
    };
    const firstJournal = new SqliteAdapterActionJournal(path);
    const firstBoundary = claudeBoundary();
    Reflect.set(firstBoundary, "launchChair", vi.fn(async (input: {
      providerContractDigest: string;
      providerAdapterId: string;
      actionId: string;
    }) => provedChairLaunch(
      "claude-chair-restart-session",
      input.providerContractDigest,
      input.providerAdapterId,
      input.actionId,
    )));
    await createClaudeAgentSdkAdapter({
      boundary: firstBoundary,
      journal: firstJournal,
      chairLaunchHandoff: {
        capability: "chair-restart-capability-canary",
        socketPath: "/private/chair-restart.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    }).request("launch_chair", request);
    await expectJournalFilesNotToContain(path, ATTESTATION_CHALLENGE);
    for (const journalFile of [path, `${path}-wal`, `${path}-shm`]) {
      expect((await stat(journalFile)).mode & 0o777).toBe(0o600);
    }
    firstJournal.close();

    const reopenedJournal = new SqliteAdapterActionJournal(path);
    const replacementBoundary = claudeBoundary();
    Reflect.set(replacementBoundary, "launchChair", vi.fn(async () => {
      throw new Error("must not relaunch");
    }));
    const replacement = createClaudeAgentSdkAdapter({ boundary: replacementBoundary, journal: reopenedJournal });
    await expect(replacement.request("lookup_action", { actionId: request.actionId })).rejects.toMatchObject({
      code: "CHAIR_BRIDGE_LOST",
    });
    await expect(replacement.request("launch_chair", request)).rejects.toMatchObject({
      code: "CHAIR_BRIDGE_LOST",
    });
    reopenedJournal.close();
  });

  it("sanitises raw attestation material from ambiguous adapter-journal evidence", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-chair-journal-ambiguity-"));
    temporaryDirectories.push(directory);
    const path = join(directory, "actions.sqlite3");
    const actionJournal = new SqliteAdapterActionJournal(path);
    actionJournal.prepare(
      "chair-ambiguous-challenge-canary",
      "launch_chair",
      { providerContractDigest: `sha256:${"d".repeat(64)}` },
      [ATTESTATION_CHALLENGE],
    );
    actionJournal.markDispatched("chair-ambiguous-challenge-canary");
    const record = actionJournal.markAmbiguous(
      "chair-ambiguous-challenge-canary",
      { providerError: `native failure echoed ${ATTESTATION_CHALLENGE}` },
      [ATTESTATION_CHALLENGE],
    );

    expect(JSON.stringify(record)).not.toContain(ATTESTATION_CHALLENGE);
    await expectJournalFilesNotToContain(path, ATTESTATION_CHALLENGE);
    actionJournal.close();
  });

  it("rejects a changed chair-launch replay against the persisted public contract digest", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const launchChair = vi.fn(async (input: { providerContractDigest: string; providerAdapterId: string; actionId: string }) => (
      provedChairLaunch("claude-chair-session-2", input.providerContractDigest, input.providerAdapterId, input.actionId)
    ));
    Reflect.set(boundary, "launchChair", launchChair);
    const adapterOptions = {
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability: "changed-replay-capability-canary",
        socketPath: "/private/changed-replay.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    };
    const adapter = createClaudeAgentSdkAdapter(adapterOptions);
    const request = {
      schemaVersion: 1,
      actionId: "claude-chair-launch-2",
      providerContractDigest: `sha256:${"e".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "original reviewed work",
      },
    };

    await adapter.request("launch_chair", request);
    await expect(adapter.request("launch_chair", {
      ...request,
      payload: { ...request.payload, prompt: "changed work" },
    })).rejects.toMatchObject({ code: "ACTION_CONFLICT" });
    expect(launchChair).toHaveBeenCalledOnce();
    actionJournal.close();
  });

  it("rejects a private capability reused as a journalled chair-launch identifier", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const launchChair = vi.fn(async () => ({ resumeReference: "must-not-launch" }));
    Reflect.set(boundary, "launchChair", launchChair);
    const capability = "identifier-capability-secret-canary";
    const adapterOptions = {
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath: "/private/identifier.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    };
    const adapter = createClaudeAgentSdkAdapter(adapterOptions);

    await expect(adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: `launch-${capability}`,
      providerContractDigest: `sha256:${"f".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "reviewed work",
      },
    })).rejects.toMatchObject({ code: "PRIVATE_HANDOFF_DISCLOSED" });
    expect(launchChair).not.toHaveBeenCalled();
    actionJournal.close();
  });

  it("rejects an open provider launch result before core outcome binding", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const launchChair = vi.fn(async (input: { providerContractDigest: string; providerAdapterId: string; actionId: string }) => ({
      ...provedChairLaunch("claude-chair-session-open", input.providerContractDigest, input.providerAdapterId, input.actionId),
      unexpected: "must-not-cross-the-launch-boundary",
    }));
    Reflect.set(boundary, "launchChair", launchChair);
    const adapter = createClaudeAgentSdkAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability: "open-result-capability-canary",
        socketPath: "/private/open-result.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });

    await expect(adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: "claude-chair-open-result",
      providerContractDigest: `sha256:${"0".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "reviewed work",
      },
    })).rejects.toMatchObject({
      code: "CHAIR_LAUNCH_FAILED",
      message: "chair launch provider handoff failed",
    });
    const persisted = await adapter.request("lookup_action", {
      actionId: "claude-chair-open-result",
    });
    expect(persisted).toMatchObject({ status: "ambiguous" });
    expect(persisted).not.toHaveProperty("result");
    actionJournal.close();
  });

  it("rejects Fabric continuity evidence bound to another provider contract", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const launchChair = vi.fn(async () => provedChairLaunch(
      "claude-chair-wrong-contract",
      `sha256:${"3".repeat(64)}`,
      "claude-agent-sdk",
      "claude-chair-wrong-contract",
    ));
    Reflect.set(boundary, "launchChair", launchChair);
    const adapter = createClaudeAgentSdkAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability: "wrong-contract-capability-canary",
        socketPath: "/private/wrong-contract.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });

    await expect(adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: "claude-chair-wrong-contract",
      providerContractDigest: `sha256:${"4".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "reviewed work",
      },
    })).rejects.toMatchObject({ code: "CHAIR_LAUNCH_FAILED" });
    await expect(adapter.request("lookup_action", {
      actionId: "claude-chair-wrong-contract",
    })).resolves.toMatchObject({ status: "ambiguous", idempotencyProven: false });
    actionJournal.close();
  });

  it("redacts private handoff material from provider launch failures", async () => {
    const actionJournal = await journal();
    const boundary = claudeBoundary();
    const capability = "failure-capability-secret-canary";
    const socketPath = "/private/failure-socket-canary.sock";
    Reflect.set(boundary, "launchChair", vi.fn(async () => {
      throw new Error(`provider leaked ${capability} at ${socketPath}`);
    }));
    const adapter = createClaudeAgentSdkAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath,
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });

    const error = await adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: "claude-chair-redacted-failure",
      providerContractDigest: `sha256:${"1".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "reviewed work",
      },
    }).catch((cause: unknown) => cause);
    expect(error).toMatchObject({
      code: "CHAIR_LAUNCH_FAILED",
      message: "chair launch provider handoff failed",
      details: { actionId: "claude-chair-redacted-failure" },
    });
    expect(JSON.stringify(error)).not.toContain(capability);
    expect(JSON.stringify(error)).not.toContain(socketPath);
    expect(String(error)).not.toContain(capability);
    expect(String(error)).not.toContain(socketPath);
    const persisted = await adapter.request("lookup_action", {
      actionId: "claude-chair-redacted-failure",
    });
    expect(JSON.stringify(persisted)).not.toContain(capability);
    expect(JSON.stringify(persisted)).not.toContain(socketPath);
    actionJournal.close();
  });
});

describe("installed Claude chair launch boundary", () => {
  it("requires a provider MCP callback and reuses the retained bridge on a later turn", async () => {
    const actionJournal = await journal();
    const fabricServer = await fabricSocketFixture();
    const rpcCall = fabricServer.rpcCall;
    let bridge: Awaited<ReturnType<typeof createChairLaunchFabricBridge>> | undefined;
    let mcp: ReturnType<typeof createClaudeChairMcpBridge> | undefined;
    let queryCount = 0;
    const queryClose = vi.fn();
    let directMailboxError: unknown;
    let mismatchedMailboxError: unknown;
    let replayedMailboxError: unknown;
    let attestationProjection: unknown;
    let mailboxProjection: unknown;
    const queryFactory = vi.fn((input: unknown) => {
      queryCount += 1;
      const thisQuery = queryCount;
      return {
        close: queryClose,
        async *[Symbol.asyncIterator]() {
          yield { type: "system", subtype: "init", session_id: "claude-chair-session-1" };
          if (mcp === undefined || bridge === undefined) throw new Error("MCP bridge missing");
          if (thisQuery === 1) {
            yield {
              type: "assistant",
              session_id: "claude-chair-session-1",
              uuid: "claude-message-1",
              request_id: "claude-turn-1",
              parent_tool_use_id: null,
              message: {
                content: [{
                  type: "tool_use",
                  name: mcp.attestationToolName,
                  id: "claude-tool-call-1",
                  input: { challengeResponse: bridge.challengeResponse },
                }],
              },
            };
            attestationProjection = await mcp.invokeTool(bridge.challengeToolName, { challengeResponse: bridge.challengeResponse });
          } else {
            directMailboxError = await mcp.invokeTool("fabric_mailbox_read", {}).catch((error: unknown) => error);
            yield {
              type: "assistant",
              session_id: "claude-chair-session-1",
              uuid: "claude-message-mailbox-mismatch",
              request_id: "claude-turn-2",
              parent_tool_use_id: null,
              message: {
                content: [{
                  type: "tool_use",
                  name: mcp.mailboxToolName,
                  id: "claude-tool-mailbox-mismatch",
                  input: { unexpected: true },
                }],
              },
            };
            mismatchedMailboxError = await mcp.invokeTool("fabric_mailbox_read", {}).catch((error: unknown) => error);
            yield {
              type: "assistant",
              session_id: "claude-chair-session-1",
              uuid: "claude-message-mailbox-1",
              request_id: "claude-turn-2",
              parent_tool_use_id: null,
              message: {
                content: [{
                  type: "tool_use",
                  name: mcp.mailboxToolName,
                  id: "claude-tool-mailbox-1",
                  input: {},
                }],
              },
            };
            mailboxProjection = await mcp.invokeTool("fabric_mailbox_read", {});
            replayedMailboxError = await mcp.invokeTool("fabric_mailbox_read", {}).catch((error: unknown) => error);
          }
          yield {
            type: "result",
            subtype: "success",
            session_id: "claude-chair-session-1",
            result: thisQuery === 1 ? "chair ready" : "later turn ready",
            usage: {},
            total_cost_usd: 0,
          };
        },
        input,
      };
    });
    const providerContractDigest = `sha256:${"c".repeat(64)}`;
    const mcpBridgeFactory = vi.fn((session: Parameters<typeof createClaudeChairMcpBridge>[0]) => {
      mcp = createClaudeChairMcpBridge(session);
      return mcp;
    });
    const boundary = Reflect.construct(InstalledClaudeAgentSdkBoundary, [{
      executable: "/trusted/claude",
      verifyExecutable: vi.fn(async () => undefined),
      query: queryFactory,
      bridgeFactory: async (input: Parameters<typeof createChairLaunchFabricBridge>[0]) => {
        bridge = await createChairLaunchFabricBridge(input);
        return bridge;
      },
      mcpBridgeFactory,
    }]);
    const capability = `afc_${"c".repeat(43)}`;
    const socketPath = fabricServer.socketPath;
    const adapter = createClaudeAgentSdkAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath,
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });

    const launched = await adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: "claude-launch-1",
      providerContractDigest,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "continue reviewed work",
      },
    });
    expect(launched).toEqual(provedChairLaunch(
      "claude-chair-session-1",
      providerContractDigest,
      "claude-agent-sdk",
      "claude-launch-1",
      bridge?.challengeDigest,
      "claude-tool-call-1",
      "claude-turn-1",
    ));
    expect(JSON.stringify(launched)).not.toContain(ATTESTATION_CHALLENGE);

    const queryInput = queryFactory.mock.calls[0]?.[0] as {
      prompt: string;
      options: Record<string, unknown> & { env?: Record<string, string> };
    };
    expect(queryInput.prompt).toContain("challengeResponse");
    expect(queryInput.options.env).toBeUndefined();
    expect(queryInput.prompt).not.toContain(capability);
    expect(queryInput.prompt).not.toContain(socketPath);
    expect(Reflect.get(mcp ?? {}, "descriptors")).toStrictEqual(bridge?.descriptors);
    expect(Reflect.get(mcp ?? {}, "invokeTool")).toBeTypeOf("function");
    expect(queryInput.options.allowedTools).toStrictEqual(bridge?.descriptors.map(
      (descriptor) => `mcp__agent_fabric_session__${descriptor.name}`,
    ));
    if (mcp === undefined) throw new Error("Claude MCP bridge missing");
    const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
    const mcpClient = new Client({ name: "claude-parity-test", version: "1.0.0" });
    await Promise.all([
      mcp.server.instance.connect(serverTransport),
      mcpClient.connect(clientTransport),
    ]);
    const listed = await mcpClient.listTools();
    expect(listed.tools).toStrictEqual(mcp.descriptors.map(({ name, description, inputSchema, outputSchema }) => ({
      name,
      description,
      inputSchema,
      outputSchema,
      ...(name === bridge?.challengeToolName
        ? { _meta: { "anthropic/alwaysLoad": true } }
        : {}),
    })));
    await mcpClient.close();
    expect(attestationProjection).toStrictEqual({
      content: [{ type: "text", text: "launch continuity attested" }],
      structuredContent: { attested: true, challengeDigest: bridge?.challengeDigest },
    });
    expect(rpcCall).toHaveBeenCalledOnce();
    await boundary.sendTurn({ resumeReference: "claude-chair-session-1", prompt: "later work" });
    expect(mailboxProjection).toStrictEqual({
      content: [{ type: "text", text: "fabric_mailbox_read completed" }],
      structuredContent: { contiguousWatermark: 0, acknowledgedAboveWatermark: [] },
    });
    expect(JSON.stringify([attestationProjection, mailboxProjection])).not.toContain(ATTESTATION_CHALLENGE);
    expect(JSON.stringify([attestationProjection, mailboxProjection])).not.toContain(capability);
    expect(directMailboxError).toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    expect(mismatchedMailboxError).toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    expect(replayedMailboxError).toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    expect(rpcCall).toHaveBeenCalledTimes(2);
    expect(mcpBridgeFactory).toHaveBeenCalledOnce();
    expect(queryClose).toHaveBeenCalledTimes(2);
    await expect(adapter.request("lookup_action", { actionId: "claude-launch-1" })).resolves.toMatchObject({
      status: "terminal",
      idempotencyProven: true,
    });
    if (bridge === undefined) throw new Error("Claude bridge missing");
    await fabricServer.drop();
    await vi.waitFor(() => expect(bridge?.closed).toBe(true));
    await expect(adapter.request("lookup_action", { actionId: "claude-launch-1" })).rejects.toMatchObject({
      code: "CHAIR_BRIDGE_LOST",
    });
    await boundary.closeAll();
    actionJournal.close();
  });

  it("rejects an MCP handler call without a native provider tool event and preserves resume evidence", async () => {
    const actionJournal = await journal();
    const capability = "claude-probe-failure-capability-canary";
    const socketPath = "/private/claude-probe-failure.sock";
    let bridge: Awaited<ReturnType<typeof createChairLaunchFabricBridge>> | undefined;
    let mcp: ReturnType<typeof createClaudeChairMcpBridge> | undefined;
    let directInvocationError: unknown;
    let preAttestationMailboxError: unknown;
    const queryFactory = vi.fn((_input: unknown) => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        yield { type: "system", subtype: "init", session_id: "claude-chair-orphan-session" };
        if (bridge === undefined || mcp === undefined) throw new Error("test bridge missing");
        preAttestationMailboxError = await mcp.invokeTool("fabric_mailbox_read", {}).catch((error: unknown) => error);
        directInvocationError = await mcp.invokeTool(bridge.challengeToolName, {
          challengeResponse: bridge.challengeResponse,
        }).catch((error: unknown) => error);
        yield {
          type: "result",
          subtype: "success",
          session_id: "claude-chair-orphan-session",
          result: "bootstrap complete",
          usage: {},
          total_cost_usd: 0,
        };
      },
    }));
    const rpcClose = vi.fn(async () => undefined);
    const boundary = Reflect.construct(InstalledClaudeAgentSdkBoundary, [{
      query: queryFactory,
      bridgeFactory: async (input: Parameters<typeof createChairLaunchFabricBridge>[0]) => {
        bridge = await createChairLaunchFabricBridge(input, {
          connect: vi.fn(async () => providerSessionProtocolTransport(
            vi.fn(async () => ({ contiguousWatermark: 0, acknowledgedAboveWatermark: [] })),
            rpcClose,
          )),
        });
        return bridge;
      },
      mcpBridgeFactory: (session: Parameters<typeof createClaudeChairMcpBridge>[0]) => {
        mcp = createClaudeChairMcpBridge(session);
        return mcp;
      },
    }]);
    const adapter = createClaudeAgentSdkAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath,
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });
    const providerContractDigest = `sha256:${"5".repeat(64)}`;

    const error = await adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: "claude-launch-probe-failure",
      providerContractDigest,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "begin reviewed coordination",
      },
    }).catch((cause: unknown) => cause);
    expect(error).toMatchObject({
      code: "CHAIR_LAUNCH_FAILED",
      message: "chair launch provider handoff failed",
    });
    expect(String(error)).not.toContain(capability);
    expect(String(error)).not.toContain(socketPath);
    expect(preAttestationMailboxError).toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    expect(directInvocationError).toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    expect(rpcClose).toHaveBeenCalledOnce();
    await expect(adapter.request("lookup_action", {
      actionId: "claude-launch-probe-failure",
    })).resolves.toMatchObject({
      status: "ambiguous",
      history: ["prepared", "dispatched", "accepted", "ambiguous"],
      effectCount: 1,
      idempotencyProven: false,
      result: {
        kind: "continuity-unproven",
        providerContractDigest,
        resumeReference: "claude-chair-orphan-session",
        providerSessionGeneration: 1,
      },
    });
    actionJournal.close();
  });
});

describe("installed primary chair recovery boundaries", () => {
  it("reattaches the exact Claude session through a fresh native MCP attestation", async () => {
    const expectedPrincipal = { ...EXPECTED_CHAIR_PRINCIPAL, principalGeneration: 2 } as const;
    let bridge: Awaited<ReturnType<typeof createChairLaunchFabricBridge>> | undefined;
    let mcp: ReturnType<typeof createClaudeChairMcpBridge> | undefined;
    const rpcCall = vi.fn(async () => ({ contiguousWatermark: 0, acknowledgedAboveWatermark: [] }));
    const queryFactory = vi.fn((input: unknown) => ({
      close: vi.fn(),
      async *[Symbol.asyncIterator]() {
        yield { type: "system", subtype: "init", session_id: "claude-recovery-session" };
        if (mcp === undefined || bridge === undefined) throw new Error("Claude recovery bridge missing");
        yield {
          type: "assistant",
          session_id: "claude-recovery-session",
          uuid: "claude-recovery-message",
          request_id: "claude-recovery-turn",
          parent_tool_use_id: null,
          message: {
            content: [{
              type: "tool_use",
              name: mcp.attestationToolName,
              id: "claude-recovery-provider-call",
              input: { challengeResponse: bridge.challengeResponse },
            }],
          },
        };
        await mcp.invokeTool(bridge.challengeToolName, { challengeResponse: bridge.challengeResponse });
        yield {
          type: "result",
          subtype: "success",
          session_id: "claude-recovery-session",
          result: "recovered",
          usage: {},
          total_cost_usd: 0,
        };
      },
      input,
    }));
    const boundary = new InstalledClaudeAgentSdkBoundary({
      query: queryFactory as never,
      bridgeFactory: async (input) => {
        bridge = await createChairLaunchFabricBridge(input, {
          connect: vi.fn(async () => providerSessionProtocolTransport(
            rpcCall,
            vi.fn(async () => undefined),
            { kind: "agent", ...expectedPrincipal } as ProviderSessionProtocolTransport["principal"],
          )),
        });
        return bridge;
      },
      mcpBridgeFactory: (session) => {
        mcp = createClaudeChairMcpBridge(session);
        return mcp;
      },
    });
    const result = await boundary.recoverChair({
      actionId: "claude-native-recovery",
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: `sha256:${"6".repeat(64)}`,
      challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
      expectedPrincipal,
      recoveryId: "claude-native-recovery-custody",
      lossId: "claude-native-loss",
      resumeReference: "claude-recovery-session",
      expectedProviderSessionGeneration: 1,
      nextProviderSessionGeneration: 2,
      bridgeGeneration: 2,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        prompt: "resume reviewed work",
      },
      environment: {
        AGENT_FABRIC_CAPABILITY: "claude-native-recovery-capability",
        AGENT_FABRIC_SOCKET_PATH: "/private/claude-native-recovery.sock",
        AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
      },
    });
    expect(result).toMatchObject({
      resumeReference: "claude-recovery-session",
      providerSessionGeneration: 2,
      fabricContinuity: {
        providerActionId: "claude-native-recovery",
        providerSessionGeneration: 2,
      },
    });
    expect(boundary.hasLiveChairSession("claude-recovery-session", 2)).toBe(true);
    expect(queryFactory).toHaveBeenCalledOnce();
    await boundary.closeAll();
  });

  it("resumes the exact Codex thread through a fresh attributed dynamic-tool call", async () => {
    const expectedPrincipal = { ...EXPECTED_CHAIR_PRINCIPAL, principalGeneration: 2 } as const;
    let bridge: Awaited<ReturnType<typeof createChairLaunchFabricBridge>> | undefined;
    let handler: ((params: Record<string, unknown>) => Promise<unknown>) | undefined;
    let currentTurnId = "";
    const requests: Array<{ method: string; params: Record<string, unknown> }> = [];
    const connection = {
      get closed() { return false; },
      initialize: vi.fn(async () => undefined),
      setServerRequestHandler: vi.fn((_method: string, value: (params: Record<string, unknown>) => Promise<unknown>) => {
        handler = value;
      }),
      request: vi.fn(async (method: string, params: Record<string, unknown>) => {
        requests.push({ method, params });
        if (method === "thread/resume") return { thread: { id: "codex-recovery-thread" } };
        if (method === "turn/start") {
          currentTurnId = "codex-recovery-turn";
          return { turn: { id: currentTurnId, status: "inProgress" } };
        }
        if (method === "thread/read") {
          return {
            thread: {
              id: "codex-recovery-thread",
              turns: [{
                id: currentTurnId,
                status: "completed",
                items: [{ type: "agentMessage", text: "recovered" }],
              }],
            },
          };
        }
        throw new Error(`unexpected Codex recovery method ${method}`);
      }),
      waitForNotification: vi.fn(async () => {
        if (handler === undefined || bridge === undefined) throw new Error("Codex recovery handler missing");
        await handler({
          arguments: { challengeResponse: bridge.challengeResponse },
          callId: "codex-recovery-provider-call",
          threadId: "codex-recovery-thread",
          tool: bridge.challengeToolName,
          turnId: currentTurnId,
        });
        return {
          threadId: "codex-recovery-thread",
          turn: { id: currentTurnId, status: "completed" },
        };
      }),
      close: vi.fn(async () => undefined),
    };
    const boundary = new InstalledCodexAppServerBoundary(
      vi.fn(() => connection) as never,
      async (input) => {
        bridge = await createChairLaunchFabricBridge(input, {
          connect: vi.fn(async () => providerSessionProtocolTransport(
            vi.fn(async () => ({ contiguousWatermark: 0, acknowledgedAboveWatermark: [] })),
            vi.fn(async () => undefined),
            { kind: "agent", ...expectedPrincipal } as ProviderSessionProtocolTransport["principal"],
          )),
        });
        return bridge;
      },
    );
    const result = await boundary.recoverChair({
      actionId: "codex-native-recovery",
      providerAdapterId: "codex-app-server",
      providerContractDigest: `sha256:${"7".repeat(64)}`,
      challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
      expectedPrincipal,
      recoveryId: "codex-native-recovery-custody",
      lossId: "codex-native-loss",
      resumeReference: "codex-recovery-thread",
      expectedProviderSessionGeneration: 1,
      nextProviderSessionGeneration: 2,
      bridgeGeneration: 2,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "openai",
        model: "gpt-test",
        prompt: "resume reviewed work",
      },
      environment: {
        AGENT_FABRIC_CAPABILITY: "codex-native-recovery-capability",
        AGENT_FABRIC_SOCKET_PATH: "/private/codex-native-recovery.sock",
        AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
      },
    });
    expect(result).toMatchObject({
      resumeReference: "codex-recovery-thread",
      providerSessionGeneration: 2,
      fabricContinuity: {
        providerActionId: "codex-native-recovery",
        providerSessionGeneration: 2,
      },
    });
    expect(requests[0]).toMatchObject({ method: "thread/resume", params: { threadId: "codex-recovery-thread" } });
    expect(boundary.hasLiveChairSession("codex-recovery-thread", 2)).toBe(true);
    await boundary.closeAll();
  });
});

describe("Codex app-server response validation", () => {
  it("consumes a lifecycle handoff in a completed turn after starting or resuming", async () => {
    const requests: Array<{ method: string; params: Record<string, unknown> }> = [];
    const connection = {
      get closed() { return false; },
      initialize: vi.fn(async () => undefined),
      setServerRequestHandler: vi.fn(),
      request: vi.fn(async (method: string, params: Record<string, unknown>) => {
        requests.push({ method, params });
        if (method === "thread/resume") return { thread: { id: "codex-lifecycle-thread" } };
        if (method === "turn/start") return { turn: { id: "codex-lifecycle-turn", status: "inProgress" } };
        if (method === "thread/read") {
          return {
            thread: {
              id: "codex-lifecycle-thread",
              turns: [{
                id: "codex-lifecycle-turn",
                status: "completed",
                items: [{ type: "agentMessage", text: "checkpoint consumed" }],
              }],
            },
          };
        }
        throw new Error(`unexpected lifecycle method ${method}`);
      }),
      waitForNotification: vi.fn(async () => ({
        threadId: "codex-lifecycle-thread",
        turn: { id: "codex-lifecycle-turn", status: "completed" },
      })),
      close: vi.fn(async () => undefined),
    };
    const boundary = new InstalledCodexAppServerBoundary(vi.fn(() => connection) as never);

    await expect(boundary.spawn({
      priorResumeReference: "codex-lifecycle-thread",
      prompt: "verified checkpoint handoff",
      model: "gpt-test",
    })).resolves.toMatchObject({
      resumeReference: "codex-lifecycle-thread",
      turnId: "codex-lifecycle-turn",
      status: "completed",
      result: "checkpoint consumed",
    });
    expect(requests.map(({ method }) => method)).toEqual([
      "thread/resume",
      "turn/start",
      "thread/read",
    ]);
    expect(requests[1]).toMatchObject({
      method: "turn/start",
      params: {
        threadId: "codex-lifecycle-thread",
        input: [{ type: "text", text: "verified checkpoint handoff" }],
      },
    });
    await boundary.closeAll();
  });

  it("extracts only the final agent message from a completed turn", () => {
    expect(codexCompletedTurnResult({
      id: "turn-1",
      status: "completed",
      items: [
        { type: "userMessage", id: "user-1", content: [] },
        { type: "agentMessage", id: "agent-1", text: "FABRIC_SMOKE_TURN_OK" },
      ],
    })).toBe("FABRIC_SMOKE_TURN_OK");
  });

  it("fails closed for failed turns or missing agent output", () => {
    expect(() => codexCompletedTurnResult({ status: "failed", error: { message: "provider error" }, items: [] }))
      .toThrow("provider error");
    expect(() => codexCompletedTurnResult({ status: "completed", items: [] }))
      .toThrow("no agent message");
  });
});

describe("installed Codex chair launch boundary", () => {
  it("requires an attributed dynamic-tool callback and reuses it on a later turn", async () => {
    const actionJournal = await journal();
    const requests: Array<{ method: string; params: Record<string, unknown> }> = [];
    const fabricServer = await fabricSocketFixture();
    const rpcCall = fabricServer.rpcCall;
    let bridge: Awaited<ReturnType<typeof createChairLaunchFabricBridge>> | undefined;
    let serverRequestHandler: ((params: Record<string, unknown>) => Promise<unknown>) | undefined;
    const providerServerRequestIds: number[] = [];
    let attestationProjection: unknown;
    let mailboxProjection: unknown;
    let turnCount = 0;
    let currentTurnId = "";
    let connectionClosed = false;
    const connection = {
      get closed() {
        return connectionClosed;
      },
      initialize: vi.fn(async () => undefined),
      setServerRequestHandler: vi.fn((_method: string, handler: (params: Record<string, unknown>) => Promise<unknown>) => {
        serverRequestHandler = handler;
      }),
      request: vi.fn(async (method: string, params: Record<string, unknown>) => {
        requests.push({ method, params });
        if (method === "thread/start") return { thread: { id: "codex-chair-thread-1" } };
        if (method === "turn/start") {
          turnCount += 1;
          currentTurnId = `codex-chair-turn-${turnCount}`;
          return { turn: { id: currentTurnId, status: "inProgress" } };
        }
        return {
          thread: {
            id: "codex-chair-thread-1",
            turns: [{
              id: currentTurnId,
              status: "completed",
              items: [{ type: "agentMessage", text: "chair bootstrap complete" }],
            }],
          },
        };
      }),
      close: vi.fn(async () => {
        connectionClosed = true;
      }),
      waitForNotification: vi.fn(async () => {
        if (serverRequestHandler === undefined || bridge === undefined) throw new Error("dynamic tool handler missing");
        if (turnCount === 1) {
          await expect(serverRequestHandler({
            arguments: {},
            callId: "missing-challenge-call",
            threadId: "codex-chair-thread-1",
            tool: bridge.challengeToolName,
            turnId: currentTurnId,
          })).rejects.toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
          await expect(serverRequestHandler({
            arguments: { challengeResponse: Buffer.alloc(32, 9).toString("hex") },
            callId: "wrong-challenge-call",
            threadId: "codex-chair-thread-1",
            tool: bridge.challengeToolName,
            turnId: currentTurnId,
          })).rejects.toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
          await expect(serverRequestHandler({
            arguments: { challengeResponse: bridge.challengeResponse, extra: true },
            callId: "open-arguments-call",
            threadId: "codex-chair-thread-1",
            tool: bridge.challengeToolName,
            turnId: currentTurnId,
          })).rejects.toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
          attestationProjection = await serverRequestHandler({
            arguments: { challengeResponse: bridge.challengeResponse },
            callId: "codex-provider-tool-call-1",
            threadId: "codex-chair-thread-1",
            tool: bridge.challengeToolName,
            turnId: currentTurnId,
          });
        } else {
          const mailboxInvocation = {
            arguments: {},
            callId: "codex-provider-mailbox-call-1",
            threadId: "codex-chair-thread-1",
            tool: "fabric_mailbox_read",
            turnId: currentTurnId,
          };
          const invokeServerRequest = async (jsonRpcId: number): Promise<unknown> => {
            providerServerRequestIds.push(jsonRpcId);
            return await serverRequestHandler?.({ ...mailboxInvocation });
          };
          mailboxProjection = await invokeServerRequest(901);
          await expect(invokeServerRequest(902)).rejects.toMatchObject({
            code: "CHAIR_CONTINUITY_UNPROVEN",
          });
        }
        return {
          threadId: "codex-chair-thread-1",
          turn: { id: currentTurnId, status: "completed" },
        };
      }),
    };
    const connectionFactory = vi.fn(() => connection);
    const providerContractDigest = `sha256:${"d".repeat(64)}`;
    const bridgeFactory = async (input: Parameters<typeof createChairLaunchFabricBridge>[0]) => {
      bridge = await createChairLaunchFabricBridge(input);
      return bridge;
    };
    const boundary = Reflect.construct(InstalledCodexAppServerBoundary, [connectionFactory, bridgeFactory]);
    const capability = `afc_${"d".repeat(43)}`;
    const socketPath = fabricServer.socketPath;
    const adapter = createCodexAppServerAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability,
        socketPath,
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });

    const launched = await adapter.request("launch_chair", {
      schemaVersion: 1,
      actionId: "codex-launch-1",
      providerContractDigest,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "openai",
        model: "gpt-test",
        prompt: "begin chair coordination",
        ephemeral: false,
      },
    });
    expect(launched).toEqual(provedChairLaunch(
      "codex-chair-thread-1",
      providerContractDigest,
      "codex-app-server",
      "codex-launch-1",
      bridge?.challengeDigest,
      "codex-provider-tool-call-1",
      "codex-chair-turn-1",
    ));
    expect(JSON.stringify(launched)).not.toContain(ATTESTATION_CHALLENGE);

    expect(connectionFactory).toHaveBeenCalledWith(undefined);
    expect(requests[0]).toStrictEqual({
      method: "thread/start",
      params: {
        ...codexThreadConfiguration({
          cwd: "/workspace/project",
          modelFamily: "openai",
          model: "gpt-test",
          prompt: "begin chair coordination",
          ephemeral: false,
        }),
        dynamicTools: bridge?.descriptors.map((descriptor) => ({
          type: "function",
          name: descriptor.name,
          description: descriptor.description,
          inputSchema: descriptor.inputSchema,
          deferLoading: false,
        })),
      },
    });
    expect(requests[1]).toMatchObject({
      method: "turn/start",
      params: { threadId: "codex-chair-thread-1", model: "gpt-test" },
    });
    expect(JSON.stringify(requests)).not.toContain(capability);
    expect(JSON.stringify(requests)).not.toContain(socketPath);
    expect(attestationProjection).toStrictEqual({
      contentItems: [
        { type: "inputText", text: "launch continuity attested" },
        { type: "inputText", text: JSON.stringify({ attested: true, challengeDigest: bridge?.challengeDigest }) },
      ],
      success: true,
    });
    expect(connectionFactory).toHaveBeenCalledOnce();
    expect(connection.close).not.toHaveBeenCalled();
    expect(rpcCall).toHaveBeenCalledOnce();
    await expect(serverRequestHandler?.({
      arguments: {},
      callId: "late-provider-call",
      threadId: "codex-chair-thread-1",
      tool: "fabric_mailbox_read",
      turnId: "codex-chair-turn-1",
    })).rejects.toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    await boundary.sendTurn({ resumeReference: "codex-chair-thread-1", prompt: "later work" });
    expect(mailboxProjection).toStrictEqual({
      contentItems: [
        { type: "inputText", text: "fabric_mailbox_read completed" },
        { type: "inputText", text: JSON.stringify({ contiguousWatermark: 0, acknowledgedAboveWatermark: [] }) },
      ],
      success: true,
    });
    expect(JSON.stringify([attestationProjection, mailboxProjection])).not.toContain(ATTESTATION_CHALLENGE);
    expect(JSON.stringify([attestationProjection, mailboxProjection])).not.toContain(capability);
    expect(rpcCall).toHaveBeenCalledTimes(2);
    expect(providerServerRequestIds).toEqual([901, 902]);
    expect(connectionFactory).toHaveBeenCalledOnce();
    await expect(adapter.request("lookup_action", { actionId: "codex-launch-1" })).resolves.toMatchObject({
      status: "terminal",
      idempotencyProven: true,
    });
    await fabricServer.drop();
    await vi.waitFor(() => expect(bridge?.closed).toBe(true));
    await expect(adapter.request("lookup_action", { actionId: "codex-launch-1" })).rejects.toMatchObject({
      code: "CHAIR_BRIDGE_LOST",
    });
    await boundary.closeAll();
    expect(connection.close).toHaveBeenCalledOnce();
    actionJournal.close();
  });

  it("fences a Codex chair before a 257th distinct native tuple and retains replay history", async () => {
    let bridge: Awaited<ReturnType<typeof createChairLaunchFabricBridge>> | undefined;
    let serverRequestHandler: ((params: Record<string, unknown>) => Promise<unknown>) | undefined;
    let turnCount = 0;
    let currentTurnId = "";
    let connectionClosed = false;
    let capacityError: unknown;
    let replayError: unknown;
    const rpcCall = vi.fn(async () => ({ contiguousWatermark: 0, acknowledgedAboveWatermark: [] }));
    const connection = {
      get closed() {
        return connectionClosed;
      },
      initialize: vi.fn(async () => undefined),
      setServerRequestHandler: vi.fn((_method: string, handler: (params: Record<string, unknown>) => Promise<unknown>) => {
        serverRequestHandler = handler;
      }),
      request: vi.fn(async (method: string) => {
        if (method === "thread/start") return { thread: { id: "codex-capacity-thread" } };
        if (method === "turn/start") {
          turnCount += 1;
          currentTurnId = `codex-capacity-turn-${turnCount}`;
          return { turn: { id: currentTurnId, status: "inProgress" } };
        }
        return {
          thread: {
            id: "codex-capacity-thread",
            turns: [{
              id: currentTurnId,
              status: "completed",
              items: [{ type: "agentMessage", text: "capacity turn" }],
            }],
          },
        };
      }),
      close: vi.fn(async () => {
        connectionClosed = true;
      }),
      waitForNotification: vi.fn(async () => {
        if (serverRequestHandler === undefined || bridge === undefined) throw new Error("capacity handler missing");
        if (turnCount === 1) {
          await serverRequestHandler({
            arguments: { challengeResponse: bridge.challengeResponse },
            callId: "capacity-attestation",
            threadId: "codex-capacity-thread",
            tool: bridge.challengeToolName,
            turnId: currentTurnId,
          });
          return {
            threadId: "codex-capacity-thread",
            turn: { id: currentTurnId, status: "completed" },
          };
        }
        for (let index = 0; index < 256; index += 1) {
          await serverRequestHandler({
            arguments: {},
            callId: `capacity-call-${index}`,
            threadId: "codex-capacity-thread",
            tool: "fabric_mailbox_read",
            turnId: currentTurnId,
          });
        }
        capacityError = await serverRequestHandler({
          arguments: {},
          callId: "capacity-call-256",
          threadId: "codex-capacity-thread",
          tool: "fabric_mailbox_read",
          turnId: currentTurnId,
        }).catch((error: unknown) => error);
        replayError = await serverRequestHandler({
          arguments: {},
          callId: "capacity-call-0",
          threadId: "codex-capacity-thread",
          tool: "fabric_mailbox_read",
          turnId: currentTurnId,
        }).catch((error: unknown) => error);
        throw new Error("capacity-fenced provider connection");
      }),
    };
    const boundary = Reflect.construct(InstalledCodexAppServerBoundary, [
      vi.fn(() => connection),
      async (input: Parameters<typeof createChairLaunchFabricBridge>[0]) => {
        bridge = await createChairLaunchFabricBridge(input, {
          connect: vi.fn(async () => providerSessionProtocolTransport(
            rpcCall,
            vi.fn(async () => undefined),
          )),
        });
        return bridge;
      },
    ]);
    await boundary.launchChair({
      actionId: "codex-capacity-launch",
      providerAdapterId: "codex-app-server",
      providerContractDigest: `sha256:${"8".repeat(64)}`,
      challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
      expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "openai",
        model: "gpt-test",
        prompt: "begin capacity test",
      },
      environment: {
        AGENT_FABRIC_CAPABILITY: "codex-capacity-capability",
        AGENT_FABRIC_SOCKET_PATH: "/private/codex-capacity.sock",
        AGENT_FABRIC_ATTESTATION_CHALLENGE: ATTESTATION_CHALLENGE,
      },
    });

    await expect(boundary.sendTurn({
      resumeReference: "codex-capacity-thread",
      prompt: "exercise native tuple capacity",
    })).rejects.toThrow("capacity-fenced provider connection");
    expect(capacityError).toMatchObject({ code: "CHAIR_BRIDGE_LOST" });
    expect(replayError).toMatchObject({ code: "CHAIR_CONTINUITY_UNPROVEN" });
    expect(rpcCall).toHaveBeenCalledTimes(257);
    expect(connectionClosed).toBe(true);
    expect(bridge?.closed).toBe(true);
    expect(boundary.hasLiveChairSession("codex-capacity-thread", 1)).toBe(false);
    await boundary.closeAll();
  });
});

describe("Codex chair launch contract", () => {
  it("advertises and enforces a closed public payload before launch I/O", async () => {
    const actionJournal = await journal();
    const boundary = codexBoundary();
    const launchChair = vi.fn(async (input: { providerContractDigest: string; providerAdapterId: string; actionId: string }) => (
      provedChairLaunch("codex-chair-contract-thread", input.providerContractDigest, input.providerAdapterId, input.actionId)
    ));
    Reflect.set(boundary, "launchChair", launchChair);
    const adapter = createCodexAppServerAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability: "codex-contract-capability-canary",
        socketPath: "/private/codex-contract.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });
    const request = {
      schemaVersion: 1,
      providerContractDigest: `sha256:${"2".repeat(64)}`,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "openai",
        model: "gpt-test",
        prompt: "begin reviewed coordination",
      },
    };

    await expect(adapter.request("capabilities", {})).resolves.toMatchObject({
      chairLaunch: {
        inputSchemaId: "codex-app-server.chair-launch.v1",
        noEffectProofSchemas: {},
        publicPayloadSchema: {
          additionalProperties: false,
          // model is optional: ChatGPT-subscription accounts dispatch on the
          // account default and reject explicit model ids (#190).
          required: ["cwd", "modelFamily", "prompt"],
          properties: {
            cwd: { pattern: "^/" },
            modelFamily: { const: "openai" },
            ephemeral: { const: false },
          },
        },
      },
    });
    await expect(adapter.request("launch_chair", {
      ...request,
      actionId: "codex-chair-missing-prompt",
      payload: { cwd: "/workspace/project", modelFamily: "openai", model: "gpt-test" },
    })).rejects.toMatchObject({ code: "INVALID_PARAMS" });
    await expect(adapter.request("launch_chair", {
      ...request,
      actionId: "codex-chair-public-environment",
      payload: { ...request.payload, environment: { inherit: true } },
    })).rejects.toMatchObject({ code: "INVALID_PARAMS" });
    await expect(adapter.request("launch_chair", {
      ...request,
      actionId: "codex-chair-provider-override",
      payload: { ...request.payload, modelProvider: "unreviewed-provider" },
    })).rejects.toMatchObject({ code: "INVALID_PARAMS" });
    await expect(adapter.request("launch_chair", {
      ...request,
      actionId: "codex-chair-ephemeral",
      payload: { ...request.payload, ephemeral: true },
    })).rejects.toMatchObject({ code: "INVALID_PARAMS" });
    await expect(adapter.request("launch_chair", {
      ...request,
      actionId: "codex-chair-wrong-family",
      payload: { ...request.payload, modelFamily: "anthropic" },
    })).rejects.toMatchObject({ code: "INVALID_PARAMS" });
    expect(launchChair).not.toHaveBeenCalled();
    await expect(adapter.request("launch_chair", {
      ...request,
      actionId: "codex-chair-valid-contract",
    })).resolves.toEqual(provedChairLaunch(
      "codex-chair-contract-thread",
      request.providerContractDigest,
      "codex-app-server",
      "codex-chair-valid-contract",
    ));
    expect(launchChair).toHaveBeenCalledOnce();
    actionJournal.close();
  });

  it("admits an account-default launch payload without an explicit model", async () => {
    // ChatGPT-subscription accounts reject explicit model ids (HTTP 400) and
    // dispatch on the account default, so an absent model is valid (#190).
    const actionJournal = await journal();
    const boundary = codexBoundary();
    const launchChair = vi.fn(async (input: { providerContractDigest: string; providerAdapterId: string; actionId: string }) => (
      provedChairLaunch("codex-chair-account-default-thread", input.providerContractDigest, input.providerAdapterId, input.actionId)
    ));
    Reflect.set(boundary, "launchChair", launchChair);
    const adapter = createCodexAppServerAdapter({
      boundary,
      journal: actionJournal,
      chairLaunchHandoff: {
        capability: "codex-account-default-capability-canary",
        socketPath: "/private/codex-account-default.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      },
    });
    const providerContractDigest = `sha256:${"3".repeat(64)}`;
    await expect(adapter.request("launch_chair", {
      schemaVersion: 1,
      providerContractDigest,
      actionId: "codex-chair-account-default",
      payload: {
        cwd: "/workspace/project",
        modelFamily: "openai",
        prompt: "begin reviewed coordination",
      },
    })).resolves.toEqual(provedChairLaunch(
      "codex-chair-account-default-thread",
      providerContractDigest,
      "codex-app-server",
      "codex-chair-account-default",
    ));
    expect(launchChair).toHaveBeenCalledOnce();
    actionJournal.close();
  });
});

describe("trusted primary adapter configuration", () => {
  it("routes both primaries through tracked source fabric wrappers after activation", async () => {
    const root = fileURLToPath(new URL("../../../../", import.meta.url));
    const config: unknown = parse(await readFile(join(root, "config/agent-fabric.yaml"), "utf8"));
    const compatibility: unknown = parse(await readFile(join(root, "config/adapter-compatibility.yaml"), "utf8"));
    expect(config).toMatchObject({
      adapters: {
        "claude-agent-sdk": {
          command: expect.arrayContaining([
            expect.stringContaining("node_modules/tsx/dist/loader.mjs"),
            "--conditions=source",
            expect.stringContaining("src/adapters/providers/claude-agent-sdk.ts"),
          ]),
        },
        "codex-app-server": {
          command: expect.arrayContaining([
            expect.stringContaining("node_modules/tsx/dist/loader.mjs"),
            "--conditions=source",
            expect.stringContaining("src/adapters/providers/codex-app-server.ts"),
          ]),
        },
      },
    });
    expect(compatibility).toMatchObject({
      activation_policy: { default_enabled: false, real_adapters_require_separate_gate: true },
      adapters: {
        "claude-agent-sdk": {
          enabled: true,
        },
        "codex-app-server": {
          enabled: true,
        },
      },
    });
  });
});

describe("Codex app-server fabric adapter", () => {
  it("requires an absolute configured provider executable", () => {
    expect(codexAppServerCommand("/trusted/codex")).toEqual(["/trusted/codex", "app-server"]);
    expect(() => codexAppServerCommand("codex")).toThrow(/absolute/u);
  });
  it("forces read-only sandboxing regardless of caller-supplied permissions", () => {
    expect(codexThreadConfiguration({
      cwd: "/workspace/src",
      sandbox: "workspace-write",
      approvalPolicy: "on-request",
      permissions: "read-only",
    })).toEqual({ cwd: "/workspace/src", sandbox: "read-only", approvalPolicy: "never" });
  });
  it("keeps the Codex thread read-only while preparing the exact write-offline root", () => {
    expect(codexThreadConfiguration({
      cwd: "/workspace/src",
      readOnlyRoot: "/workspace/src",
      writeRoot: "/workspace/src",
      executionProfile: "workspace-write-offline",
      networkAccess: "none",
      sandbox: "workspace-write",
      approvalPolicy: "never",
      allowedTools: ["Read", "Glob", "Grep", "Write", "Edit", "Bash"],
    })).toEqual({
      cwd: "/workspace/src",
      sandbox: "read-only",
      approvalPolicy: "never",
      runtimeWorkspaceRoots: ["/workspace/src"],
      environments: [],
    });
  });
  it.each([
    { label: "fresh", priorResumeReference: undefined, threadMethod: "thread/start" },
    { label: "resumed", priorResumeReference: "codex-write-thread", threadMethod: "thread/resume" },
  ])("pins a $label write turn and restores the ordinary read-only policy", async ({ priorResumeReference, threadMethod }) => {
    const requests: Array<{ method: string; params: Record<string, unknown> }> = [];
    const connection = {
      get closed() { return false; },
      initialize: vi.fn(async () => undefined),
      setServerRequestHandler: vi.fn(),
      request: vi.fn(async (method: string, params: Record<string, unknown>) => {
        requests.push({ method, params });
        if (method === "thread/start" || method === "thread/resume") {
          return { thread: { id: "codex-write-thread" } };
        }
        if (method === "turn/start") return { turn: { id: "codex-write-turn", status: "inProgress" } };
        if (method === "thread/read") {
          return {
            thread: {
              id: "codex-write-thread",
              turns: [{
                id: "codex-write-turn",
                status: "completed",
                items: [{ type: "agentMessage", text: "bounded" }],
              }],
            },
          };
        }
        throw new Error(`unexpected write-offline method ${method}`);
      }),
      waitForNotification: vi.fn(async () => ({
        threadId: "codex-write-thread",
        turn: { id: "codex-write-turn", status: "completed" },
      })),
      close: vi.fn(async () => undefined),
    };
    const boundary = new InstalledCodexAppServerBoundary(vi.fn(() => connection) as never);
    const payload = {
      ...(priorResumeReference === undefined ? {} : { priorResumeReference }),
      prompt: "Run the approved offline case.",
      model: "gpt-controls-fresh",
      effort: "high",
      cwd: "/workspace/src",
      readOnlyRoot: "/workspace/src",
      writeRoot: "/workspace/src",
      executionProfile: "workspace-write-offline",
      networkAccess: "none",
      sandbox: "workspace-write",
      approvalPolicy: "never",
      allowedTools: ["Read", "Glob", "Grep", "Write", "Edit", "Bash"],
      sandboxPolicy: { type: "dangerFullAccess" },
      runtimeWorkspaceRoots: ["/host"],
      environments: [{ environmentId: "host", cwd: "/host" }],
    };

    await expect(boundary.spawn(payload)).resolves.toMatchObject({
      resumeReference: "codex-write-thread",
      turnId: "codex-write-turn",
      result: "bounded",
    });

    expect(requests[0]?.method).toBe(threadMethod);
    expect(requests[0]?.params).toMatchObject({ sandbox: "read-only", model: "gpt-controls-fresh" });
    if (threadMethod === "thread/start") expect(requests[0]?.params).toMatchObject({ environments: [] });
    else expect(requests[0]?.params).not.toHaveProperty("environments");
    expect(requests[1]).toEqual({
      method: "turn/start",
      params: {
        threadId: "codex-write-thread",
        input: [{ type: "text", text: "Run the approved offline case." }],
        cwd: "/workspace/src",
        runtimeWorkspaceRoots: ["/workspace/src"],
        environments: [],
        model: "gpt-controls-fresh",
        effort: "high",
        sandboxPolicy: {
          type: "workspaceWrite",
          writableRoots: ["/workspace/src"],
          networkAccess: false,
          excludeTmpdirEnvVar: true,
          excludeSlashTmp: true,
        },
      },
    });

    await expect(boundary.sendTurn({
      resumeReference: "codex-write-thread",
      prompt: "Continue without write authority.",
      model: "gpt-controls-resumed",
      effort: "xhigh",
    })).resolves.toMatchObject({ resumeReference: "codex-write-thread", result: "bounded" });
    expect(requests[3]).toEqual({
      method: "turn/start",
      params: {
        threadId: "codex-write-thread",
        input: [{ type: "text", text: "Continue without write authority." }],
        model: "gpt-controls-resumed",
        effort: "xhigh",
        sandboxPolicy: { type: "readOnly", networkAccess: false },
      },
    });
    await boundary.closeAll();
  });
  it("matches the exact current Codex review-readonly golden without a positive network fence", async () => {
    const admitted = await providerPermissionGolden("admitted");
    const configuration = codexThreadConfiguration({
      ...admitted,
      sandbox: "workspace-write",
      approvalPolicy: "on-request",
    });
    expect(configuration).toStrictEqual(await providerPermissionGolden("codex"));
    expect(configuration).not.toHaveProperty("network");
  });
  it("maps fabric turn, steer and release actions to an injected app-server boundary", async () => {
    const actionJournal = await journal();
    const boundary = codexBoundary();
    const adapter = createCodexAppServerAdapter({ boundary, journal: actionJournal });

    await expect(
      adapter.request("dispatch", {
        actionId: "codex-turn-1",
        operation: "send_turn",
        payload: { resumeReference: "codex-thread-1", prompt: "implement slice" },
      }),
    ).resolves.toMatchObject({ actionId: "codex-turn-1", status: "terminal", effectCount: 1 });
    await expect(
      adapter.request("dispatch", {
        actionId: "codex-steer-1",
        operation: "steer",
        payload: { resumeReference: "codex-thread-1", expectedTurnId: "turn-1", prompt: "focus tests" },
      }),
    ).resolves.toMatchObject({ status: "terminal" });
    await expect(
      adapter.request("release", {
        actionId: "codex-release-1",
        payload: { resumeReference: "codex-thread-1" },
      }),
    ).resolves.toEqual({ released: true, deleted: false });

    expect(boundary.sendTurn).toHaveBeenCalledTimes(1);
    expect(boundary.steer).toHaveBeenCalledTimes(1);
    expect(boundary.compact).not.toHaveBeenCalled();
    expect(boundary.release).toHaveBeenCalledTimes(1);
    actionJournal.close();
  });

  it("does not claim verified interactive injection even though app-server supports managed steering", async () => {
    const actionJournal = await journal();
    const adapter = createCodexAppServerAdapter({ boundary: codexBoundary(), journal: actionJournal });

    await expect(adapter.request("capabilities", {})).resolves.toMatchObject({
      adapterId: "codex-app-server",
      controlModes: ["managed"],
      inboxDeliveryModes: ["structured-push"],
      operations: expect.arrayContaining(["spawn", "attach", "send_turn", "steer", "interrupt"]),
      compactInPlace: false,
    });
    await expect(adapter.request("wakeup", { actionId: "wake-1", payload: {} })).rejects.toMatchObject({
      code: "CAPABILITY_UNAVAILABLE",
    });
    actionJournal.close();
  });

  it("advertises compact unavailable until Codex proves a next context generation", async () => {
    const actionJournal = await journal();
    const boundary = codexBoundary();
    const adapter = createCodexAppServerAdapter({ boundary, journal: actionJournal });

    const capabilities = await adapter.request("capabilities", {});
    expect(capabilities).toMatchObject({ compactInPlace: false });
    expect((capabilities as { operations: string[] }).operations).not.toContain("compact");
    await expect(adapter.request("compact", {
      actionId: "codex-compact-unproved",
      payload: { resumeReference: "codex-thread-1", generation: 2 },
    })).rejects.toMatchObject({ code: "CAPABILITY_UNAVAILABLE" });
    expect(boundary.compact).not.toHaveBeenCalled();
    actionJournal.close();
  });
});

describe("primary provider retained child bridges", () => {
  it.each(["claude", "codex"] as const)(
    "promotes the exact %s child under a distinct promotion action identity",
    async (provider) => {
      const actionJournal = await journal();
      const expectedPrincipal = {
        agentId: `${provider}-successor`,
        projectSessionId: "session-1",
        runId: "run-1",
        principalGeneration: 2,
      } as const;
      const adapterId = provider === "claude" ? "claude-agent-sdk" : "codex-app-server";
      const sourceActionId = `${provider}-successor-spawn`;
      const promotionActionId = `${provider}-successor-promotion`;
      const providerSessionRef = `${provider}-successor-session`;
      const bridgeContractDigest = `sha256:${"9".repeat(64)}`;
      const boundary = provider === "claude" ? claudeBoundary() : codexBoundary();
      Reflect.set(boundary, "provisionAgent", vi.fn(async () => ({
        schemaVersion: 1,
        adapterId,
        actionId: sourceActionId,
        targetAgentId: expectedPrincipal.agentId,
        providerSessionRef,
        providerSessionGeneration: 1,
        bridgeGeneration: 3,
        bridgeContractDigest,
        activationEvidenceDigest: `sha256:${"a".repeat(64)}`,
      })));
      Reflect.set(boundary, "hasLiveAgentSession", vi.fn(() => true));
      Reflect.set(boundary, "hasLiveChairSession", vi.fn(() => true));
      const adapter = provider === "claude"
        ? createClaudeAgentSdkAdapter({
            boundary: boundary as ClaudeAgentSdkBoundary,
            journal: actionJournal,
            agentBridgeHandoff: {
              capability: `${provider}-successor-capability-canary`,
              socketPath: `/private/${provider}-successor.sock`,
              expectedPrincipal,
            },
          })
        : createCodexAppServerAdapter({
            boundary: boundary as CodexAppServerBoundary,
            journal: actionJournal,
            agentBridgeHandoff: {
              capability: `${provider}-successor-capability-canary`,
              socketPath: `/private/${provider}-successor.sock`,
              expectedPrincipal,
            },
          });
      await adapter.request("provision_agent", {
        schemaVersion: 1,
        runId: expectedPrincipal.runId,
        operation: "spawn",
        actionId: sourceActionId,
        targetAgentId: expectedPrincipal.agentId,
        authorityId: `${provider}-successor-authority`,
        bridgeGeneration: 3,
        bridgeContractDigest,
        payload: {},
      });

      await expect(adapter.request("promote_retained_bridge", {
        schemaVersion: 1,
        actionId: promotionActionId,
        sourceActionId,
        agentId: expectedPrincipal.agentId,
        projectSessionId: expectedPrincipal.projectSessionId,
        runId: expectedPrincipal.runId,
        principalGeneration: expectedPrincipal.principalGeneration,
        providerSessionRef,
        providerSessionGeneration: 1,
        sourceBridgeGeneration: 3,
        chairBridgeGeneration: 4,
      })).resolves.toEqual({ schemaVersion: 1, promoted: true });
      await expect(adapter.request("retained_bridge_health", {
        schemaVersion: 1,
        kind: "chair",
        actionId: promotionActionId,
        agentId: expectedPrincipal.agentId,
        projectSessionId: expectedPrincipal.projectSessionId,
        runId: expectedPrincipal.runId,
        principalGeneration: expectedPrincipal.principalGeneration,
        providerSessionRef,
        providerSessionGeneration: 1,
        bridgeGeneration: 4,
      })).resolves.toEqual({ schemaVersion: 1, kind: "chair", live: true });
      actionJournal.close();
    },
  );

  it.each(["claude", "codex"] as const)(
    "rejects a public run substitution in the %s wrapper before provider or journal I/O",
    async (provider) => {
      const actionJournal = await journal();
      const expectedPrincipal = {
        agentId: `${provider}-child`,
        projectSessionId: "session-1",
        runId: "run-1",
        principalGeneration: 2,
      } as const;
      const boundary = provider === "claude" ? claudeBoundary() : codexBoundary();
      const providerIo = vi.fn();
      Reflect.set(boundary, "provisionAgent", providerIo);
      const adapter = provider === "claude"
        ? createClaudeAgentSdkAdapter({
            boundary: boundary as ClaudeAgentSdkBoundary,
            journal: actionJournal,
            agentBridgeHandoff: {
              capability: `${provider}-child-capability-canary`,
              socketPath: `/private/${provider}-child.sock`,
              expectedPrincipal,
            },
          })
        : createCodexAppServerAdapter({
            boundary: boundary as CodexAppServerBoundary,
            journal: actionJournal,
            agentBridgeHandoff: {
              capability: `${provider}-child-capability-canary`,
              socketPath: `/private/${provider}-child.sock`,
              expectedPrincipal,
            },
          });
      const actionId = `${provider}-wrong-public-run`;
      await expect(adapter.request("provision_agent", {
        schemaVersion: 1,
        runId: "foreign-run",
        operation: "spawn",
        actionId,
        targetAgentId: expectedPrincipal.agentId,
        authorityId: "child-authority",
        bridgeGeneration: 1,
        bridgeContractDigest: `sha256:${"0".repeat(64)}`,
        payload: {},
      })).rejects.toMatchObject({ code: "AGENT_BRIDGE_UNPROVEN" });
      expect(providerIo).not.toHaveBeenCalled();
      await expect(adapter.request("lookup_action", { actionId })).rejects.toMatchObject({ code: "ACTION_NOT_FOUND" });
      actionJournal.close();
    },
  );

  it.each([
    ["agentId", { agentId: "wrong-agent" }],
    ["projectSessionId", { projectSessionId: "wrong-session" }],
    ["runId", { runId: "wrong-run" }],
    ["principalGeneration", { principalGeneration: 99 }],
  ] as const)("rejects a child bridge with the wrong %s before descriptor projection", (_field, changed) => {
    const expectedPrincipal = {
      agentId: "child",
      projectSessionId: "session-1",
      runId: "run-1",
      principalGeneration: 2,
    } as const;
    const transport = providerSessionProtocolTransport(
      vi.fn(),
      vi.fn(async () => undefined),
      { kind: "agent", ...expectedPrincipal, ...changed } as ProviderSessionProtocolTransport["principal"],
    );
    const construct = (): unknown => Reflect.construct(AgentSessionFabricBridge as unknown as new (...args: never[]) => unknown, [{
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "child-action",
      targetAgentId: expectedPrincipal.agentId,
      expectedPrincipal,
      bridgeGeneration: 1,
      bridgeContractDigest: `sha256:${"1".repeat(64)}`,
      capability: `afc_${"1".repeat(43)}`,
      socketPath: "/private/fabric.sock",
    }, transport]);
    expect(construct).toThrow(expect.objectContaining({ code: "AGENT_BRIDGE_UNPROVEN" }));
    expect(transport.call).not.toHaveBeenCalled();
  });

  it.each(["claude", "codex"] as const)("performs zero provider I/O for a wrong-principal %s child bridge", async (provider) => {
    const expectedPrincipal = {
      agentId: `${provider}-child`,
      projectSessionId: "session-1",
      runId: "run-1",
      principalGeneration: 2,
    } as const;
    const transport = providerSessionProtocolTransport(
      vi.fn(),
      vi.fn(async () => undefined),
      { kind: "agent", ...expectedPrincipal, runId: "foreign-run" } as ProviderSessionProtocolTransport["principal"],
    );
    const agentBridgeFactory = vi.fn(async (input: Record<string, unknown>) => Reflect.construct(
      AgentSessionFabricBridge as unknown as new (...args: never[]) => unknown,
      [input, transport],
    ) as never);
    const input = {
      schemaVersion: 1 as const,
      runId: expectedPrincipal.runId,
      operation: "spawn" as const,
      actionId: `${provider}-wrong-principal-action`,
      targetAgentId: expectedPrincipal.agentId,
      authorityId: "child-authority",
      bridgeGeneration: 1,
      bridgeContractDigest: `sha256:${"2".repeat(64)}`,
      expectedPrincipal,
      payload: {
        cwd: "/workspace/project",
        modelFamily: provider === "claude" ? "anthropic" : "openai",
        model: "provider-test",
        initialPrompt: "must not start provider I/O",
      },
      environment: {
        AGENT_FABRIC_CAPABILITY: `afc_${"2".repeat(43)}`,
        AGENT_FABRIC_SOCKET_PATH: "/private/fabric.sock",
      },
    };
    if (provider === "claude") {
      const query = vi.fn();
      const boundary = new InstalledClaudeAgentSdkBoundary({ query: query as never, agentBridgeFactory });
      await expect(boundary.provisionAgent(input)).rejects.toMatchObject({ code: "AGENT_BRIDGE_UNPROVEN" });
      expect(query).not.toHaveBeenCalled();
    } else {
      const connectionFactory = vi.fn();
      const boundary = new InstalledCodexAppServerBoundary(connectionFactory as never, undefined, agentBridgeFactory);
      await expect(boundary.provisionAgent(input)).rejects.toMatchObject({ code: "AGENT_BRIDGE_UNPROVEN" });
      expect(connectionFactory).not.toHaveBeenCalled();
    }
    expect(transport.call).not.toHaveBeenCalled();
  });

  it("requires the one-use provider-session challenge before emitting lifecycle replacement evidence", async () => {
    const expectedPrincipal = {
      agentId: "child",
      projectSessionId: "session-1",
      runId: "run-1",
      principalGeneration: 2,
    } as const;
    const transport = providerSessionProtocolTransport(
      vi.fn(),
      vi.fn(async () => undefined),
      { kind: "agent", ...expectedPrincipal } as ProviderSessionProtocolTransport["principal"],
    );
    const binding = {
      custodyId: "lifecycle-custody-1",
      checkpointDigest: `sha256:${"c".repeat(64)}`,
      challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
    };
    const bridge = Reflect.construct(
      AgentSessionFabricBridge as unknown as new (...args: never[]) => AgentSessionFabricBridge,
      [{
        providerAdapterId: "codex-app-server",
        providerActionId: "lifecycle-action-1",
        targetAgentId: expectedPrincipal.agentId,
        expectedPrincipal,
        bridgeGeneration: 2,
        bridgeContractDigest: `sha256:${"b".repeat(64)}`,
        capability: `afc_${"1".repeat(43)}`,
        socketPath: "/private/fabric.sock",
        lifecycleAttestation: { ...binding, challenge: ATTESTATION_CHALLENGE },
      }, transport],
    );
    bridge.bindProviderSession("provider-session-2", 2);
    expect(() => bridge.result()).toThrow(expect.objectContaining({ code: "AGENT_BRIDGE_UNPROVEN" }));
    const ordinaryTool = (bridge.descriptors as readonly { name: string; operation: string }[])
      .find((descriptor) => descriptor.operation === FABRIC_OPERATIONS.getMailboxState);
    expect(ordinaryTool).toBeDefined();
    await expect(bridge.invokeTool(ordinaryTool!.name, {}, {
      providerSessionRef: "provider-session-2",
      providerSessionGeneration: 2,
      providerTurnRef: "turn-0",
      providerInvocationRef: "call-0",
    })).rejects.toMatchObject({ code: "AGENT_BRIDGE_UNPROVEN" });
    expect(() => bridge.result()).toThrow(expect.objectContaining({ code: "AGENT_BRIDGE_UNPROVEN" }));
    await expect(bridge.invokeTool(bridge.activationToolName, { challengeResponse: "00".repeat(32) }, {
      providerSessionRef: "provider-session-2",
      providerSessionGeneration: 2,
      providerTurnRef: "turn-1",
      providerInvocationRef: "call-1",
    })).rejects.toMatchObject({ code: "AGENT_BRIDGE_UNPROVEN" });
    await bridge.invokeTool(bridge.activationToolName, { challengeResponse: ATTESTATION_CHALLENGE }, {
      providerSessionRef: "provider-session-2",
      providerSessionGeneration: 2,
      providerTurnRef: "turn-1",
      providerInvocationRef: "call-2",
    });
    const result = bridge.result();
    expect(providerTypes.parseAgentProvisionProviderResult(result, {
      adapterId: "codex-app-server",
      actionId: "lifecycle-action-1",
      targetAgentId: expectedPrincipal.agentId,
      bridgeGeneration: 2,
      bridgeContractDigest: `sha256:${"b".repeat(64)}`,
      lifecycleAttestation: binding,
    })).toEqual(result);
    await expect(bridge.invokeTool(bridge.activationToolName, { challengeResponse: ATTESTATION_CHALLENGE }, {
      providerSessionRef: "provider-session-2",
      providerSessionGeneration: 2,
      providerTurnRef: "turn-1",
      providerInvocationRef: "call-3",
    })).rejects.toMatchObject({ code: "AGENT_BRIDGE_UNPROVEN" });
  });

  it.each(["claude", "codex"] as const)(
    "rejects a %s lifecycle provider result that reflects the raw private challenge as a provider reference",
    async (provider) => {
      const actionJournal = await journal();
      const expectedPrincipal = {
        agentId: `${provider}-lifecycle-child`,
        projectSessionId: "session-1",
        runId: "run-1",
        principalGeneration: 2,
      } as const;
      const adapterId = provider === "claude" ? "claude-agent-sdk" : "codex-app-server";
      const actionId = `${provider}-reflected-lifecycle-challenge`;
      const challenge = ATTESTATION_CHALLENGE;
      const binding = {
        custodyId: `${provider}-custody`,
        checkpointDigest: `sha256:${"c".repeat(64)}`,
        challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
      };
      const unsigned = {
        schemaVersion: 1 as const,
        kind: "provider-session-lifecycle-attestation" as const,
        custodyId: binding.custodyId,
        actionId,
        checkpointDigest: binding.checkpointDigest,
        challengeDigest: binding.challengeDigest,
        providerSessionRef: challenge,
        providerSessionGeneration: 2,
        bridgeGeneration: 2,
        providerTurnRef: "turn-reflected",
        providerInvocationRef: "call-reflected",
      };
      const boundary = provider === "claude" ? claudeBoundary() : codexBoundary();
      Reflect.set(boundary, "provisionAgent", vi.fn(async () => ({
        schemaVersion: 1,
        adapterId,
        actionId,
        targetAgentId: expectedPrincipal.agentId,
        providerSessionRef: challenge,
        providerSessionGeneration: 2,
        bridgeGeneration: 2,
        bridgeContractDigest: `sha256:${"b".repeat(64)}`,
        activationEvidenceDigest: providerTypes.agentLifecycleAttestationDigest(unsigned),
        lifecycleAttestation: {
          ...unsigned,
          attestationDigest: providerTypes.agentLifecycleAttestationDigest(unsigned),
        },
      })));
      const handoff = {
        capability: `afc_${"1".repeat(43)}`,
        socketPath: `/private/${provider}-lifecycle.sock`,
        expectedPrincipal,
        lifecycleAttestation: { ...binding, challenge },
      };
      const adapter = provider === "claude"
        ? createClaudeAgentSdkAdapter({ boundary: boundary as ClaudeAgentSdkBoundary, journal: actionJournal,
            agentBridgeHandoff: handoff })
        : createCodexAppServerAdapter({ boundary: boundary as CodexAppServerBoundary, journal: actionJournal,
            agentBridgeHandoff: handoff });
      await expect(adapter.request("provision_agent", {
        schemaVersion: 1,
        runId: expectedPrincipal.runId,
        operation: "spawn",
        actionId,
        targetAgentId: expectedPrincipal.agentId,
        authorityId: `${provider}-authority`,
        bridgeGeneration: 2,
        bridgeContractDigest: `sha256:${"b".repeat(64)}`,
        payload: { generation: 2 },
      })).rejects.toMatchObject({ code: "PROVIDER_RESPONSE_INVALID" });
      actionJournal.close();
    },
  );

  it.each(["claude", "codex"] as const)(
    "sanitises a %s lifecycle provider error that reflects the private challenge",
    async (provider) => {
      const directory = await mkdtemp(join(tmpdir(), `agent-fabric-${provider}-lifecycle-error-`));
      temporaryDirectories.push(directory);
      const journalPath = join(directory, "actions.sqlite3");
      const actionJournal = new SqliteAdapterActionJournal(journalPath);
      const expectedPrincipal = {
        agentId: `${provider}-error-child`,
        projectSessionId: "session-1",
        runId: "run-1",
        principalGeneration: 2,
      } as const;
      const boundary = provider === "claude" ? claudeBoundary() : codexBoundary();
      Reflect.set(boundary, "provisionAgent", vi.fn(async () => {
        const reflected = new ProviderAdapterError(
          "PROVIDER_TURN_FAILED",
          `provider reflected ${ATTESTATION_CHALLENGE}`,
          { structured: { challenge: ATTESTATION_CHALLENGE } },
          { cause: new Error(`nested ${ATTESTATION_CHALLENGE}`) },
        );
        reflected.stack = `provider stack ${ATTESTATION_CHALLENGE}`;
        throw reflected;
      }));
      const handoff = {
        capability: `afc_${"1".repeat(43)}`,
        socketPath: `/private/${provider}-lifecycle-error.sock`,
        expectedPrincipal,
        lifecycleAttestation: {
          custodyId: `${provider}-error-custody`,
          checkpointDigest: `sha256:${"c".repeat(64)}`,
          challengeDigest: ATTESTATION_CHALLENGE_DIGEST,
          challenge: ATTESTATION_CHALLENGE,
        },
      };
      const adapter = provider === "claude"
        ? createClaudeAgentSdkAdapter({
            boundary: boundary as ClaudeAgentSdkBoundary,
            journal: actionJournal,
            agentBridgeHandoff: handoff,
          })
        : createCodexAppServerAdapter({
            boundary: boundary as CodexAppServerBoundary,
            journal: actionJournal,
            agentBridgeHandoff: handoff,
          });
      const actionId = `${provider}-private-error`;
      const error = await adapter.request("provision_agent", {
        schemaVersion: 1,
        runId: expectedPrincipal.runId,
        operation: "spawn",
        actionId,
        targetAgentId: expectedPrincipal.agentId,
        authorityId: `${provider}-authority`,
        bridgeGeneration: 2,
        bridgeContractDigest: `sha256:${"b".repeat(64)}`,
        payload: { generation: 2 },
      }).catch((cause: unknown) => cause);
      expect(error).toMatchObject({
        code: "PROVIDER_RESPONSE_INVALID",
        message: "agent provision provider error contained private handoff material",
      });
      expect(Reflect.get(error as object, "cause")).toBeUndefined();
      expect(Reflect.get(error as object, "details")).toBeUndefined();
      expect(String(Reflect.get(error as object, "stack"))).not.toContain(ATTESTATION_CHALLENGE);
      expect(JSON.stringify(actionJournal.get(actionId))).not.toContain(ATTESTATION_CHALLENGE);
      await expectJournalFilesNotToContain(journalPath, ATTESTATION_CHALLENGE);
      actionJournal.close();
    },
  );

  it("provisions and reuses the exact Claude SDK child MCP bridge", async () => {
    const childPrincipal = {
      agentId: "claude-child",
      projectSessionId: "session-1",
      runId: "run-1",
      principalGeneration: 1,
    } as const;
    const fabricServer = await fabricSocketFixture(childPrincipal);
    let mcp: ReturnType<typeof createClaudeChairMcpBridge> | undefined;
    let queryCount = 0;
    const queryFactory = vi.fn((input: unknown) => {
      queryCount += 1;
      const turn = queryCount;
      return {
        close: vi.fn(),
        async *[Symbol.asyncIterator]() {
          yield { type: "system", subtype: "init", session_id: "claude-child-session" };
          if (mcp === undefined) throw new Error("Claude child MCP bridge missing");
          yield {
            type: "assistant",
            session_id: "claude-child-session",
            uuid: `claude-child-message-${String(turn)}`,
            request_id: `claude-child-turn-${String(turn)}`,
            parent_tool_use_id: null,
            message: {
              content: [{
                type: "tool_use",
                name: mcp.attestationToolName,
                id: `claude-child-call-${String(turn)}`,
                input: {},
              }],
            },
          };
          await mcp.invokeTool("fabric_mailbox_read", {});
          yield {
            type: "result",
            subtype: "success",
            session_id: "claude-child-session",
            result: turn === 1 ? "child activated" : "child continued",
            usage: {},
            total_cost_usd: 0,
          };
        },
        input,
      };
    });
    const boundary = Reflect.construct(InstalledClaudeAgentSdkBoundary, [{
      query: queryFactory,
      mcpBridgeFactory: (session: Parameters<typeof createClaudeChairMcpBridge>[0]) => {
        mcp = createClaudeChairMcpBridge(session);
        return mcp;
      },
    }]);
    const bridgeContractDigest = `sha256:${"7".repeat(64)}`;
    const capability = `afc_${"7".repeat(43)}`;

    const result = await boundary.provisionAgent({
      schemaVersion: 1,
      runId: childPrincipal.runId,
      operation: "spawn",
      actionId: "claude-child-action",
      targetAgentId: "claude-child",
      authorityId: "claude-child-authority",
      bridgeGeneration: 3,
      bridgeContractDigest,
      expectedPrincipal: childPrincipal,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "anthropic",
        model: "claude-test",
        initialPrompt: "perform bounded work",
      },
      environment: {
        AGENT_FABRIC_CAPABILITY: capability,
        AGENT_FABRIC_SOCKET_PATH: fabricServer.socketPath,
      },
    });
    expect(result).toMatchObject({
      schemaVersion: 1,
      adapterId: "claude-agent-sdk",
      actionId: "claude-child-action",
      targetAgentId: "claude-child",
      providerSessionRef: "claude-child-session",
      providerSessionGeneration: 1,
      bridgeGeneration: 3,
      bridgeContractDigest,
      activationEvidenceDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
    });
    const publicQueryInputs = queryFactory.mock.calls.map(([value]) => {
      const input = value as { prompt?: unknown; options?: { env?: unknown } };
      return { prompt: input.prompt, env: input.options?.env };
    });
    expect(JSON.stringify(publicQueryInputs)).not.toContain(capability);
    expect(JSON.stringify(publicQueryInputs)).not.toContain(fabricServer.socketPath);
    expect(boundary.hasLiveAgentSession("claude-child-session", 1, 3)).toBe(true);
    await boundary.sendTurn({ resumeReference: "claude-child-session", prompt: "read the mailbox again" });
    expect(fabricServer.rpcCall).toHaveBeenCalledTimes(2);
    await fabricServer.drop();
    await vi.waitFor(() => expect(boundary.hasLiveAgentSession("claude-child-session", 1, 3)).toBe(false));
    await boundary.closeAll();
  });

  it("provisions and reuses the exact Codex dynamic-tool child bridge", async () => {
    const childPrincipal = {
      agentId: "codex-child",
      projectSessionId: "session-1",
      runId: "run-1",
      principalGeneration: 1,
    } as const;
    const fabricServer = await fabricSocketFixture(childPrincipal);
    const requests: Array<{ method: string; params: Record<string, unknown> }> = [];
    let handler: ((params: Record<string, unknown>) => Promise<unknown>) | undefined;
    let turn = 0;
    let currentTurnId = "";
    let closed = false;
    const connection = {
      get closed() { return closed; },
      initialize: vi.fn(async () => undefined),
      setServerRequestHandler: vi.fn((_method: string, value: (params: Record<string, unknown>) => Promise<unknown>) => {
        handler = value;
      }),
      request: vi.fn(async (method: string, params: Record<string, unknown>) => {
        requests.push({ method, params });
        if (method === "thread/start") return { thread: { id: "codex-child-thread" } };
        if (method === "turn/start") {
          turn += 1;
          currentTurnId = `codex-child-turn-${String(turn)}`;
          return { turn: { id: currentTurnId, status: "inProgress" } };
        }
        if (method === "thread/read") {
          return {
            thread: {
              id: "codex-child-thread",
              turns: [{
                id: currentTurnId,
                status: "completed",
                items: [{ type: "agentMessage", text: "child turn complete" }],
              }],
            },
          };
        }
        throw new Error(`unexpected Codex method ${method}`);
      }),
      waitForNotification: vi.fn(async () => {
        if (handler === undefined) throw new Error("Codex child tool handler missing");
        await handler({
          arguments: {},
          callId: `codex-child-call-${String(turn)}`,
          threadId: "codex-child-thread",
          tool: "fabric_mailbox_read",
          turnId: currentTurnId,
        });
        return {
          threadId: "codex-child-thread",
          turn: { id: currentTurnId, status: "completed" },
        };
      }),
      close: vi.fn(async () => { closed = true; }),
    };
    const connectionFactory = vi.fn(() => connection);
    const boundary = new InstalledCodexAppServerBoundary(connectionFactory as never);
    const bridgeContractDigest = `sha256:${"8".repeat(64)}`;
    const capability = `afc_${"8".repeat(43)}`;

    const result = await boundary.provisionAgent({
      schemaVersion: 1,
      runId: childPrincipal.runId,
      operation: "spawn",
      actionId: "codex-child-action",
      targetAgentId: "codex-child",
      authorityId: "codex-child-authority",
      bridgeGeneration: 4,
      bridgeContractDigest,
      expectedPrincipal: childPrincipal,
      payload: {
        cwd: "/workspace/project",
        modelFamily: "openai",
        model: "gpt-test",
        initialPrompt: "perform bounded work",
      },
      environment: {
        AGENT_FABRIC_CAPABILITY: capability,
        AGENT_FABRIC_SOCKET_PATH: fabricServer.socketPath,
      },
    });
    expect(result).toMatchObject({
      schemaVersion: 1,
      adapterId: "codex-app-server",
      actionId: "codex-child-action",
      targetAgentId: "codex-child",
      providerSessionRef: "codex-child-thread",
      providerSessionGeneration: 1,
      bridgeGeneration: 4,
      bridgeContractDigest,
      activationEvidenceDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
    });
    expect(JSON.stringify(requests)).not.toContain(capability);
    expect(JSON.stringify(requests)).not.toContain(fabricServer.socketPath);
    expect(boundary.hasLiveAgentSession("codex-child-thread", 1, 4)).toBe(true);
    await boundary.sendTurn({ resumeReference: "codex-child-thread", prompt: "read the mailbox again" });
    expect(fabricServer.rpcCall).toHaveBeenCalledTimes(2);
    await fabricServer.drop();
    await vi.waitFor(() => expect(boundary.hasLiveAgentSession("codex-child-thread", 1, 4)).toBe(false));
    await boundary.closeAll();
  });
});
