import { existsSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { createInterface } from "node:readline";

import {
  FABRIC_OPERATIONS,
  NdjsonRpcTransport,
  type AuthorityInput,
} from "@local/agent-fabric-protocol";
import {
  DeadlineTimeoutError,
  waitUntil,
} from "../shared/deadline-wait.ts";
import {
  chairLaunchAttestationDigest,
  chairLaunchChallengeDigest,
} from "../../src/adapters/providers/types.js";

const contractJson = process.env.FRESH_LAUNCH_CONTRACT_JSON;
const peerAuthorityJson = process.env.FRESH_LAUNCH_PEER_AUTHORITY_JSON;
const peerReadyPath = process.env.FRESH_LAUNCH_PEER_READY_PATH;
const peerTriggerPath = process.env.FRESH_LAUNCH_PEER_TRIGGER_PATH;
if (
  contractJson === undefined || peerAuthorityJson === undefined ||
  peerReadyPath === undefined || peerTriggerPath === undefined
) {
  throw new Error("fresh launch adapter requires its contract and peer launch inputs");
}
const chairLaunch: unknown = JSON.parse(contractJson);
const peerAuthority = JSON.parse(peerAuthorityJson) as AuthorityInput;
const requiredPeerReadyPath: string = peerReadyPath;
const requiredPeerTriggerPath: string = peerTriggerPath;
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });

async function createClaudePeer(): Promise<void> {
  const capability = process.env.AGENT_FABRIC_CAPABILITY;
  const socketPath = process.env.AGENT_FABRIC_SOCKET_PATH;
  const runId = process.env.AGENT_FABRIC_EXPECTED_RUN_ID;
  if (capability === undefined || socketPath === undefined || runId === undefined) {
    throw new Error("private launch principal is missing");
  }
  const transport = await NdjsonRpcTransport.connect(createConnection(socketPath), {
    protocolVersion: 1,
    client: { name: "fresh-launch-adapter", version: "1.0.0" },
    authentication: {
      scheme: "capability",
      credential: capability,
      clientNonce: "fresh_launch_peer_bootstrap_01",
    },
    expectedPrincipalKind: "agent",
    requiredFeatures: ["fabric-core.v1"],
    optionalFeatures: [],
  });
  try {
    try {
      await waitUntil(
        () => existsSync(requiredPeerTriggerPath),
        5_000,
        "Fresh launch peer trigger",
        { pollIntervalMs: 10 },
      );
    } catch (error: unknown) {
      if (!(error instanceof DeadlineTimeoutError)) throw error;
      throw new Error("fresh launch peer trigger timed out", { cause: error });
    }
    const team = await transport.call(FABRIC_OPERATIONS.createTeam, {
      teamId: "team_fresh_claude_01",
      leader: { agentId: "claude_fresh_peer_01", authority: peerAuthority },
      rootTask: {
        taskId: "task_fresh_claude_01",
        objective: "Coordinate the fresh Claude peer",
        baseRevision: "a04169b",
      },
      initialMembers: [],
      discussionGroups: [],
      reservedBudget: { provider_calls: 2 },
      commandId: "fresh-launch:create-claude-peer",
    });
    const leader: unknown = typeof team === "object" && team !== null ? Reflect.get(team, "leader") : undefined;
    const leaderAuthorityId: unknown = typeof leader === "object" && leader !== null
      ? Reflect.get(leader, "authorityId")
      : undefined;
    if (typeof leaderAuthorityId !== "string") {
      throw new Error("fresh launch team creation returned no leader authority");
    }
    // Team creation only registers the peer identity; the chair must still
    // issue the peer's own agent principal before an MCP seat can bind to it.
    await transport.call(FABRIC_OPERATIONS.registerAgent, {
      agentId: "claude_fresh_peer_01",
      authorityId: leaderAuthorityId,
    });
    writeFileSync(requiredPeerReadyPath, "ready\n", { mode: 0o600 });
  } finally {
    await transport.close();
  }
}

lines.on("line", (line) => {
  const request = JSON.parse(line) as {
    id: string;
    method: string;
    params?: { actionId?: unknown; providerContractDigest?: unknown; kind?: unknown };
  };
  if (request.method === "capabilities") {
    process.stdout.write(`${JSON.stringify({ id: request.id, result: {
      protocolVersion: 1,
      adapterId: "claude-agent-sdk",
      operations: ["capabilities", "launch_chair", "lookup_action", "retained_bridge_health"],
      actionJournal: true,
      persistentSession: true,
      ephemeralWorker: true,
      controlModes: ["managed"],
      inboxDeliveryModes: ["structured-push"],
      recoveryOperations: ["lookup_action"],
      compactInPlace: false,
      idempotencyEvidence: "per-action-fail-closed",
      chairLaunch,
    } })}\n`);
    return;
  }
  if (request.method === "retained_bridge_health") {
    process.stdout.write(`${JSON.stringify({
      id: request.id,
      result: { schemaVersion: 1, kind: request.params?.kind, live: true },
    })}\n`);
    return;
  }
  if (request.method !== "launch_chair") {
    process.stdout.write(`${JSON.stringify({
      id: request.id,
      error: { code: "UNEXPECTED_CALL", message: `unexpected ${request.method}` },
    })}\n`);
    return;
  }
  const capability = process.env.AGENT_FABRIC_CAPABILITY;
  const challenge = process.env.AGENT_FABRIC_ATTESTATION_CHALLENGE;
  if (capability === undefined || challenge === undefined) throw new Error("private launch handoff is missing");
  const providerActionId = String(request.params?.actionId);
  const providerContractDigest = String(request.params?.providerContractDigest);
  const unsigned = {
    schemaVersion: 1 as const,
    kind: "provider-session-fabric-attestation" as const,
    method: "provider-session-random-challenge-v1" as const,
    bridgeContract: "agent-fabric-session-bridge-v1" as const,
    providerAdapterId: "claude-agent-sdk",
    providerActionId,
    providerContractDigest,
    providerSessionRef: "fixture-claude-chair-session",
    providerSessionGeneration: 1,
    providerTurnRef: "fixture-claude-launch-turn",
    challengeDigest: chairLaunchChallengeDigest(challenge),
    providerInvocationRef: "fixture-claude-tool-call",
  };
  process.stdout.write(`${JSON.stringify({ id: request.id, result: {
    resumeReference: unsigned.providerSessionRef,
    providerSessionGeneration: 1,
    fabricContinuity: {
      ...unsigned,
      attestationDigest: chairLaunchAttestationDigest(unsigned),
    },
  } })}\n`, () => { void createClaudePeer(); });
});
