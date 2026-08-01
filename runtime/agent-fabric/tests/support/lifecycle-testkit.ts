import { createHash, randomUUID } from "node:crypto";
import { existsSync, watch } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { PROTOCOL_FEATURES, PROTOCOL_LIMITS } from "@local/agent-fabric-protocol";

import {
  AUTHORITY_ACTION_VOCABULARY,
  connectFabricDaemon,
  openFabric,
  startFabricDaemon,
} from "../../src/index.ts";
import type {
  AuthorityInput,
  Fabric,
  FabricClient,
  LifecycleIntegrityReceiptAuthorityPort,
} from "../../src/index.ts";
import { servePublicProtocolConnection } from "../../src/daemon/public-protocol.ts";
import { waitUntil } from "../shared/deadline-wait.ts";

import { createCurrentSessionRun } from "./current-session-testkit.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "./authority-v2-testkit.ts";
import { ManualClock } from "./manual-clock.ts";
import { callTool, spawnMcpProxy, type McpProxy } from "./mcp-testkit.ts";
import { terminateTrackedTestProcess, trackTestProcess, untrackTestProcess } from "./test-process-registry.ts";

const fakeProvider = fileURLToPath(new URL("./lifecycle-fake-provider.ts", import.meta.url));
type OwnedDaemon = Awaited<ReturnType<typeof startFabricDaemon>>;

async function stopOwnedDaemon(daemon: OwnedDaemon): Promise<void> {
  try {
    await daemon.stop();
    untrackTestProcess(daemon.pid);
  } finally {
    await terminateTrackedTestProcess(daemon.pid);
  }
}

export type LifecycleCheckpoint = {
  relativePath: string;
  sha256: string;
  mailboxWatermark: number;
  acknowledgedAboveWatermark: number[];
  inFlightChildren: string[];
  openWork: string[];
  nextAction: string;
  providerResumeReference: string;
};

export type LifecycleResult = {
  agentId: string;
  lifecycle: string;
  providerSessionGeneration: number;
  rotation?: { kind: "in-place" | "replacement-session"; priorResumeReference: string };
};

export type ProviderActionResult = {
  actionId: string;
  status: "prepared" | "dispatched" | "accepted" | "terminal" | "ambiguous" | "quarantined";
  history: string[];
  executionCount: number;
  effectCount: number;
  result?: unknown;
};

type Stage3OpenOptions = {
  databasePath: string;
  workspaceRoots: string[];
  clock: ManualClock["now"];
  maximumConcurrentProviderTurns?: number;
  adapters: Record<string, { command: string[]; environment: Record<string, string> }>;
  fault?: (label: string) => void;
};

export type LifecycleFixture = {
  directory: string;
  runDirectory: string;
  databasePath: string;
  providerJournalPath: string;
  providerSpawnBarrier?: {
    waitUntilEntered: () => Promise<void>;
    release: () => Promise<void>;
  };
  secondaryProviderJournalPath?: string;
  providerSessionMarker: string;
  clock: ManualClock;
  fabric: Fabric;
  capabilities: { chair: string; leader: string; child: string };
  chairAuthorityId: string;
  rootAuthority: AuthorityInput;
  chair: FabricClient;
  leader: FabricClient;
  child: FabricClient;
  runId: string;
  leaderTask: { taskId: string; revision: number };
  childTask: { taskId: string; revision: number };
  restartRetainedDaemon?: () => Promise<{ fabric: Fabric; chair: FabricClient }>;
};

export function asLifecycleClient(client: FabricClient): FabricClient {
  return client;
}

