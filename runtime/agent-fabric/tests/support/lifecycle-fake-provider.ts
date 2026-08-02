import { createHash, randomUUID } from "node:crypto";
import { appendFileSync, existsSync, readFileSync, watch, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { dirname } from "node:path";
import { createInterface } from "node:readline";

import {
  FABRIC_OPERATIONS,
  NdjsonRpcTransport,
  type LifecycleCheckpoint,
} from "@local/agent-fabric-protocol";

import { writeJsonFileAtomic } from "./atomic-json-file.ts";

const journalPath = process.env.LIFECYCLE_FAKE_JOURNAL;
if (journalPath === undefined) {
  throw new Error("LIFECYCLE_FAKE_JOURNAL is required");
}
const requiredJournalPath: string = journalPath;
const spawnDelayMs = Number(process.env.LIFECYCLE_FAKE_SPAWN_DELAY_MS ?? "0");
const spawnBarrierEnteredPath = process.env.LIFECYCLE_FAKE_SPAWN_BARRIER_ENTERED;
const spawnBarrierReleasePath = process.env.LIFECYCLE_FAKE_SPAWN_BARRIER_RELEASE;
const adapterId = process.env.LIFECYCLE_FAKE_ADAPTER_ID ?? "fake-lifecycle";
if ((spawnBarrierEnteredPath === undefined) !== (spawnBarrierReleasePath === undefined)) {
  throw new Error("lifecycle fake spawn barrier requires entered and release paths");
}

type Action = {
  actionId: string;
  payloadHash: string;
  status: string;
  history: string[];
  executionCount: number;
  effectCount: number;
  idempotencyProven: boolean;
  result?: unknown;
  scenario?: string;
  lookupCount?: number;
};

type Journal = {
  schemaVersion: 1;
  actions: Record<string, Action>;
  sessions: Record<string, { released: boolean; generation: number; spawnRequests?: number }>;
};

const bridgeContract = {
  schemaVersion: 1,
  method: "provision_agent",
  operations: ["spawn", "attach"],
  secretTransport: "private-handoff",
  bridgeContract: "agent-fabric-session-bridge-v1",
  generationBound: true,
  providerOriginatedActivation: true,
};

let retainedProtocol: Awaited<ReturnType<typeof NdjsonRpcTransport.connect>> | undefined;
let retainedBinding: Readonly<{
  actionId: string;
  agentId: string;
  projectSessionId: string;
  runId: string;
  principalGeneration: number;
  providerSessionRef: string;
  providerSessionGeneration: number;
  bridgeGeneration: number;
}> | undefined;

type Request = { id: string; method: string; params: Record<string, unknown> };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isJournal(value: unknown): value is Journal {
  return isRecord(value) && value.schemaVersion === 1 && isRecord(value.actions) && isRecord(value.sessions);
}

function loadJournal(): Journal {
  if (!existsSync(requiredJournalPath)) {
    return { schemaVersion: 1, actions: {}, sessions: {} };
  }
  const value: unknown = JSON.parse(readFileSync(requiredJournalPath, "utf8"));
  if (!isJournal(value)) throw new Error("fake provider journal is invalid");
  return value;
}

function saveJournal(journal: Journal): void {
  writeJsonFileAtomic(requiredJournalPath, `${JSON.stringify(journal, null, 2)}\n`);
}

function payloadHash(value: unknown): string {
  return createHash("sha256").update(JSON.stringify(value)).digest("hex");
}

function respond(id: string, result: unknown): void {
  process.stdout.write(`${JSON.stringify({ id, result })}\n`);
}

function fail(id: string, code: string, message: string): void {
  process.stdout.write(`${JSON.stringify({ id, error: { code, message } })}\n`);
}

function afterSpawnBarrier(complete: () => void): void {
  if (spawnBarrierEnteredPath === undefined || spawnBarrierReleasePath === undefined) {
    complete();
    return;
  }
  writeFileSync(spawnBarrierEnteredPath, "entered\n", { mode: 0o600 });
  if (existsSync(spawnBarrierReleasePath)) {
    complete();
    return;
  }
  const watcher = watch(dirname(spawnBarrierReleasePath), () => {
    if (!existsSync(spawnBarrierReleasePath)) return;
    watcher.close();
    complete();
  });
  if (existsSync(spawnBarrierReleasePath)) {
    watcher.close();
    complete();
  }
}

function actionFor(request: Request, journal: Journal): Action | undefined {
  const actionId = request.params.actionId;
  return typeof actionId === "string" ? journal.actions[actionId] : undefined;
}

async function provisionRetainedAgent(params: Record<string, unknown>): Promise<Record<string, unknown>> {
  const capability = process.env.AGENT_FABRIC_CAPABILITY;
  const socketPath = process.env.AGENT_FABRIC_SOCKET_PATH;
  if (capability === undefined || socketPath === undefined) throw new Error("private child handoff missing");
  if (params.operation !== "attach" && params.operation !== "spawn") {
    throw new Error("lifecycle fake bridge supports attach and spawn only");
  }
  const socket = createConnection(socketPath);
  retainedProtocol = await NdjsonRpcTransport.connect(socket, {
    protocolVersion: 1,
    client: { name: "lifecycle-fake-provider", version: "1.0.0" },
    authentication: {
      scheme: "capability",
      credential: capability,
      clientNonce: `fake_${randomUUID()}`,
    },
    expectedPrincipalKind: "agent",
    requiredFeatures: ["fabric-core.v1"],
    optionalFeatures: [],
  });
  const expectedPrincipal = {
    agentId: process.env.AGENT_FABRIC_EXPECTED_AGENT_ID,
    projectSessionId: process.env.AGENT_FABRIC_EXPECTED_PROJECT_SESSION_ID,
    runId: process.env.AGENT_FABRIC_EXPECTED_RUN_ID,
    principalGeneration: Number(process.env.AGENT_FABRIC_EXPECTED_PRINCIPAL_GENERATION),
  };
  if (
    retainedProtocol.principal.kind !== "agent" ||
    retainedProtocol.principal.agentId !== expectedPrincipal.agentId ||
    retainedProtocol.principal.projectSessionId !== expectedPrincipal.projectSessionId ||
    retainedProtocol.principal.runId !== expectedPrincipal.runId ||
    retainedProtocol.principal.principalGeneration !== expectedPrincipal.principalGeneration ||
    expectedPrincipal.agentId !== params.targetAgentId ||
    expectedPrincipal.runId !== params.runId ||
    (params.operation === "attach" && typeof params.providerSessionRef !== "string")
  ) throw new Error("private child principal binding changed");
  const payload = isRecord(params.payload) ? params.payload : {};
  const requestedGeneration = payload.targetProviderGeneration ?? payload.generation;
  const reservedProviderSessionGeneration = typeof requestedGeneration === "number" && Number.isSafeInteger(requestedGeneration)
    ? requestedGeneration
    : params.operation === "attach" ? 1 : 2;
  const providerSessionGeneration = process.env.LIFECYCLE_FAKE_WRONG_PROVIDER_GENERATION === "1" &&
    params.operation === "spawn"
    ? reservedProviderSessionGeneration + 1
    : reservedProviderSessionGeneration;
  const providerSessionRef = typeof params.providerSessionRef === "string"
    ? params.providerSessionRef
    : `fake-session:${String(params.targetAgentId)}:g${String(providerSessionGeneration)}:replacement`;
  const activation = await retainedProtocol.call(FABRIC_OPERATIONS.getMailboxState, {});
  const challenge = process.env.AGENT_FABRIC_ATTESTATION_CHALLENGE;
  const challengeDigest = process.env.AGENT_FABRIC_ATTESTATION_CHALLENGE_DIGEST;
  const custodyId = process.env.AGENT_FABRIC_LIFECYCLE_CUSTODY_ID;
  const checkpointDigest = process.env.AGENT_FABRIC_LIFECYCLE_CHECKPOINT_DIGEST;
  if (challenge !== undefined && (
    !/^[0-9a-f]{64}$/u.test(challenge) ||
    `sha256:${createHash("sha256").update(Buffer.from(challenge, "hex")).digest("hex")}` !== challengeDigest ||
    typeof custodyId !== "string" || typeof checkpointDigest !== "string"
  )) throw new Error("lifecycle launch attestation handoff is invalid");
  if (challenge !== undefined && process.env.LIFECYCLE_FAKE_REFLECT_CHALLENGE_ERROR === "1") {
    const canaryPath = process.env.LIFECYCLE_FAKE_CHALLENGE_CANARY;
    if (canaryPath === undefined) throw new Error("lifecycle challenge canary path is required");
    writeFileSync(canaryPath, `${challenge}\n`, { mode: 0o600 });
    throw new Error(`provider reflected private lifecycle challenge ${challenge}`);
  }
  const unsignedAttestation = challenge === undefined ? undefined : {
    schemaVersion: 1 as const,
    kind: "provider-session-lifecycle-attestation" as const,
    custodyId,
    actionId: String(params.actionId),
    checkpointDigest,
    challengeDigest,
    providerSessionRef,
    providerSessionGeneration,
    bridgeGeneration: Number(params.bridgeGeneration),
    providerTurnRef: `fake-turn:${String(params.actionId)}`,
    providerInvocationRef: `fake-invocation:${String(params.actionId)}`,
  };
  const lifecycleAttestation = unsignedAttestation === undefined ? undefined : {
    ...unsignedAttestation,
    attestationDigest: `sha256:${createHash("sha256").update(JSON.stringify(unsignedAttestation)).digest("hex")}`,
  };
  retainedBinding = {
    actionId: String(params.actionId),
    agentId: expectedPrincipal.agentId,
    projectSessionId: expectedPrincipal.projectSessionId,
    runId: expectedPrincipal.runId,
    principalGeneration: expectedPrincipal.principalGeneration,
    providerSessionRef,
    providerSessionGeneration,
    bridgeGeneration: Number(params.bridgeGeneration),
  };
  const result: Record<string, unknown> = {
    schemaVersion: 1,
    adapterId,
    actionId: params.actionId,
    targetAgentId: params.targetAgentId,
    providerSessionRef,
    providerSessionGeneration,
    bridgeGeneration: params.bridgeGeneration,
    bridgeContractDigest: params.bridgeContractDigest,
    activationEvidenceDigest: lifecycleAttestation?.attestationDigest ??
      `sha256:${createHash("sha256").update(JSON.stringify({
        actionId: params.actionId, targetAgentId: params.targetAgentId, providerSessionRef, activation,
      })).digest("hex")}`,
    ...(lifecycleAttestation === undefined ? {} : { lifecycleAttestation }),
  };
  if (lifecycleAttestation !== undefined && process.env.LIFECYCLE_FAKE_ATTESTATION_MUTATION === "custody") {
    result.lifecycleAttestation = { ...lifecycleAttestation, custodyId: `${custodyId}:crossed` };
  }
  if (lifecycleAttestation !== undefined && process.env.LIFECYCLE_FAKE_ATTESTATION_MUTATION === "unknown-provider-field") {
    result.unexpected = "rejected";
  }
  return result;
}

async function runRetainedLifecycleCallback(payload: Record<string, unknown>): Promise<unknown> {
  if (retainedProtocol === undefined || retainedBinding === undefined) {
    throw new Error("retained lifecycle bridge is unavailable");
  }
  const lifecycleRequest = payload.lifecycleRequest;
  if (!isRecord(lifecycleRequest)) throw new Error("retained lifecycle request is missing");
  const taskId = lifecycleRequest.taskId;
  const expectedTaskRevision = lifecycleRequest.expectedTaskRevision;
  const action = lifecycleRequest.action;
  const checkpoint = lifecycleRequest.checkpoint;
  const commandId = lifecycleRequest.commandId;
  if (
    typeof taskId !== "string" || typeof expectedTaskRevision !== "number" ||
    (action !== "compact" && action !== "rotate" && action !== "completion-ready" && action !== "release") ||
    !isRecord(checkpoint) || typeof commandId !== "string"
  ) {
    throw new Error("retained lifecycle task binding is invalid");
  }
  const claimed = await retainedProtocol.call(FABRIC_OPERATIONS.claimTask, {
    taskId,
    expectedRevision: expectedTaskRevision,
    commandId: `${commandId}:claim`,
  });
  if (!isRecord(claimed) || typeof claimed.revision !== "number") {
    throw new Error("retained lifecycle task claim is invalid");
  }
  return await retainedProtocol.call(FABRIC_OPERATIONS.requestLifecycle, {
    action,
    agentId: retainedBinding.agentId,
    taskId,
    taskRevision: claimed.revision,
    checkpoint: checkpoint as LifecycleCheckpoint,
    commandId,
  });
}

const retainedActionOperations = Object.freeze({
  acquireWriteLease: FABRIC_OPERATIONS.acquireWriteLease,
  attachAgent: FABRIC_OPERATIONS.attachAgent,
  claimTask: FABRIC_OPERATIONS.claimTask,
  createTask: FABRIC_OPERATIONS.createTask,
  delegateAuthority: FABRIC_OPERATIONS.delegateAuthority,
  receiveMessages: FABRIC_OPERATIONS.receiveMessages,
  requestLifecycle: FABRIC_OPERATIONS.requestLifecycle,
});

async function runRetainedAction(payload: Record<string, unknown>): Promise<unknown> {
  if (retainedProtocol === undefined || retainedBinding === undefined) {
    throw new Error("retained lifecycle bridge is unavailable");
  }
  const retainedAction = payload.retainedAction;
  if (!isRecord(retainedAction) || typeof retainedAction.operation !== "string" || !isRecord(retainedAction.input)) {
    throw new Error("retained test action is invalid");
  }
  if (payload.agentId !== retainedBinding.agentId) {
    throw new Error("retained test action agent binding changed");
  }
  const operation = retainedActionOperations[
    retainedAction.operation as keyof typeof retainedActionOperations
  ];
  if (operation === undefined) throw new Error("retained test action is not allowlisted");
  return await retainedProtocol.call(operation, retainedAction.input as never);
}

const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on("line", (line) => {
  const parsed: unknown = JSON.parse(line);
  if (!isRecord(parsed) || typeof parsed.id !== "string" || typeof parsed.method !== "string" || !isRecord(parsed.params)) {
    throw new Error("invalid fake provider request");
  }
  const request: Request = { id: parsed.id, method: parsed.method, params: parsed.params };
  const journal = loadJournal();

  if (request.method === "capabilities") {
    const result = {
      protocolVersion: 1,
      operations: ["status", "spawn", "attach", "dispatch", "lookup_action", "cancel_action", "release"],
      actionJournal: true,
      ephemeralWorker: true,
      answerBearingSpawn: true,
      answerBearingSpawnTurns: process.env.LIFECYCLE_FAKE_PAYLOAD_MAX_TURNS === "1"
        ? "payload-max-turns"
        : "one-shot",
      ...(process.env.LIFECYCLE_FAKE_MANDATORY_USAGE === "1"
        ? { answerBearingUsageUnits: ["cost:USD", "input_tokens:fake", "output_tokens:fake"] }
        : {}),
      compactInPlace: false,
      idempotencyEvidence: "per-action",
      agentBridge: bridgeContract,
    };
    const delay = Number(process.env.LIFECYCLE_FAKE_CAPABILITIES_DELAY_MS ?? "0");
    if (Number.isSafeInteger(delay) && delay > 0) setTimeout(() => respond(request.id, result), delay);
    else respond(request.id, result);
    return;
  }
  if (request.method === "provision_agent") {
    void (async () => {
      try {
        const result = await provisionRetainedAgent(request.params);
        const actionId = String(request.params.actionId);
        const currentJournal = loadJournal();
        const lifecycleSpawn = request.params.operation === "spawn";
        const unresolved = lifecycleSpawn && process.env.LIFECYCLE_FAKE_SPAWN_UNRESOLVED === "1";
        const lookupMissing = lifecycleSpawn && process.env.LIFECYCLE_FAKE_SPAWN_LOOKUP_MISSING === "1";
        if (!lookupMissing) {
          currentJournal.actions[actionId] = {
            actionId,
            payloadHash: payloadHash(Object.fromEntries(
              Object.entries(request.params).filter(([key]) => key !== "actionId"),
            )),
            status: unresolved ? "ambiguous" : "terminal",
            history: unresolved
              ? ["prepared", "dispatched", "accepted", "ambiguous"]
              : ["prepared", "dispatched", "accepted", "terminal"],
            executionCount: 1,
            effectCount: 1,
            idempotencyProven: !unresolved,
            ...(unresolved ? {} : { result }),
          };
        }
        if (lifecycleSpawn && isRecord(result) && typeof result.providerSessionRef === "string") {
          currentJournal.sessions[result.providerSessionRef] = {
            released: false,
            generation: typeof result.providerSessionGeneration === "number"
              ? result.providerSessionGeneration
              : 2,
            spawnRequests: (currentJournal.sessions[result.providerSessionRef]?.spawnRequests ?? 0) + 1,
          };
        }
        saveJournal(currentJournal);
        const complete = (): void => {
          if (lookupMissing) {
            fail(request.id, "TRANSPORT_RESULT_UNKNOWN", "provider request began but no action lookup record exists");
          } else if (unresolved) {
            fail(request.id, "PROVIDER_OUTCOME_AMBIGUOUS", "provider effect cannot be reconciled");
          } else if (lifecycleSpawn && process.env.LIFECYCLE_FAKE_SPAWN_RESULT_LOST === "1") {
            fail(request.id, "TRANSPORT_RESULT_LOST", "provider completed but the lifecycle response was lost");
          } else {
            respond(request.id, result);
          }
        };
        if (lifecycleSpawn) {
          afterSpawnBarrier(() => {
            if (Number.isSafeInteger(spawnDelayMs) && spawnDelayMs > 0) setTimeout(complete, spawnDelayMs);
            else complete();
          });
        } else {
          complete();
        }
      } catch (error: unknown) {
        fail(request.id, "PROVISION_FAILED", error instanceof Error ? error.message : String(error));
      }
    })();
    return;
  }
  if (request.method === "spawn") {
    const taskBoundMaxTurns = request.params.maxTurns;
    if (
      typeof request.params.taskId === "string" &&
      (
        typeof taskBoundMaxTurns !== "number" ||
        !Number.isSafeInteger(taskBoundMaxTurns) ||
        taskBoundMaxTurns < 1 ||
        (process.env.LIFECYCLE_FAKE_PAYLOAD_MAX_TURNS !== "1" && taskBoundMaxTurns !== 1)
      )
    ) {
      fail(request.id, "INVALID_PARAMS", "task-bound fake spawn requires its advertised turn ceiling");
      return;
    }
    const prior = typeof request.params.priorResumeReference === "string" ? request.params.priorResumeReference : "new";
    const generation = typeof request.params.generation === "number" ? request.params.generation : 1;
    const resumeReference = `${prior}:replacement:g${String(generation)}`;
    journal.sessions[resumeReference] = {
      released: false,
      generation,
      spawnRequests: (journal.sessions[resumeReference]?.spawnRequests ?? 0) + 1,
    };
    const scenario = typeof request.params.scenario === "string" ? request.params.scenario : "terminal";
    if (scenario.startsWith("ambiguous-review-")) {
      const actionId = request.params.actionId;
      if (typeof actionId !== "string") {
        fail(request.id, "INVALID_PARAMS", "actionId is required");
        return;
      }
      const answer = scenario === "ambiguous-review-valid" ||
        scenario === "ambiguous-review-wrong-action-id" ||
        scenario === "ambiguous-review-concurrent-divergent"
        ? "recovered provider review"
        : scenario === "ambiguous-review-usage-late"
          ? "recovered provider review with late usage"
        : scenario === "ambiguous-review-empty"
          ? ""
          : "x".repeat(262_145);
      journal.actions[actionId] = {
        actionId,
        payloadHash: payloadHash(request.params.payload),
        status: "terminal",
        history: ["prepared", "dispatched", "accepted", "terminal"],
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        result: { resumeReference, generation, result: answer },
        scenario,
        lookupCount: 0,
      };
      saveJournal(journal);
      fail(request.id, "TRANSPORT_RESULT_LOST", "provider completed but the direct response was lost");
      return;
    }
    const result = {
      resumeReference,
      generation,
      result: "fake provider review complete",
      ...(scenario === "terminal-exact-usage"
        ? { resourceUsage: { "cost:USD": 5, "input_tokens:fake": 3, "output_tokens:fake": 4 } }
        : scenario === "terminal-partial-turn-usage"
          ? { resourceUsage: { turns: 1 } }
        : scenario === "terminal-malformed-turn-usage"
          ? { resourceUsage: { turns: -1 } }
        : scenario === "terminal-over-turn-usage"
          ? { resourceUsage: { turns: 3 } }
        : scenario === "terminal-unreserved-usage"
          ? { resourceUsage: { "output_tokens:other": 1 } }
          : scenario === "terminal-over-cap-usage"
            ? { resourceUsage: { "cost:USD": 11 } }
            : scenario === "terminal-malformed-usage"
              ? { resourceUsage: "not-a-budget-vector" }
        : {}),
    };
    if (process.env.LIFECYCLE_FAKE_SPAWN_LOOKUP_MISSING === "1") {
      saveJournal(journal);
      fail(request.id, "TRANSPORT_RESULT_UNKNOWN", "provider request began but no action lookup record exists");
      return;
    }
    const actionId = request.params.actionId;
    const unresolved = process.env.LIFECYCLE_FAKE_SPAWN_UNRESOLVED === "1";
    if (typeof actionId === "string") {
      journal.actions[actionId] = {
        actionId,
        payloadHash: payloadHash(request.params.payload),
        status: unresolved ? "ambiguous" : "terminal",
        history: unresolved
          ? ["prepared", "dispatched", "accepted", "ambiguous"]
          : ["prepared", "dispatched", "accepted", "terminal"],
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: !unresolved,
        ...(unresolved ? {} : { result }),
      };
    }
    saveJournal(journal);
    if (unresolved) {
      fail(request.id, "PROVIDER_OUTCOME_AMBIGUOUS", "provider effect cannot be reconciled");
      return;
    }
    if (process.env.LIFECYCLE_FAKE_SPAWN_RESULT_LOST === "1") {
      fail(request.id, "TRANSPORT_RESULT_LOST", "provider completed but the lifecycle response was lost");
      return;
    }
    const complete = (): void => respond(request.id, result);
    afterSpawnBarrier(() => {
      if (Number.isSafeInteger(spawnDelayMs) && spawnDelayMs > 0) setTimeout(complete, spawnDelayMs);
      else complete();
    });
    return;
  }
  if (request.method === "release") {
    const reference = request.params.resumeReference;
    // Result variant (issue-354 E2R-3): a resume reference carrying the ":release-deleted"
    // sentinel simulates a destructive release. The adapter reports released+deleted, which the
    // fabric release proof (fabric.ts:5712) rejects as non-destructive-proof failure. The sentinel
    // is supplied entirely through the test-controlled agents.provider_session_ref column, so no
    // adapter env wiring is required.
    const destructive = typeof reference === "string" && reference.endsWith(":release-deleted");
    if (typeof reference === "string") {
      journal.sessions[reference] = {
        released: true,
        generation: typeof request.params.generation === "number" ? request.params.generation : 1,
      };
      saveJournal(journal);
    }
    respond(request.id, { released: true, deleted: destructive });
    return;
  }
  if (request.method === "dispatch") {
    const actionId = request.params.actionId;
    const payload = request.params.payload;
    if (typeof actionId !== "string" || !isRecord(payload)) {
      fail(request.id, "INVALID_PARAMS", "actionId and payload are required");
      return;
    }
    const digest = payloadHash(payload);
    const existing = journal.actions[actionId];
    if (existing !== undefined) {
      if (existing.payloadHash !== digest) {
        fail(request.id, "ACTION_CONFLICT", "action ID was reused with changed payload");
        return;
      }
      if (existing.status === "ambiguous" && existing.idempotencyProven) {
        existing.executionCount += 1;
        existing.history.push("dispatched", "accepted", "terminal");
        existing.status = "terminal";
        existing.result = { replayedWithSameActionId: true };
        saveJournal(journal);
      }
      respond(request.id, existing);
      return;
    }
    const scenario = typeof payload.scenario === "string" ? payload.scenario : "terminal";
    if (scenario === "retained-lifecycle-callback") {
      void (async () => {
        try {
          const lifecycle = await runRetainedLifecycleCallback(payload);
          const currentJournal = loadJournal();
          const action: Action = {
            actionId,
            payloadHash: digest,
            status: "terminal",
            history: ["prepared", "dispatched", "accepted", "terminal"],
            executionCount: 1,
            effectCount: 1,
            idempotencyProven: true,
            result: { completed: true, lifecycleAcceptance: lifecycle },
            scenario,
          };
          currentJournal.actions[actionId] = action;
          saveJournal(currentJournal);
          respond(request.id, action);
        } catch (error: unknown) {
          const callbackError = error instanceof Error ? error.message : String(error);
          const currentJournal = loadJournal();
          currentJournal.actions[actionId] = {
            actionId,
            payloadHash: digest,
            status: "ambiguous",
            history: ["prepared", "dispatched", "accepted", "ambiguous"],
            executionCount: 1,
            effectCount: 0,
            idempotencyProven: false,
            result: { callbackError },
            scenario,
          };
          saveJournal(currentJournal);
          fail(request.id, "LIFECYCLE_CALLBACK_FAILED", callbackError);
        }
      })();
      return;
    }
    if (scenario === "retained-test-action") {
      void (async () => {
        try {
          const retainedActionResult = await runRetainedAction(payload);
          const currentJournal = loadJournal();
          const action: Action = {
            actionId,
            payloadHash: digest,
            status: "terminal",
            history: ["prepared", "dispatched", "accepted", "terminal"],
            executionCount: 1,
            effectCount: 1,
            idempotencyProven: true,
            result: { completed: true, retainedActionResult },
            scenario,
          };
          currentJournal.actions[actionId] = action;
          saveJournal(currentJournal);
          respond(request.id, action);
        } catch (error: unknown) {
          const callbackError = error instanceof Error ? error.message : String(error);
          const callbackCode = isRecord(error) && typeof error.code === "string"
            ? error.code
            : "RETAINED_ACTION_FAILED";
          const currentJournal = loadJournal();
          currentJournal.actions[actionId] = {
            actionId,
            payloadHash: digest,
            status: "ambiguous",
            history: ["prepared", "dispatched", "accepted", "ambiguous"],
            executionCount: 1,
            effectCount: 0,
            idempotencyProven: false,
            result: { callbackError, callbackCode },
            scenario,
          };
          saveJournal(currentJournal);
          fail(request.id, callbackCode, callbackError);
        }
      })();
      return;
    }
    const ambiguous = scenario === "ambiguous-unproven" || scenario === "ambiguous-idempotent";
    const action: Action = {
      actionId,
      payloadHash: digest,
      status: ambiguous ? "ambiguous" : "terminal",
      history: ambiguous
        ? ["prepared", "dispatched", "accepted", "ambiguous"]
        : ["prepared", "dispatched", "accepted", "terminal"],
      executionCount: 1,
      effectCount: 1,
      idempotencyProven: scenario === "ambiguous-idempotent",
      ...(ambiguous ? {} : { result: { completed: true } }),
    };
    journal.actions[actionId] = action;
    saveJournal(journal);
    respond(request.id, action);
    return;
  }
  if (request.method === "lookup_action") {
    const action = actionFor(request, journal);
    if (action === undefined) {
      fail(request.id, "ACTION_NOT_FOUND", "action does not exist");
    } else {
      if (action.scenario === "ambiguous-review-concurrent-divergent") {
        action.lookupCount = (action.lookupCount ?? 0) + 1;
        const lookupCount = action.lookupCount;
        const candidate = {
          ...action,
          result: {
            ...(isRecord(action.result) ? action.result : {}),
            result: lookupCount === 1 ? "recovered provider review" : "divergent provider review",
            resourceUsage: lookupCount === 1
              ? { turns: 1 }
              : { turns: 2, "cost:USD": 2 },
          },
        };
        saveJournal(journal);
        if (lookupCount === 1) afterSpawnBarrier(() => respond(request.id, candidate));
        else respond(request.id, candidate);
        return;
      }
      if (action.scenario === "ambiguous-review-usage-late") {
        action.lookupCount = (action.lookupCount ?? 0) + 1;
        if (action.lookupCount >= 2 && isRecord(action.result)) {
          action.result.resourceUsage = {
            "cost:USD": 5,
            "input_tokens:fake": 3,
            "output_tokens:fake": 4,
          };
        }
        saveJournal(journal);
      }
      respond(request.id, action.scenario === "ambiguous-review-wrong-action-id"
        ? { ...action, actionId: `${action.actionId}:wrong` }
        : action);
    }
    return;
  }
  if (request.method === "cancel_action") {
    const action = actionFor(request, journal);
    if (action === undefined) {
      fail(request.id, "ACTION_NOT_FOUND", "action does not exist");
    } else {
      action.status = "terminal";
      action.history.push("terminal");
      saveJournal(journal);
      respond(request.id, action);
    }
    return;
  }
  if (request.method === "status") {
    const callsPath = process.env.LIFECYCLE_FAKE_STATUS_CALLS;
    if (callsPath !== undefined) {
      appendFileSync(callsPath, `${JSON.stringify({ adapterId, params: request.params })}\n`, { mode: 0o600 });
    }
    if (process.env.LIFECYCLE_FAKE_STATUS === "missing-evidence") {
      respond(request.id, {});
      return;
    }
    const managed = process.env.LIFECYCLE_FAKE_STATUS !== "unmanaged";
    respond(request.id, { healthy: managed, matches: managed });
    return;
  }
  if (request.method === "retained_bridge_health") {
    const binding = retainedBinding;
    const live = binding !== undefined && retainedProtocol !== undefined &&
      request.params.kind === "child" && request.params.actionId === binding.actionId &&
      request.params.agentId === binding.agentId && request.params.projectSessionId === binding.projectSessionId &&
      request.params.runId === binding.runId && request.params.principalGeneration === binding.principalGeneration &&
      request.params.providerSessionRef === binding.providerSessionRef &&
      request.params.providerSessionGeneration === binding.providerSessionGeneration &&
      request.params.bridgeGeneration === binding.bridgeGeneration;
    respond(request.id, { schemaVersion: 1, kind: request.params.kind, live });
    return;
  }
  fail(request.id, "METHOD_NOT_FOUND", `unsupported method ${request.method}`);
});

input.on("close", () => {
  void retainedProtocol?.close();
});