function adapterOptions(fixture: {
  databasePath: string;
  directory: string;
  clock: ManualClock;
  providerJournalPath: string;
  secondaryProviderJournalPath?: string;
  providerStatusCallsPath?: string;
  providerStatus?: "healthy" | "unmanaged" | "missing-evidence";
  capabilitiesDelayMs?: number;
  spawnDelayMs?: number;
  spawnBarrierPaths?: { entered: string; release: string };
  mandatoryUsageUnits?: boolean;
  maximumConcurrentProviderTurns?: number;
  payloadMaxTurns?: boolean;
  spawnResultLost?: boolean;
  spawnUnresolved?: boolean;
  spawnLookupMissing?: boolean;
  fault?: (label: string) => void;
}): Stage3OpenOptions {
  return {
    databasePath: fixture.databasePath,
    workspaceRoots: [fixture.directory],
    clock: fixture.clock.now,
    ...(fixture.maximumConcurrentProviderTurns === undefined
      ? {}
      : { maximumConcurrentProviderTurns: fixture.maximumConcurrentProviderTurns }),
    adapters: {
      "fake-lifecycle": {
        command: [process.execPath, "--import", "tsx", fakeProvider],
        environment: {
          LIFECYCLE_FAKE_JOURNAL: fixture.providerJournalPath,
          LIFECYCLE_FAKE_ADAPTER_ID: "fake-lifecycle",
          LIFECYCLE_FAKE_STATUS: fixture.providerStatus ?? "healthy",
          ...(fixture.providerStatusCallsPath === undefined
            ? {}
            : { LIFECYCLE_FAKE_STATUS_CALLS: fixture.providerStatusCallsPath }),
          ...(fixture.capabilitiesDelayMs === undefined
            ? {}
            : { LIFECYCLE_FAKE_CAPABILITIES_DELAY_MS: String(fixture.capabilitiesDelayMs) }),
          ...(fixture.spawnDelayMs === undefined
            ? {}
            : { LIFECYCLE_FAKE_SPAWN_DELAY_MS: String(fixture.spawnDelayMs) }),
          ...(fixture.spawnBarrierPaths === undefined
            ? {}
            : {
                LIFECYCLE_FAKE_SPAWN_BARRIER_ENTERED: fixture.spawnBarrierPaths.entered,
                LIFECYCLE_FAKE_SPAWN_BARRIER_RELEASE: fixture.spawnBarrierPaths.release,
              }),
          LIFECYCLE_FAKE_MANDATORY_USAGE: fixture.mandatoryUsageUnits === true ? "1" : "0",
          LIFECYCLE_FAKE_PAYLOAD_MAX_TURNS: fixture.payloadMaxTurns === true ? "1" : "0",
          LIFECYCLE_FAKE_SPAWN_RESULT_LOST: fixture.spawnResultLost === true ? "1" : "0",
          LIFECYCLE_FAKE_SPAWN_UNRESOLVED: fixture.spawnUnresolved === true ? "1" : "0",
          LIFECYCLE_FAKE_SPAWN_LOOKUP_MISSING: fixture.spawnLookupMissing === true ? "1" : "0",
        },
      },
      ...(fixture.secondaryProviderJournalPath === undefined
        ? {}
        : {
            "fake-lifecycle-secondary": {
              command: [process.execPath, "--import", "tsx", fakeProvider],
              environment: {
                LIFECYCLE_FAKE_JOURNAL: fixture.secondaryProviderJournalPath,
                LIFECYCLE_FAKE_ADAPTER_ID: "fake-lifecycle-secondary",
                LIFECYCLE_FAKE_STATUS: fixture.providerStatus ?? "healthy",
                ...(fixture.providerStatusCallsPath === undefined
                  ? {}
                  : { LIFECYCLE_FAKE_STATUS_CALLS: fixture.providerStatusCallsPath }),
                LIFECYCLE_FAKE_MANDATORY_USAGE: fixture.mandatoryUsageUnits === true ? "1" : "0",
                LIFECYCLE_FAKE_PAYLOAD_MAX_TURNS: fixture.payloadMaxTurns === true ? "1" : "0",
              },
            },
          }),
    },
    ...(fixture.fault === undefined ? {} : { fault: fixture.fault }),
  };
}

export async function createLifecycleFixture(
  options: {
    capabilitiesDelayMs?: number;
    spawnDelayMs?: number;
    spawnBarrier?: boolean;
    mandatoryUsageUnits?: boolean;
    maximumConcurrentProviderTurns?: number;
    payloadMaxTurns?: boolean;
    spawnResultLost?: boolean;
    spawnUnresolved?: boolean;
    spawnLookupMissing?: boolean;
    fault?: (label: string) => void;
    secondaryAdapter?: boolean;
    retainedAgents?: boolean;
    retainedDaemonStarted?: (input: { pid: number; directory: string; release: () => void }) => Promise<void> | void;
  } = {},
): Promise<LifecycleFixture> {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-lifecycle-"));
  const runDirectory = join(directory, ".agent-run", "run-stage3");
  const databasePath = join(directory, "fabric.sqlite3");
  const providerJournalPath = join(directory, "fake-provider-journal.json");
  const secondaryProviderJournalPath = join(directory, "fake-provider-secondary-journal.json");
  const providerSessionMarker = join(directory, "provider-native-session.json");
  const providerSpawnBarrier = options.spawnBarrier === true
    ? {
        entered: join(directory, "fake-provider-spawn-barrier-entered"),
        release: join(directory, "fake-provider-spawn-barrier-release"),
      }
    : undefined;
  const clock = new ManualClock();
  await mkdir(join(directory, "src", "leader", "child"), { recursive: true });
  await mkdir(runDirectory, { recursive: true });
  await writeFile(providerSessionMarker, '{"provider":"fake","session":"leader"}\n');
  if (options.retainedAgents === true) {
    return await createRetainedLifecycleFixture({
      directory,
      runDirectory,
      databasePath,
      providerJournalPath,
      secondaryProviderJournalPath,
      ...(providerSpawnBarrier === undefined ? {} : { providerSpawnBarrier }),
      providerSessionMarker: "fake-session:leader:g1",
      clock,
      options,
    });
  }
  const fabric = await openFabric(adapterOptions({
    databasePath,
    directory,
    clock,
    providerJournalPath,
    ...(providerSpawnBarrier === undefined ? {} : { spawnBarrierPaths: providerSpawnBarrier }),
    ...(options.secondaryAdapter === true ? { secondaryProviderJournalPath } : {}),
    ...options,
  }));
  const rootAuthority = {
    ...TEST_AUTHORITY_V2_FIELDS,
    workspaceRoots: ["."],
    sourcePaths: ["src"],
    artifactPaths: [".agent-run/run-stage3"],
    actions: [...AUTHORITY_ACTION_VOCABULARY],
    disclosure: { level: "scoped", scopes: ["local", "approved-provider"] } as const,
    expiresAt: "2099-01-01T00:00:00.000Z",
    budget: {
      turns: 40,
      provider_calls: 40,
      concurrent_turns: 8,
      wall_clock_milliseconds: 1_000_000,
      "cost:USD": 20,
      "input_tokens:fake": 100_000,
      "output_tokens:fake": 100_000,
      descendants: 10,
      message_bytes: 1_000_000,
      artifact_bytes: 1_000_000,
    },
  };
  const run = await createCurrentSessionRun({
    databasePath,
    workspaceRoot: directory,
    runId: "run-stage3",
    projectRunDirectory: runDirectory,
    chair: { agentId: "chair", authority: rootAuthority },
  });
  const chairBase = fabric.connect(run.chairCapability);
  const leaderAuthority = await chairBase.delegateAuthority({
    parentAuthorityId: run.chairAuthorityId,
    authority: {
      ...rootAuthority,
      sourcePaths: ["src/leader"],
      actions: [...rootAuthority.actions],
      budget: { turns: 20, "cost:USD": 10 },
    },
  });
  const leaderRegistration = await chairBase.registerAgent({
    agentId: "leader",
    authorityId: leaderAuthority.authorityId,
    providerSessionRef: providerSessionMarker,
    adapterId: "fake-lifecycle",
  });
  const leaderBase = fabric.connect(leaderRegistration.capability);
  const childAuthority = await leaderBase.delegateAuthority({
    parentAuthorityId: leaderAuthority.authorityId,
    authority: {
      ...rootAuthority,
      sourcePaths: ["src/leader/child"],
      actions: [...rootAuthority.actions],
      budget: { turns: 5, "cost:USD": 2 },
    },
  });
  const childRegistration = await leaderBase.registerAgent({
    agentId: "child",
    authorityId: childAuthority.authorityId,
    providerSessionRef: "fake-session:child:g1",
    adapterId: "fake-lifecycle",
  });
  const childBase = fabric.connect(childRegistration.capability);
  const leaderTaskReady = await chairBase.createTask({
    taskId: "leader-task",
    authorityId: leaderAuthority.authorityId,
    participantAgentIds: ["chair", "leader"],
    eligibleAgentIds: ["leader"],
    objective: "own the Stage 3 lifecycle",
    baseRevision: "stage3-base",
    commandId: "stage3:create:leader-task",
  });
  const leaderTask = await leaderBase.claimTask({
    taskId: leaderTaskReady.taskId,
    expectedRevision: leaderTaskReady.revision,
    commandId: "stage3:claim:leader-task",
  });
  const childTaskReady = await leaderBase.createTask({
    taskId: "child-task",
    authorityId: childAuthority.authorityId,
    eligibleAgentIds: ["child"],
    objective: "perform bounded child work",
    baseRevision: "stage3-base",
    commandId: "stage3:create:child-task",
  });
  const childTask = await childBase.claimTask({
    taskId: childTaskReady.taskId,
    expectedRevision: childTaskReady.revision,
    commandId: "stage3:claim:child-task",
  });

  return {
    directory,
    runDirectory,
    databasePath,
    providerJournalPath,
    ...(providerSpawnBarrier === undefined
      ? {}
      : {
          providerSpawnBarrier: {
            waitUntilEntered: async () => await waitUntilFileExists(providerSpawnBarrier.entered),
            release: async () => await writeFile(providerSpawnBarrier.release, "released\n"),
          },
        }),
    ...(options.secondaryAdapter === true ? { secondaryProviderJournalPath } : {}),
    providerSessionMarker,
    clock,
    fabric,
    capabilities: {
      chair: run.chairCapability,
      leader: leaderRegistration.capability,
      child: childRegistration.capability,
    },
    chairAuthorityId: run.chairAuthorityId,
    rootAuthority,
    chair: asLifecycleClient(chairBase),
    leader: asLifecycleClient(leaderBase),
    child: asLifecycleClient(childBase),
    runId: run.runId,
    leaderTask,
    childTask,
  };
}

/**
 * Wait for a barrier file: `fs.watch` for latency, the deadline port for safety.
 *
 * The watch alone could hang forever on a missed event, with no diagnostic
 * beyond Vitest's outer timeout. Racing it against `waitUntil` keeps the fast
 * path, catches a missed event on the next poll, and fails a file that never
 * appears with a DeadlineTimeoutError naming it.
 *
 * The budget sits inside Vitest's 15s test timeout with headroom to spare, so
 * the diagnostic reaches the report, but stays loose enough that a loaded
 * machine does not turn a slow barrier into a failure.
 */
async function waitUntilFileExists(path: string, timeoutMs = 10_000): Promise<void> {
  if (existsSync(path)) return;
  const watcher = watch(dirname(path));
  try {
    await Promise.race([
      new Promise<true>((resolvePromise, reject) => {
        watcher.on("change", () => { if (existsSync(path)) resolvePromise(true); });
        watcher.once("error", reject);
        if (existsSync(path)) resolvePromise(true);
      }),
      waitUntil(() => existsSync(path), timeoutMs, `Barrier file ${path}`),
    ]);
  } finally {
    watcher.close();
  }
}

function mcpFailure(outcome: Awaited<ReturnType<typeof callTool>>): Error & { code?: string } {
  const code = typeof outcome.structured.code === "string" ? outcome.structured.code : undefined;
  return Object.assign(new Error(outcome.text || code || "MCP test operation failed"), code === undefined ? {} : { code });
}

async function callMcpResult(
  proxy: McpProxy,
  name: `fabric_${string}`,
  input: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  const outcome = await callTool(proxy.client, name, input);
  if (outcome.isError) throw mcpFailure(outcome);
  return outcome.structured;
}

function retainedAgentClient(input: {
  chair: {
    dispatchProviderAction(
      value: Parameters<FabricClient["dispatchProviderAction"]>[0],
    ): Promise<ProviderActionResult>;
  };
  adapterId: "fake-lifecycle" | "fake-lifecycle-secondary";
  agentId: "leader" | "child";
}): FabricClient {
  let sequence = 0;
  const retainedCall = async (operation: string, operationInput: Record<string, unknown>): Promise<unknown> => {
    sequence += 1;
    const taskId = operation === "createTask"
      ? `${input.agentId}-task`
      : typeof operationInput.taskId === "string"
      ? operationInput.taskId
      : operation === "receiveMessages"
        ? `${input.agentId}-task`
        : undefined;
    const actionIdentity = operation === "requestLifecycle" && typeof operationInput.commandId === "string"
      ? `lifecycle:${operationInput.commandId}`
      : String(sequence);
    const action = await input.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: input.adapterId,
      actionId: `retained-test:${input.agentId}:${actionIdentity}`,
      operation: "send_turn",
      payload: {
        agentId: input.agentId,
        providerSessionGeneration: 1,
        ...(taskId === undefined ? {} : { taskId }),
        scenario: "retained-test-action",
        retainedAction: { operation, input: operationInput },
      },
      commandId: `retained-test:${input.agentId}:${actionIdentity}:dispatch`,
    });
    if (
      action.status !== "terminal" ||
      typeof action.result !== "object" || action.result === null ||
      !("retainedActionResult" in action.result)
    ) throw new Error(`retained ${input.agentId} ${operation} did not return a terminal result`);
    return (action.result as { retainedActionResult: unknown }).retainedActionResult;
  };
  return {
    acquireWriteLease: async (value: Record<string, unknown>) => await retainedCall(
      "acquireWriteLease",
      { taskId: `${input.agentId}-task`, ...value },
    ),
    attachAgent: async (value: Record<string, unknown>) => await retainedCall("attachAgent", value),
    claimTask: async (value: Record<string, unknown>) => await retainedCall("claimTask", value),
    createTask: async (value: Record<string, unknown>) => await retainedCall("createTask", value),
    delegateAuthority: async (value: Record<string, unknown>) => await retainedCall("delegateAuthority", value),
    receiveMessages: async (value: Record<string, unknown>) => await retainedCall("receiveMessages", value),
    requestLifecycle: async (value: Record<string, unknown>) => await retainedCall("requestLifecycle", value),
  } as unknown as FabricClient;
}

async function createRetainedLifecycleFixture(input: {
  directory: string;
  runDirectory: string;
  databasePath: string;
  providerJournalPath: string;
  secondaryProviderJournalPath: string;
  providerSpawnBarrier?: { entered: string; release: string };
  providerSessionMarker: string;
  clock: ManualClock;
  options: {
    capabilitiesDelayMs?: number;
    spawnDelayMs?: number;
    mandatoryUsageUnits?: boolean;
    maximumConcurrentProviderTurns?: number;
    payloadMaxTurns?: boolean;
    spawnResultLost?: boolean;
    spawnUnresolved?: boolean;
    spawnLookupMissing?: boolean;
    fault?: (label: string) => void;
    retainedDaemonStarted?: (input: { pid: number; directory: string; release: () => void }) => Promise<void> | void;
  };
}): Promise<LifecycleFixture> {
  const providerSpawnBarrier = input.providerSpawnBarrier;
  await mkdir(join(input.directory, "leader", "child"), { recursive: true });
  const stateDirectory = join(input.directory, "state");
  const runtimeDirectory = join(input.directory, "runtime");
  const socketPath = join(runtimeDirectory, "fabric.sock");
  const environment = (journalPath: string, adapterId: string): Record<string, string> => ({
    LIFECYCLE_FAKE_JOURNAL: journalPath,
    LIFECYCLE_FAKE_ADAPTER_ID: adapterId,
    ...(input.options.capabilitiesDelayMs === undefined
      ? {}
      : { LIFECYCLE_FAKE_CAPABILITIES_DELAY_MS: String(input.options.capabilitiesDelayMs) }),
    ...(input.options.spawnDelayMs === undefined
      ? {}
      : { LIFECYCLE_FAKE_SPAWN_DELAY_MS: String(input.options.spawnDelayMs) }),
    ...(providerSpawnBarrier === undefined
      ? {}
      : {
          LIFECYCLE_FAKE_SPAWN_BARRIER_ENTERED: providerSpawnBarrier.entered,
          LIFECYCLE_FAKE_SPAWN_BARRIER_RELEASE: providerSpawnBarrier.release,
        }),
    LIFECYCLE_FAKE_MANDATORY_USAGE: input.options.mandatoryUsageUnits === true ? "1" : "0",
    LIFECYCLE_FAKE_PAYLOAD_MAX_TURNS: input.options.payloadMaxTurns === true ? "1" : "0",
    LIFECYCLE_FAKE_SPAWN_RESULT_LOST: input.options.spawnResultLost === true ? "1" : "0",
    LIFECYCLE_FAKE_SPAWN_UNRESOLVED: input.options.spawnUnresolved === true ? "1" : "0",
    LIFECYCLE_FAKE_SPAWN_LOOKUP_MISSING: input.options.spawnLookupMissing === true ? "1" : "0",
  });
  let inProcessFabric: Fabric | undefined;
  let protocolServer: Server | undefined;
  const daemonOptions = {
    databasePath: input.databasePath,
    stateDirectory,
    runtimeDirectory,
    socketPath,
    workspaceRoots: [input.directory],
    ...(input.options.maximumConcurrentProviderTurns === undefined
      ? {}
      : { maximumConcurrentProviderTurns: input.options.maximumConcurrentProviderTurns }),
    adapters: {
      "fake-lifecycle": {
        command: [process.execPath, "--import", "tsx", fakeProvider],
        environment: environment(input.providerJournalPath, "fake-lifecycle"),
      },
      "fake-lifecycle-secondary": {
        command: [process.execPath, "--import", "tsx", fakeProvider],
        environment: environment(input.secondaryProviderJournalPath, "fake-lifecycle-secondary"),
      },
    },
  };
  const daemon = input.options.fault === undefined
    ? await startFabricDaemon(daemonOptions)
    : undefined;
  if (daemon !== undefined) trackTestProcess(daemon.pid, `retained lifecycle daemon ${input.directory}`);
  if (input.options.fault !== undefined) {
    await Promise.all([
      mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
      mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
    ]);
    inProcessFabric = await openFabric({
      ...adapterOptions({
        databasePath: input.databasePath,
        directory: input.directory,
        clock: input.clock,
        providerJournalPath: input.providerJournalPath,
        secondaryProviderJournalPath: input.secondaryProviderJournalPath,
        ...(input.options.maximumConcurrentProviderTurns === undefined
          ? {}
          : { maximumConcurrentProviderTurns: input.options.maximumConcurrentProviderTurns }),
        fault: input.options.fault,
      }),
      fabricSocketPath: socketPath,
    });
    const localFabric = inProcessFabric;
    protocolServer = createServer((socket) => {
      servePublicProtocolConnection(socket, {
        daemonVersion: "lifecycle-checkpoint-fixture",
        daemonInstanceGeneration: 1,
        offeredFeatures: PROTOCOL_FEATURES,
        limits: PROTOCOL_LIMITS,
        verifyCredential: (credential) => localFabric.verifyProtocolCredential(credential),
        dispatch: async (protocolContext, operation, value) =>
          await localFabric.dispatchPublicProtocol(protocolContext, operation, value),
      });
    });
    await new Promise<void>((resolve, reject) => {
      protocolServer?.once("error", reject);
      protocolServer?.listen(socketPath, () => {
        protocolServer?.off("error", reject);
        resolve();
      });
    });
  }
  let remoteChair: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let chairProxy: McpProxy | undefined;
  let closed = false;
  const close = async (): Promise<void> => {
    if (closed) return;
    closed = true;
    await Promise.allSettled([chairProxy?.close() ?? Promise.resolve(), remoteChair?.close() ?? Promise.resolve()]);
    await (daemon === undefined ? inProcessFabric?.close() ?? Promise.resolve() : stopOwnedDaemon(daemon));
    if (protocolServer !== undefined) {
      await new Promise<void>((resolve) => protocolServer?.close(() => resolve()));
    }
  };
  try {
    if (daemon !== undefined) {
      await input.options.retainedDaemonStarted?.({
        pid: daemon.pid,
        directory: input.directory,
        release: () => daemon.release(),
      });
    }
    const rootAuthority = {
      ...TEST_AUTHORITY_V2_FIELDS,
      workspaceRoots: ["."],
      sourcePaths: ["."],
      artifactPaths: [".agent-run/run-stage3"],
      actions: [...AUTHORITY_ACTION_VOCABULARY],
      disclosure: { level: "scoped", scopes: ["local", "approved-provider"] } as const,
      expiresAt: "2099-01-01T00:00:00.000Z",
      budget: {
        turns: 80,
        provider_calls: 80,
        concurrent_turns: 8,
        wall_clock_milliseconds: 1_000_000,
        "cost:USD": 40,
        descendants: 10,
        message_bytes: 1_000_000,
        artifact_bytes: 1_000_000,
      },
    };
    const run = await createCurrentSessionRun({
      databasePath: input.databasePath,
      workspaceRoot: input.directory,
      runId: "run-stage3",
      projectRunDirectory: input.runDirectory,
      chair: { agentId: "chair", authority: rootAuthority },
    });
    remoteChair = daemon === undefined
      ? undefined
      : await connectFabricDaemon({ socketPath, capability: run.chairCapability });
    const directChair = inProcessFabric?.connect(run.chairCapability);
    const chair = remoteChair ?? directChair;
    if (chair === undefined) throw new Error("retained Stage 3 chair is unavailable");
    chairProxy = daemon === undefined
      ? undefined
      : await spawnMcpProxy({ socketPath, capability: run.chairCapability, label: "retained-stage3-chair" });
    const retainedChair = {
      dispatchProviderAction: async (
        value: Parameters<FabricClient["dispatchProviderAction"]>[0],
      ): Promise<ProviderActionResult> => await chair.dispatchProviderAction(value) as ProviderActionResult,
    };
    const leaderAuthority = await chair.delegateAuthority({
      parentAuthorityId: run.chairAuthorityId,
      authority: { ...rootAuthority, sourcePaths: ["leader"], budget: { turns: 30, provider_calls: 30 } },
      commandId: "stage3:delegate:leader",
    });
    const leaderAttachInput = {
      agentId: "leader",
      authorityId: leaderAuthority.authorityId,
      adapterId: "fake-lifecycle",
      actionId: "stage3:attach:leader",
      providerSessionRef: input.providerSessionMarker,
    };
    const leaderAttached = directChair !== undefined
      ? await directChair.attachAgent(leaderAttachInput)
      : (await callTool((chairProxy as McpProxy).client, "fabric_agent_attach", leaderAttachInput)).structured;
    if (leaderAttached.bridgeState !== "active") {
      throw new Error("retained Stage 3 leader attach failed");
    }
    const leader = retainedAgentClient({
      chair: retainedChair,
      adapterId: "fake-lifecycle",
      agentId: "leader",
    });
    const childAuthority = await leader.delegateAuthority({
      parentAuthorityId: leaderAuthority.authorityId,
      authority: { ...rootAuthority, sourcePaths: ["leader/child"], budget: { turns: 15, provider_calls: 15 } },
      commandId: "stage3:delegate:child",
    });
    const childAttached = await leader.attachAgent({
      agentId: "child",
      authorityId: childAuthority.authorityId,
      adapterId: "fake-lifecycle-secondary",
      actionId: "stage3:attach:child",
      providerSessionRef: "fake-session:child:g1",
    });
    if (childAttached.bridgeState !== "active") {
      throw new Error("retained Stage 3 child attach did not activate its bridge");
    }
    const child = retainedAgentClient({
      chair: retainedChair,
      adapterId: "fake-lifecycle-secondary",
      agentId: "child",
    });
    const leaderTaskInput = {
      taskId: "leader-task",
      authorityId: leaderAuthority.authorityId,
      participantAgentIds: ["chair", "leader"],
      eligibleAgentIds: ["leader"],
      objective: "own the Stage 3 lifecycle",
      baseRevision: "stage3-base",
      commandId: "stage3:create:leader-task",
    };
    const leaderTaskReady = directChair !== undefined
      ? await directChair.createTask(leaderTaskInput)
      : await callMcpResult(chairProxy as McpProxy, "fabric_task_create", leaderTaskInput);
    const leaderTask = await leader.claimTask({
      taskId: "leader-task",
      expectedRevision: leaderTaskReady.revision as number,
      commandId: "stage3:claim:leader-task",
    });
    const childTaskReady = await leader.createTask({
      taskId: "child-task",
      authorityId: childAuthority.authorityId,
      participantAgentIds: ["leader", "child"],
      eligibleAgentIds: ["child"],
      objective: "perform bounded child work",
      baseRevision: "stage3-base",
      commandId: "stage3:create:child-task",
    });
    const childTask = await child.claimTask({
      taskId: "child-task",
      expectedRevision: childTaskReady.revision as number,
      commandId: "stage3:claim:child-task",
    });
    const proxy = chairProxy;
    const chairClient = proxy === undefined ? directChair as FabricClient : {
      sendMessage: async (value: Record<string, unknown>) => await callMcpResult(proxy, "fabric_message_send", value),
      reportProviderState: async (value: Record<string, unknown>) =>
        await callMcpResult(proxy, "fabric_provider_state_report", value),
      dispatchProviderAction: async (value: Parameters<FabricClient["dispatchProviderAction"]>[0]) =>
        await chair.dispatchProviderAction(value),
      closeBarrier: async (value: Record<string, unknown>) => await callMcpResult(proxy, "fabric_barrier_close", value),
      getAgentLifecycle: async (value: Record<string, unknown>) =>
        await callMcpResult(proxy, "fabric_lifecycle_read", value),
      getWriteLease: async (value: Record<string, unknown>) => await callMcpResult(proxy, "fabric_write_lease_read", value),
      recordVisibilityFailure: async (value: Record<string, unknown>) =>
        await callMcpResult(proxy, "fabric_visibility_failure_record", value),
    } as unknown as FabricClient;
    return {
      directory: input.directory,
      runDirectory: input.runDirectory,
      databasePath: input.databasePath,
      providerJournalPath: input.providerJournalPath,
      secondaryProviderJournalPath: input.secondaryProviderJournalPath,
      ...(providerSpawnBarrier === undefined
        ? {}
        : {
            providerSpawnBarrier: {
              waitUntilEntered: async () => await waitUntilFileExists(providerSpawnBarrier.entered),
              release: async () => await writeFile(providerSpawnBarrier.release, "released\n"),
            },
          }),
      providerSessionMarker: input.providerSessionMarker,
      clock: input.clock,
      fabric: { close } as unknown as Fabric,
      capabilities: { chair: run.chairCapability, leader: "", child: "" },
      chairAuthorityId: run.chairAuthorityId,
      rootAuthority,
      chair: chairClient,
      leader,
      child,
      runId: run.runId,
      leaderTask: leaderTask as { taskId: string; revision: number },
      childTask: childTask as { taskId: string; revision: number },
      ...(daemon === undefined ? {} : {
        restartRetainedDaemon: async (): Promise<{ fabric: Fabric; chair: FabricClient }> => {
          const restartedDaemon = await startFabricDaemon(daemonOptions);
          trackTestProcess(restartedDaemon.pid, `retained lifecycle restart daemon ${input.directory}`);
          let restartedChair: Awaited<ReturnType<typeof connectFabricDaemon>>;
          try {
            restartedChair = await connectFabricDaemon({
              socketPath,
              capability: run.chairCapability,
            });
          } catch (error: unknown) {
            await stopOwnedDaemon(restartedDaemon);
            throw error;
          }
          let restartClosed = false;
          return {
            chair: restartedChair as unknown as FabricClient,
            fabric: {
              close: async () => {
                if (restartClosed) return;
                restartClosed = true;
                await restartedChair.close();
                await stopOwnedDaemon(restartedDaemon);
              },
            } as unknown as Fabric,
          };
        },
      }),
    };
  } catch (error: unknown) {
    try {
      await close();
    } finally {
      await rm(input.directory, { recursive: true, force: true });
    }
    throw error;
  }
}

export async function reopenLifecycleFabric(
  fixture: LifecycleFixture,
  options: {
    providerStatus?: "healthy" | "unmanaged" | "missing-evidence";
    providerStatusCallsPath?: string;
    fabricSocketPath?: string;
    lifecycleReceiptAuthority?: LifecycleIntegrityReceiptAuthorityPort;
  } = {},
): Promise<Fabric> {
  return await openFabric({
    ...adapterOptions({
      databasePath: fixture.databasePath,
      directory: fixture.directory,
      clock: fixture.clock,
      providerJournalPath: fixture.providerJournalPath,
      ...(fixture.secondaryProviderJournalPath === undefined
        ? {}
        : { secondaryProviderJournalPath: fixture.secondaryProviderJournalPath }),
      ...(options.providerStatus === undefined ? {} : { providerStatus: options.providerStatus }),
      ...(options.providerStatusCallsPath === undefined
        ? {}
        : { providerStatusCallsPath: options.providerStatusCallsPath }),
    }),
    ...(options.fabricSocketPath === undefined ? {} : { fabricSocketPath: options.fabricSocketPath }),
    ...(options.lifecycleReceiptAuthority === undefined
      ? {}
      : { lifecycleReceiptAuthority: options.lifecycleReceiptAuthority }),
  });
}

export type RetainedLifecycleCallbackFixture = {
  directory: string;
  databasePath: string;
  providerJournalPath: string;
  runId: string;
  childAgentId: string;
  task: { taskId: string; revision: number };
  checkpoint: LifecycleCheckpoint;
  chair: Awaited<ReturnType<typeof connectFabricDaemon>>;
  dispatchLifecycleCallback(): Promise<ProviderActionResult>;
  close(): Promise<void>;
};

export async function createRetainedLifecycleCallbackFixture(): Promise<RetainedLifecycleCallbackFixture> {
  const directory = await mkdtemp(join(tmpdir(), "af-rl-"));
  const stateDirectory = join(directory, "s");
  const runtimeDirectory = join(directory, "r");
  const runDirectory = join(directory, ".agent-run", "run-retained-lifecycle");
  const databasePath = join(stateDirectory, "fabric.sqlite3");
  const providerJournalPath = join(directory, "retained-lifecycle-provider.json");
  const socketPath = join(runtimeDirectory, "f.sock");
  await mkdir(join(directory, "src", "retained-child"), { recursive: true });
  await mkdir(join(runDirectory, "checkpoints"), { recursive: true });
  const daemon = await startFabricDaemon({
    databasePath,
    stateDirectory,
    runtimeDirectory,
    socketPath,
    workspaceRoots: [directory],
    adapters: {
      "fake-lifecycle": {
        command: [process.execPath, "--import", "tsx", fakeProvider],
        environment: { LIFECYCLE_FAKE_JOURNAL: providerJournalPath },
      },
    },
  });
  trackTestProcess(daemon.pid, `retained lifecycle callback daemon ${directory}`);
  let bootstrap: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let chair: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let chairProxy: Awaited<ReturnType<typeof spawnMcpProxy>> | undefined;
  let closed = false;
  const close = async (): Promise<void> => {
    if (closed) return;
    closed = true;
    await Promise.allSettled([
      chairProxy?.close() ?? Promise.resolve(),
      chair?.close() ?? Promise.resolve(),
      bootstrap?.close() ?? Promise.resolve(),
    ]);
    try {
      await stopOwnedDaemon(daemon);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  };
  try {
    bootstrap = await connectFabricDaemon({ socketPath, capability: daemon.bootstrapCapability });
    const rootAuthority = {
      ...TEST_AUTHORITY_V2_FIELDS,
      workspaceRoots: ["."],
      sourcePaths: ["src"],
      artifactPaths: [".agent-run/run-retained-lifecycle"],
      actions: [...AUTHORITY_ACTION_VOCABULARY],
      disclosure: { level: "scoped", scopes: ["local", "approved-provider"] } as const,
      expiresAt: "2099-01-01T00:00:00.000Z",
      budget: {
        turns: 20,
        provider_calls: 20,
        concurrent_turns: 4,
        wall_clock_milliseconds: 1_000_000,
        descendants: 2,
        message_bytes: 100_000,
        artifact_bytes: 100_000,
      },
    };
    const run = await createCurrentSessionRun({
      databasePath,
      workspaceRoot: directory,
      runId: "run-retained-lifecycle",
      projectRunDirectory: runDirectory,
      chair: { agentId: "chair", authority: rootAuthority },
    });
    chair = await connectFabricDaemon({ socketPath, capability: run.chairCapability });
    chairProxy = await spawnMcpProxy({
      socketPath,
      capability: run.chairCapability,
      label: "retained-lifecycle-chair",
    });
    const childAuthority = await chair.delegateAuthority({
      parentAuthorityId: run.chairAuthorityId,
      authority: {
        ...rootAuthority,
        sourcePaths: ["src/retained-child"],
        budget: { turns: 10, provider_calls: 10, concurrent_turns: 2 },
      },
      commandId: "retained-lifecycle:delegate",
    });
    const attached = await callTool(chairProxy.client, "fabric_agent_attach", {
      agentId: "retained-child",
      authorityId: childAuthority.authorityId,
      adapterId: "fake-lifecycle",
      actionId: "retained-lifecycle:attach",
      providerSessionRef: "fake-session:retained-child:g1",
    });
    if (attached.isError || attached.structured.bridgeState !== "active") {
      throw new Error(`retained lifecycle attach failed: ${attached.text}`);
    }
    const createdTask = await callTool(chairProxy.client, "fabric_task_create", {
      taskId: "retained-lifecycle-task",
      authorityId: childAuthority.authorityId,
      eligibleAgentIds: ["retained-child"],
      participantAgentIds: ["chair", "retained-child"],
      objective: "exercise lifecycle from the retained provider turn",
      baseRevision: "retained-lifecycle-base",
      commandId: "retained-lifecycle:create-task",
    });
    const revision = createdTask.structured.revision;
    if (createdTask.isError || typeof revision !== "number") {
      throw new Error(`retained lifecycle task creation failed: ${createdTask.text}`);
    }
    const relativePath = join("checkpoints", "retained-child.json");
    const document = {
      schemaVersion: 1,
      agentId: "retained-child",
      mailboxWatermark: 0,
      acknowledgedAboveWatermark: [],
      inFlightChildren: [],
      openWork: ["retained-lifecycle-task"],
      nextAction: "continue from the retained provider lifecycle callback",
      providerResumeReference: "fake-session:retained-child:g1",
    };
    const bytes = `${JSON.stringify(document, null, 2)}\n`;
    await writeFile(join(runDirectory, relativePath), bytes, { mode: 0o600 });
    const checkpoint: LifecycleCheckpoint = {
      relativePath,
      sha256: createHash("sha256").update(bytes).digest("hex"),
      mailboxWatermark: 0,
      acknowledgedAboveWatermark: [],
      inFlightChildren: [],
      openWork: ["retained-lifecycle-task"],
      nextAction: document.nextAction,
      providerResumeReference: document.providerResumeReference,
    };
    const dispatchLifecycleCallback = async (): Promise<ProviderActionResult> => {
      return await chair!.dispatchProviderAction({
        certifyingReview: null,
        adapterId: "fake-lifecycle",
        actionId: "retained-lifecycle:send-turn",
        operation: "send_turn",
        payload: {
          agentId: "retained-child",
          providerSessionGeneration: 1,
          taskId: "retained-lifecycle-task",
          cwd: "src/retained-child",
          model: "claude-opus-current",
          modelFamily: "anthropic",
          effort: "high",
          instruction: "claim the task and rotate from this retained provider turn",
          scenario: "retained-lifecycle-callback",
          lifecycleRequest: {
            action: "rotate",
            taskId: "retained-lifecycle-task",
            expectedTaskRevision: revision,
            checkpoint,
            commandId: "retained-lifecycle:rotate",
          },
        },
        commandId: "retained-lifecycle:dispatch",
      }) as ProviderActionResult;
    };
    return {
      directory,
      databasePath,
      providerJournalPath,
      runId: run.runId,
      childAgentId: "retained-child",
      task: { taskId: "retained-lifecycle-task", revision },
      checkpoint,
      chair,
      dispatchLifecycleCallback,
      close,
    };
  } catch (error: unknown) {
    await close();
    throw error;
  }
}

export async function writeLifecycleCheckpoint(
  fixture: LifecycleFixture,
  options: {
    agentId: "leader" | "child";
    inFlightChildren?: string[];
    openWork?: string[];
    nextAction?: string;
    providerResumeReference?: string;
  },
): Promise<LifecycleCheckpoint> {
  const relativePath = join("checkpoints", `${options.agentId}-${randomUUID()}.json`);
  const absolutePath = join(fixture.runDirectory, relativePath);
  await mkdir(join(fixture.runDirectory, "checkpoints"), { recursive: true });
  const document = {
    schemaVersion: 1,
    agentId: options.agentId,
    mailboxWatermark: 0,
    acknowledgedAboveWatermark: [],
    inFlightChildren: options.inFlightChildren ?? [],
    openWork: options.openWork ?? [],
    nextAction: options.nextAction ?? "release",
    providerResumeReference: options.providerResumeReference ??
      (options.agentId === "leader" ? fixture.providerSessionMarker : "fake-session:child:g1"),
  };
  const bytes = `${JSON.stringify(document, null, 2)}\n`;
  await writeFile(absolutePath, bytes, { mode: 0o600 });
  return {
    relativePath,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    mailboxWatermark: document.mailboxWatermark,
    acknowledgedAboveWatermark: document.acknowledgedAboveWatermark,
    inFlightChildren: document.inFlightChildren,
    openWork: document.openWork,
    nextAction: document.nextAction,
    providerResumeReference: document.providerResumeReference,
  };
}
