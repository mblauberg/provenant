import {
  FABRIC_OPERATIONS,
  parseOperationResult,
  type AgentCustodyResult,
  type Sha256Digest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import {
  ProviderActionAdmissionCoordinator,
  type ProviderActionTicket,
} from "../application/provider-action-admission.js";
import {
  assertProviderActionOwner,
  ProviderActionOwnerError,
} from "../application/provider-action-owner.js";
import { readStoredAuthority } from "../authority/stored-authority.js";
import { ProjectFabricCoreError } from "./contracts.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
  type Row,
} from "./store-support.js";

type Digest = Sha256Digest;

// Shared with launch-custody.ts (and, via this module's imports, with
// provider-agent-custody-recovery.ts); kept here rather than in store-support.ts because these
// helpers are specific to launch/provider-agent custody protocol validation, not generic row
// access. launch-custody.ts imports them back from this module to avoid a duplicate definition.
export const DIGEST = /^sha256:[0-9a-f]{64}$/u;

export function protocol(message: string): never {
  throw new ProjectFabricCoreError("PROTOCOL_INVALID", message);
}

export function nonEmptyString(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0 || value.length > 512 || value.includes("\0")) {
    protocol(`${label} must be a bounded non-empty string`);
  }
  return value;
}

export function exactDigest(value: unknown, label: string): Digest {
  if (typeof value !== "string" || !DIGEST.test(value)) protocol(`${label} must be an exact sha256 digest`);
  return value as Digest;
}

export function jsonEvidenceDigest(value: unknown): Digest {
  try {
    return `sha256:${sha256(canonicalJson(value))}` as Digest;
  } catch {
    return `sha256:${sha256(String(value))}` as Digest;
  }
}

export function positiveOutcomeInteger(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) {
    protocol("launch provider session generation must be a positive safe integer");
  }
  return value;
}

export type AgentBridgeContract = Readonly<{
  schemaVersion: 1;
  method: "provision_agent";
  operations: readonly ("spawn" | "attach")[];
  secretTransport: "private-handoff";
  bridgeContract: "agent-fabric-session-bridge-v1";
  generationBound: true;
  providerOriginatedActivation: true;
}>;

export type AgentCustodyInput = Readonly<{
  runId: string;
  actorAgentId: string;
  operation: "spawn" | "attach";
  agentId: string;
  authorityId: string;
  adapterId: string;
  actionId: string;
  payload: Record<string, unknown>;
  providerSessionRef?: string;
  bridgeContract?: AgentBridgeContract;
}>;

export type AgentDispatchHandle = Readonly<{
  schemaVersion: 1;
  runId: string;
  operation: "spawn" | "attach";
  actorAgentId: string;
  targetAgentId: string;
  authorityId: string;
  adapterId: string;
  actionId: string;
  publicPayload: Record<string, unknown>;
  requestedProviderSessionRef?: string;
  bridgeCapable: boolean;
  bridgeContractDigest: Digest;
  bridgeGeneration: number;
  capability?: string;
  socketPath?: string;
  expectedPrincipal?: Readonly<{
    agentId: string;
    projectSessionId: string;
    runId: string;
    principalGeneration: number;
  }>;
}>;

export type ProviderAgentEffectsPort = Readonly<{
  dispatch(handle: AgentDispatchHandle): Promise<unknown>;
  attachWithoutBridge(handle: AgentDispatchHandle): Promise<unknown>;
  lookup(input: Readonly<{ adapterId: string; actionId: string }>): Promise<unknown>;
  hasRetainedBridge(result: AgentCustodyResult, handle: AgentDispatchHandle): boolean;
}>;

export type ProviderAgentCustodyAdapterOptions = Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
  clock: () => number;
  fault: (label: string) => void;
  randomCapability: () => string;
  fabricSocketPath: string;
  agentEffects: ProviderAgentEffectsPort | undefined;
  daemonInstanceGeneration: () => number;
}>;

export type ProviderAgentCustodyRecoveryTally = {
  preparedFailed: number;
  lookedUp: number;
  activated: number;
  failed: number;
  ambiguous: number;
  recoveryRequired: number;
};

export type ChildBridgeLossInput = Readonly<{
  runId: string;
  agentId: string;
  adapterId: string;
  actionId: string;
  providerSessionRef: string;
  providerSessionGeneration: number;
  bridgeGeneration: number;
  reason: string;
}>;

export type ProviderAgentCustodyRecoveryPort = Readonly<{
  agentDispatchHandle(custody: Row): AgentDispatchHandle;
  failPreparedAgent(custody: Row): void;
  agentLookupResult(handle: AgentDispatchHandle, record: unknown): AgentCustodyResult;
  activateAgent(handle: AgentDispatchHandle, result: AgentCustodyResult): void;
  persistChildBridgeLoss(input: ChildBridgeLossInput): boolean;
  fenceUnprovenAgent(handle: AgentDispatchHandle, evidenceDigest: string): void;
}>;

/**
 * Byte-moved from `LaunchCustodyService`'s provider-agent custody family: `provisionAgent`,
 * `#provisionAgentOnce`, `prepareAgentInTransaction`, `dispatchPreparedAgent`,
 * `#normaliseAgentResult`, `#activateAgent`, `#fenceUnprovenAgent`, `observeChildBridgeLoss`,
 * `#persistChildBridgeLoss`, `#agentDispatchHandle`, `#failPreparedAgent`, and
 * `#agentLookupResult` (issue #354, S4b). Preserves: preparation in one immediate transaction,
 * `dispatched` status persisted before any adapter I/O, activation and its fence run
 * transactionally together, and child/session loss persisted before bridge/capability/identity
 * are revoked. `LaunchCustodyService#provisionAgent` and `#observeChildBridgeLoss` remain thin
 * facade delegations onto this class with unchanged public signatures. `#recoverAgentCustody`
 * was split into `ProviderAgentCustodyRecoveryAdapter` (provider-agent-custody-recovery.ts) to
 * keep this module under the repository's line-count ceiling; `recoveryPort()` below exposes
 * this class's private mutation methods to that adapter as bound closures.
 */
export class ProviderAgentCustodyAdapter {
  readonly #database: Database.Database;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #randomCapability: () => string;
  readonly #fabricSocketPath: string;
  readonly #agentEffects: ProviderAgentEffectsPort | undefined;
  readonly #daemonInstanceGeneration: () => number;
  readonly #consumedAgentHandles = new Set<string>();
  readonly #agentInFlight = new Map<string, Promise<AgentCustodyResult>>();

  constructor(options: ProviderAgentCustodyAdapterOptions) {
    this.#database = options.database;
    this.#providerActionAdmission = options.providerActionAdmission;
    this.#clock = options.clock;
    this.#fault = options.fault;
    this.#randomCapability = options.randomCapability;
    this.#fabricSocketPath = options.fabricSocketPath;
    this.#agentEffects = options.agentEffects;
    this.#daemonInstanceGeneration = options.daemonInstanceGeneration;
  }

  async provisionAgent(input: AgentCustodyInput): Promise<AgentCustodyResult> {
    const key = `${input.adapterId}\0${input.actionId}`;
    const existing = this.#agentInFlight.get(key);
    if (existing !== undefined) return await existing;
    const work = this.#provisionAgentOnce(input);
    this.#agentInFlight.set(key, work);
    try {
      return await work;
    } finally {
      if (this.#agentInFlight.get(key) === work) this.#agentInFlight.delete(key);
    }
  }

  async #provisionAgentOnce(input: AgentCustodyInput): Promise<AgentCustodyResult> {
    if (input.operation === "spawn" && input.bridgeContract === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_UNAVAILABLE", "adapter cannot provision a retained child bridge");
    }
    if (
      input.bridgeContract !== undefined &&
      !input.bridgeContract.operations.includes(input.operation)
    ) {
      if (input.operation === "spawn") {
        throw new ProjectFabricCoreError("CAPABILITY_UNAVAILABLE", "adapter cannot provision a spawn bridge");
      }
    }
    const providerActionTicket = this.#providerActionAdmission.preflightAgentAction({
      runId: input.runId,
      actorAgentId: input.actorAgentId,
      actionRef: { adapterId: input.adapterId, actionId: input.actionId },
      canonicalInput: {
        schemaVersion: 1,
        operation: input.operation,
        actorAgentId: input.actorAgentId,
        targetAgentId: input.agentId,
        authorityId: input.authorityId,
        payload: input.payload,
        providerSessionRef: input.providerSessionRef ?? null,
      },
    });
    const prepared = this.#database.transaction(() => (
      this.prepareAgentInTransaction(input, providerActionTicket)
    )).immediate();
    if (prepared.kind === "replay") return prepared.result;
    return await this.dispatchPreparedAgent(prepared.handle);
  }

  prepareAgentInTransaction(input: AgentCustodyInput, providerActionTicket: ProviderActionTicket):
    | { kind: "dispatch"; handle: AgentDispatchHandle }
    | { kind: "replay"; result: AgentCustodyResult } {
    if (!this.#database.inTransaction) throw new Error("agent custody preparation requires a transaction");
    const bridgeCapable = input.bridgeContract?.operations.includes(input.operation) === true;
    const bridgeContractDigest = `sha256:${sha256(canonicalJson(
      input.bridgeContract ?? { schemaVersion: 1, kind: "bridge-unavailable", adapterId: input.adapterId },
    ))}` as Digest;
    const intentDigest = `sha256:${sha256(canonicalJson({
      runId: input.runId,
      actorAgentId: input.actorAgentId,
      operation: input.operation,
      agentId: input.agentId,
      authorityId: input.authorityId,
      adapterId: input.adapterId,
      actionId: input.actionId,
      payload: input.payload,
      providerSessionRef: input.providerSessionRef ?? null,
      bridgeContractDigest,
      bridgeCapable,
    }))}` as Digest;
    const existing = this.#database.prepare(`
      SELECT c.intent_digest, p.status, p.result_json,
             b.bridge_state, b.bridge_generation
        FROM provider_agent_custody c
        JOIN provider_actions p
          ON p.run_id=c.run_id AND p.adapter_id=c.adapter_id AND p.action_id=c.action_id
        LEFT JOIN agent_bridge_state b ON b.run_id=c.run_id AND b.agent_id=c.target_agent_id
       WHERE c.adapter_id=? AND c.action_id=?
    `).get(input.adapterId, input.actionId);
    if (isRow(existing)) {
      assertProviderActionOwner(this.#database, {
        runId: input.runId,
        adapterId: input.adapterId,
        actionId: input.actionId,
      }, "provider_agent");
      if (existing.intent_digest !== intentDigest) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "agent custody action was reused with changed input");
      }
      if (existing.status === "terminal" && typeof existing.result_json === "string") {
        const stored: unknown = JSON.parse(existing.result_json);
        if (isRow(stored) && stored.kind === "agent-custody-pre-dispatch-no-effect") {
          throw new ProjectFabricCoreError(
            "CONTEXT_UNRECONCILED",
            "agent custody was proved not dispatched before daemon restart",
          );
        }
        const parsed = parseOperationResult(
          input.operation === "spawn" ? FABRIC_OPERATIONS.spawnAgent : FABRIC_OPERATIONS.attachAgent,
          stored,
        );
        if (
          isRow(parsed) && parsed.bridgeState === "active" &&
          (
            existing.bridge_state !== "active" ||
            existing.bridge_generation !== parsed.bridgeGeneration
          )
        ) {
          throw new ProjectFabricCoreError(
            "CONTEXT_UNRECONCILED",
            "agent custody result outlived its retained provider bridge",
          );
        }
        return { kind: "replay", result: parsed as AgentCustodyResult };
      }
      throw new ProjectFabricCoreError("CONFLICT", "agent custody action is already in progress");
    }

    const actor = row(this.#database.prepare(`
      SELECT authority_id FROM agents WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.actorAgentId), "agent custody actor");
    const authority = row(this.#database.prepare(`
      SELECT parent_authority_id, authority_json, authority_hash FROM authorities
       WHERE run_id=? AND authority_id=?
    `).get(input.runId, input.authorityId), "agent custody authority");
    if (authority.parent_authority_id !== actor.authority_id) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "actor cannot provision this agent authority");
    }
    const authorityValue = readStoredAuthority(authority, "agent custody authority");
    const expiresAt = Date.parse(authorityValue.expiresAt);
    if (!Number.isFinite(expiresAt) || expiresAt <= this.#clock()) {
      throw new ProjectFabricCoreError("CAPABILITY_EXPIRED", "agent custody authority is expired");
    }

    const currentAgent = this.#database.prepare(`
      SELECT parent_agent_id, authority_id FROM agents WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId);
    if (isRow(currentAgent)) {
      if (currentAgent.parent_agent_id !== input.actorAgentId || currentAgent.authority_id !== input.authorityId) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "agent identity was reused with changed authority");
      }
    } else {
      this.#database.prepare(`
        INSERT INTO agents(
          run_id, agent_id, parent_agent_id, authority_id, provider_session_ref, classification
        ) VALUES (?, ?, ?, ?, NULL, 'worker')
      `).run(input.runId, input.agentId, input.actorAgentId, input.authorityId);
      this.#database.prepare("INSERT INTO mailbox_state(run_id, recipient_id) VALUES (?, ?)")
        .run(input.runId, input.agentId);
    }
    this.#fault("agent:prepare:identity");

    const priorBridge = this.#database.prepare(`
      SELECT bridge_generation, bridge_state FROM agent_bridge_state WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId);
    if (isRow(priorBridge) && (priorBridge.bridge_state === "active" || priorBridge.bridge_state === "pending")) {
      throw new ProjectFabricCoreError("CONFLICT", "agent already has an active or pending provider bridge");
    }
    const bridgeGeneration = !isRow(priorBridge)
      ? 1
      : priorBridge.bridge_state === "lost"
        ? integer(priorBridge, "bridge_generation")
        : integer(priorBridge, "bridge_generation") + 1;
    let capability: string | undefined;
    let capabilityHash: string | null = null;
    let principalGeneration: number | null = null;
    if (bridgeCapable) {
      principalGeneration = integer(row(this.#database.prepare(`
        SELECT COALESCE(MAX(principal_generation), 0) + 1 AS generation
          FROM capabilities WHERE run_id=? AND agent_id=?
      `).get(input.runId, input.agentId), "agent principal generation"), "generation");
      capability = this.#randomCapability();
      if (!/^afc_[A-Za-z0-9_-]{32,}$/u.test(capability)) {
        throw new Error("random agent capability has invalid format");
      }
      capabilityHash = sha256(capability);
      this.#database.prepare(`
        UPDATE capabilities SET revoked_at=?
         WHERE run_id=? AND agent_id=? AND revoked_at IS NULL
      `).run(this.#clock(), input.runId, input.agentId);
      this.#database.prepare(`
        INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
        VALUES (?, ?, ?, ?, ?)
      `).run(capabilityHash, input.runId, input.agentId, principalGeneration, expiresAt);
    }
    const projectSessionId = text(row(this.#database.prepare(`
      SELECT project_session_id FROM runs WHERE run_id=?
    `).get(input.runId), "agent custody run"), "project_session_id");
    this.#fault("agent:prepare:capability");

    const publicPayload = {
      schemaVersion: 1,
      operation: input.operation,
      actorAgentId: input.actorAgentId,
      targetAgentId: input.agentId,
      authorityId: input.authorityId,
      bridgeGeneration,
      bridgeContractDigest,
      payload: input.payload,
      ...(input.providerSessionRef === undefined ? {} : { providerSessionRef: input.providerSessionRef }),
    };
    const payloadJson = canonicalJson(publicPayload);
    this.#providerActionAdmission.admitUnroutedInCurrentTransaction(providerActionTicket, {
      runId: input.runId,
      actionId: input.actionId,
      adapterId: input.adapterId,
      operation: input.operation,
      targetAgentId: input.agentId,
      identityHash: sha256(canonicalJson({ input: publicPayload, intentDigest })),
      payloadHash: sha256(payloadJson),
      payloadJson,
      status: "prepared",
      historyJson: '["prepared"]',
      executionCount: 0,
      updatedAt: this.#clock(),
    }, "provider_agent", () => {
      this.#fault("agent:prepare:action");
      this.#database.prepare(`
      INSERT INTO provider_agent_custody(
        run_id, action_id, operation, actor_agent_id, target_agent_id, authority_id,
        adapter_id, bridge_contract_digest, bridge_capable, capability_hash,
        capability_expires_at, principal_generation, requested_provider_session_ref,
        intent_digest, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        input.runId,
        input.actionId,
        input.operation,
        input.actorAgentId,
        input.agentId,
        input.authorityId,
        input.adapterId,
        bridgeContractDigest,
        bridgeCapable ? 1 : 0,
        capabilityHash,
        bridgeCapable ? expiresAt : null,
        principalGeneration,
        input.providerSessionRef ?? null,
        intentDigest,
        this.#clock(),
      );
    });
    this.#fault("agent:prepare:custody");
    const bridgeValues = [
      input.runId,
      input.agentId,
      input.adapterId,
      input.actionId,
      bridgeCapable ? "pending" : "none",
      bridgeGeneration,
      capabilityHash,
      this.#clock(),
      this.#clock(),
    ];
    this.#database.prepare(`
      INSERT INTO agent_bridge_state(
        run_id, agent_id, adapter_id, action_id, provider_session_ref,
        provider_session_generation, bridge_state, bridge_generation,
        capability_hash, activation_evidence_digest, revision, created_at, updated_at
      ) VALUES (?, ?, ?, ?, NULL, NULL, ?, ?, ?, NULL, 1, ?, ?)
      ON CONFLICT(run_id, agent_id) DO UPDATE SET
        adapter_id=excluded.adapter_id,
        action_id=excluded.action_id,
        provider_session_ref=NULL,
        provider_session_generation=NULL,
        bridge_state=excluded.bridge_state,
        bridge_generation=excluded.bridge_generation,
        capability_hash=excluded.capability_hash,
        activation_evidence_digest=NULL,
        revision=agent_bridge_state.revision+1,
        updated_at=excluded.updated_at
    `).run(...bridgeValues);
    this.#fault("agent:prepare:bridge-state");
    return {
      kind: "dispatch",
      handle: {
        schemaVersion: 1,
        runId: input.runId,
        operation: input.operation,
        actorAgentId: input.actorAgentId,
        targetAgentId: input.agentId,
        authorityId: input.authorityId,
        adapterId: input.adapterId,
        actionId: input.actionId,
        publicPayload: input.payload,
        ...(input.providerSessionRef === undefined ? {} : { requestedProviderSessionRef: input.providerSessionRef }),
        bridgeCapable,
        bridgeContractDigest,
        bridgeGeneration,
        ...(capability === undefined ? {} : {
          capability,
          socketPath: this.#fabricSocketPath,
          expectedPrincipal: {
            agentId: input.agentId,
            projectSessionId,
            runId: input.runId,
            principalGeneration: principalGeneration as number,
          },
        }),
      },
    };
  }

  async dispatchPreparedAgent(handle: AgentDispatchHandle): Promise<AgentCustodyResult> {
    if (this.#agentEffects === undefined) throw new Error("agent custody effects are unavailable");
    assertProviderActionOwner(this.#database, {
      runId: handle.runId,
      adapterId: handle.adapterId,
      actionId: handle.actionId,
    }, "provider_agent");
    const key = `${handle.adapterId}\0${handle.actionId}`;
    if (this.#consumedAgentHandles.has(key)) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "agent custody handoff is one-use");
    }
    const changed = this.#database.prepare(`
      UPDATE provider_actions
         SET status='dispatched', history_json='["prepared","dispatched"]',
             execution_count=1, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status='prepared'
         AND EXISTS (
           SELECT 1 FROM provider_agent_custody c
            WHERE c.adapter_id=provider_actions.adapter_id
              AND c.action_id=provider_actions.action_id
              AND c.bridge_contract_digest=?
         )
    `).run(this.#clock(), handle.adapterId, handle.actionId, handle.bridgeContractDigest);
    if (changed.changes !== 1) throw new ProjectFabricCoreError("CONFLICT", "agent action is not prepared");
    this.#consumedAgentHandles.add(key);
    try {
      const raw = handle.bridgeCapable
        ? await this.#agentEffects.dispatch(handle)
        : await this.#agentEffects.attachWithoutBridge(handle);
      const result = this.#normaliseAgentResult(handle, raw);
      if (handle.bridgeCapable && !this.#agentEffects.hasRetainedBridge(result, handle)) {
        throw new ProjectFabricCoreError("CONTEXT_UNRECONCILED", "agent provider bridge was not retained");
      }
      this.#database.transaction(() => this.#activateAgent(handle, result))();
      if (handle.bridgeCapable && !this.#agentEffects.hasRetainedBridge(result, handle)) {
        this.observeChildBridgeLoss({
          runId: handle.runId,
          agentId: result.agentId,
          adapterId: result.adapterId,
          actionId: result.actionId,
          providerSessionRef: result.providerSessionRef,
          providerSessionGeneration: result.providerSessionGeneration,
          bridgeGeneration: result.bridgeGeneration,
          reason: "retained child bridge closed during activation commit",
        });
        throw new ProjectFabricCoreError("CONTEXT_UNRECONCILED", "agent provider bridge was lost during activation");
      }
      return result;
    } catch (error: unknown) {
      if (error instanceof ProviderActionOwnerError) throw error;
      const evidence = `sha256:${sha256(canonicalJson({
        code: error instanceof Error ? error.name : "agent-dispatch-error",
        message: error instanceof Error ? error.message : String(error),
      }))}`;
      this.#database.transaction(() => this.#fenceUnprovenAgent(handle, evidence))();
      throw new ProjectFabricCoreError(
        "CONTEXT_UNRECONCILED",
        "agent provider custody is ambiguous and requires lookup recovery",
      );
    }
  }

  #normaliseAgentResult(handle: AgentDispatchHandle, value: unknown): AgentCustodyResult {
    if (!isRow(value)) protocol("agent adapter result must be an object");
    const providerSessionRef = nonEmptyString(value.providerSessionRef, "agent provider session reference");
    const providerSessionGeneration = positiveOutcomeInteger(value.providerSessionGeneration ?? 1);
    const evidenceDigest = handle.bridgeCapable
      ? exactDigest(value.activationEvidenceDigest, "agent activation evidence digest")
      : (`sha256:${sha256(canonicalJson({
          kind: "bridge-unavailable-attach",
          adapterId: handle.adapterId,
          actionId: handle.actionId,
          providerSessionRef,
          providerSessionGeneration,
        }))}` as Digest);
    const result = {
      agentId: handle.targetAgentId,
      authorityId: handle.authorityId,
      adapterId: handle.adapterId,
      actionId: handle.actionId,
      providerSessionRef,
      providerSessionGeneration,
      bridgeState: handle.bridgeCapable ? "active" as const : "none" as const,
      bridgeGeneration: handle.bridgeGeneration,
      evidenceDigest,
    };
    return parseOperationResult(
      handle.operation === "spawn" ? FABRIC_OPERATIONS.spawnAgent : FABRIC_OPERATIONS.attachAgent,
      result,
    ) as AgentCustodyResult;
  }

  #activateAgent(handle: AgentDispatchHandle, result: AgentCustodyResult): void {
    assertProviderActionOwner(this.#database, {
      runId: handle.runId,
      adapterId: handle.adapterId,
      actionId: handle.actionId,
    }, "provider_agent");
    const now = this.#clock();
    const action = this.#database.prepare(`
      UPDATE provider_actions
         SET status='terminal', history_json='["prepared","dispatched","accepted","terminal"]',
             effect_count=1, idempotency_proven=1, provider_session_generation=?,
             result_json=?, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
    `).run(
      result.providerSessionGeneration,
      canonicalJson(result),
      now,
      handle.adapterId,
      handle.actionId,
    );
    if (action.changes !== 1) throw new ProjectFabricCoreError("CONFLICT", "agent action changed before activation");
    this.#database.prepare(`
      UPDATE agents SET provider_session_ref=?, lifecycle='ready'
       WHERE run_id=? AND agent_id=?
    `).run(result.providerSessionRef, handle.runId, handle.targetAgentId);
    this.#database.prepare(`
      INSERT INTO provider_state(run_id, agent_id, provider_session_generation, context_revision, reconciled_checkpoint_sha256)
      VALUES (?, ?, ?, NULL, NULL)
      ON CONFLICT(run_id, agent_id) DO UPDATE SET
        provider_session_generation=excluded.provider_session_generation,
        context_revision=NULL,
        reconciled_checkpoint_sha256=NULL
    `).run(handle.runId, handle.targetAgentId, result.providerSessionGeneration);
    this.#database.prepare(`
      INSERT INTO agent_adapter_bindings(run_id, agent_id, adapter_id, bound_at)
      VALUES (?, ?, ?, ?)
      ON CONFLICT(run_id, agent_id) DO UPDATE SET
        adapter_id=excluded.adapter_id, bound_at=excluded.bound_at
    `).run(handle.runId, handle.targetAgentId, handle.adapterId, now);
    const bridge = this.#database.prepare(`
      UPDATE agent_bridge_state
         SET provider_session_ref=?, provider_session_generation=?, bridge_state=?,
             capability_hash=CASE WHEN ?='active' THEN (
               SELECT capability_hash FROM provider_agent_custody
                WHERE adapter_id=? AND action_id=?
             ) ELSE NULL END,
             activation_evidence_digest=?, revision=revision+1, updated_at=?
       WHERE run_id=? AND agent_id=? AND adapter_id=? AND action_id=?
         AND bridge_generation=? AND bridge_state IN ('pending','none')
    `).run(
      result.providerSessionRef,
      result.providerSessionGeneration,
      result.bridgeState,
      result.bridgeState,
      handle.adapterId,
      handle.actionId,
      result.bridgeState === "active" ? result.evidenceDigest : null,
      now,
      handle.runId,
      handle.targetAgentId,
      handle.adapterId,
      handle.actionId,
      handle.bridgeGeneration,
    );
    if (bridge.changes !== 1) throw new ProjectFabricCoreError("CONFLICT", "agent bridge changed before activation");
  }

  #fenceUnprovenAgent(handle: AgentDispatchHandle, evidenceDigest: string): void {
    assertProviderActionOwner(this.#database, {
      runId: handle.runId,
      adapterId: handle.adapterId,
      actionId: handle.actionId,
    }, "provider_agent");
    const now = this.#clock();
    const custody = this.#database.prepare(`
      SELECT capability_hash FROM provider_agent_custody WHERE adapter_id=? AND action_id=?
    `).get(handle.adapterId, handle.actionId);
    if (isRow(custody) && typeof custody.capability_hash === "string") {
      this.#database.prepare("UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL")
        .run(now, custody.capability_hash);
    }
    this.#database.prepare(`
      UPDATE agent_bridge_state
         SET bridge_state='none', capability_hash=NULL, activation_evidence_digest=NULL,
             revision=revision+1, updated_at=?
       WHERE run_id=? AND agent_id=? AND adapter_id=? AND action_id=? AND bridge_state='pending'
    `).run(now, handle.runId, handle.targetAgentId, handle.adapterId, handle.actionId);
    this.#database.prepare("UPDATE agents SET lifecycle='context-unreconciled' WHERE run_id=? AND agent_id=?")
      .run(handle.runId, handle.targetAgentId);
    this.#database.prepare(`
      UPDATE provider_actions
         SET status='ambiguous', history_json='["prepared","dispatched","ambiguous"]',
             result_json=?, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
    `).run(
      canonicalJson({ schemaVersion: 1, kind: "agent-custody-ambiguous", evidenceDigest }),
      now,
      handle.adapterId,
      handle.actionId,
    );
  }

  observeChildBridgeLoss(input: Readonly<{
    runId: string;
    agentId: string;
    adapterId: string;
    actionId: string;
    providerSessionRef: string;
    providerSessionGeneration: number;
    bridgeGeneration: number;
    reason: string;
  }>): void {
    this.#database.transaction(() => this.#persistChildBridgeLoss(input))();
  }

  #persistChildBridgeLoss(input: Readonly<{
    runId: string;
    agentId: string;
    adapterId: string;
    actionId: string;
    providerSessionRef: string;
    providerSessionGeneration: number;
    bridgeGeneration: number;
    reason: string;
  }>): boolean {
    const state = this.#database.prepare(`
      SELECT capability_hash
        FROM agent_bridge_state
       WHERE run_id=? AND agent_id=? AND adapter_id=? AND action_id=?
         AND provider_session_ref=? AND provider_session_generation=?
         AND bridge_generation=? AND bridge_state='active'
    `).get(
      input.runId,
      input.agentId,
      input.adapterId,
      input.actionId,
      input.providerSessionRef,
      input.providerSessionGeneration,
      input.bridgeGeneration,
    );
    if (!isRow(state)) return false;
    const capabilityHash = text(state, "capability_hash");
    const reason = input.reason.slice(0, 160) || "retained child bridge lost";
    const evidenceDigest = `sha256:${sha256(canonicalJson({ ...input, reason }))}`;
    const lossId = `child-loss:${sha256(`${input.runId}\0${input.agentId}\0${String(input.bridgeGeneration)}`).slice(0, 40)}`;
    this.#database.prepare(`
      INSERT OR IGNORE INTO child_bridge_losses(
        loss_id, run_id, agent_id, adapter_id, action_id, provider_session_ref,
        provider_session_generation, lost_bridge_generation, next_bridge_generation,
        capability_hash, daemon_instance_generation, reason, evidence_digest, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      lossId,
      input.runId,
      input.agentId,
      input.adapterId,
      input.actionId,
      input.providerSessionRef,
      input.providerSessionGeneration,
      input.bridgeGeneration,
      input.bridgeGeneration + 1,
      capabilityHash,
      this.#daemonInstanceGeneration(),
      reason,
      evidenceDigest,
      this.#clock(),
    );
    this.#database.prepare("UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL")
      .run(this.#clock(), capabilityHash);
    this.#database.prepare(`
      UPDATE agent_bridge_state
         SET bridge_state='lost', bridge_generation=bridge_generation+1,
             revision=revision+1, updated_at=?
       WHERE run_id=? AND agent_id=? AND bridge_state='active' AND bridge_generation=?
    `).run(this.#clock(), input.runId, input.agentId, input.bridgeGeneration);
    this.#database.prepare("UPDATE agents SET lifecycle='context-unreconciled' WHERE run_id=? AND agent_id=?")
      .run(input.runId, input.agentId);
    return true;
  }
  #agentDispatchHandle(custody: Row): AgentDispatchHandle {
    const payloadValue: unknown = JSON.parse(text(custody, "payload_json"));
    if (!isRow(payloadValue) || !isRow(payloadValue.payload)) {
      throw new Error("agent custody payload is invalid");
    }
    const operation = text(custody, "operation");
    if (operation !== "spawn" && operation !== "attach") throw new Error("agent custody operation is invalid");
    const bridgeCapable = integer(custody, "bridge_capable") === 1;
    return {
      schemaVersion: 1,
      runId: text(custody, "run_id"),
      operation,
      actorAgentId: text(custody, "actor_agent_id"),
      targetAgentId: text(custody, "target_agent_id"),
      authorityId: text(custody, "authority_id"),
      adapterId: text(custody, "adapter_id"),
      actionId: text(custody, "action_id"),
      publicPayload: payloadValue.payload,
      ...(typeof custody.requested_provider_session_ref === "string"
        ? { requestedProviderSessionRef: custody.requested_provider_session_ref }
        : {}),
      bridgeCapable,
      bridgeContractDigest: exactDigest(custody.bridge_contract_digest, "agent bridge contract digest"),
      bridgeGeneration: integer(custody, "bridge_generation"),
    };
  }

  #failPreparedAgent(custody: Row): void {
    const now = this.#clock();
    const adapterId = text(custody, "adapter_id");
    const actionId = text(custody, "action_id");
    const proof = {
      schemaVersion: 1,
      kind: "agent-custody-pre-dispatch-no-effect",
      adapterId,
      actionId,
      observedAt: new Date(now).toISOString(),
      executionCount: 0,
    };
    const changed = this.#database.prepare(`
      UPDATE provider_actions
         SET status='terminal', history_json='["prepared","terminal"]',
             execution_count=0, effect_count=0, idempotency_proven=1,
             result_json=?, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status='prepared'
    `).run(canonicalJson({ ...proof, evidenceDigest: jsonEvidenceDigest(proof) }), now, adapterId, actionId);
    if (changed.changes !== 1) throw new ProjectFabricCoreError("CONFLICT", "prepared agent custody changed during recovery");
    if (typeof custody.capability_hash === "string") {
      this.#database.prepare("UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL")
        .run(now, custody.capability_hash);
    }
    this.#database.prepare(`
      UPDATE agent_bridge_state
         SET bridge_state='none', capability_hash=NULL, activation_evidence_digest=NULL,
             revision=revision+1, updated_at=?
       WHERE run_id=? AND agent_id=? AND adapter_id=? AND action_id=? AND bridge_state='pending'
    `).run(
      now,
      text(custody, "run_id"),
      text(custody, "target_agent_id"),
      adapterId,
      actionId,
    );
    this.#database.prepare("UPDATE agents SET lifecycle='context-unreconciled' WHERE run_id=? AND agent_id=?")
      .run(text(custody, "run_id"), text(custody, "target_agent_id"));
  }

  #agentLookupResult(handle: AgentDispatchHandle, record: unknown): AgentCustodyResult {
    const expectedOperation = handle.bridgeCapable ? "provision_agent" : "attach";
    const expectedPayload = handle.bridgeCapable
      ? {
          schemaVersion: 1,
          runId: handle.runId,
          operation: handle.operation,
          targetAgentId: handle.targetAgentId,
          authorityId: handle.authorityId,
          bridgeGeneration: handle.bridgeGeneration,
          bridgeContractDigest: handle.bridgeContractDigest,
          payload: handle.publicPayload,
          ...(handle.requestedProviderSessionRef === undefined
            ? {}
            : { providerSessionRef: handle.requestedProviderSessionRef }),
        }
      : {
          resumeReference: handle.requestedProviderSessionRef,
          ...handle.publicPayload,
        };
    if (
      !isRow(record) || record.actionId !== handle.actionId || record.status !== "terminal" ||
      record.operation !== expectedOperation ||
      record.payloadHash !== sha256(canonicalJson(expectedPayload)) ||
      record.executionCount !== 1 || record.effectCount !== 1 || !isRow(record.result)
    ) {
      throw new Error("agent custody lookup is not a terminal one-effect record");
    }
    if (handle.bridgeCapable) {
      const value = record.result;
      if (
        Object.keys(value).length !== 9 ||
        value.schemaVersion !== 1 || value.adapterId !== handle.adapterId ||
        value.actionId !== handle.actionId || value.targetAgentId !== handle.targetAgentId ||
        value.bridgeGeneration !== handle.bridgeGeneration ||
        value.bridgeContractDigest !== handle.bridgeContractDigest
      ) throw new Error("agent custody lookup binding changed");
      return this.#normaliseAgentResult(handle, value);
    }
    const resumeReference = record.result.resumeReference;
    return this.#normaliseAgentResult(handle, {
      providerSessionRef: typeof resumeReference === "string"
        ? resumeReference
        : handle.requestedProviderSessionRef,
      providerSessionGeneration: record.result.providerSessionGeneration ?? 1,
    });
  }

  /**
   * Narrow port of bound closures onto this class's private mutation methods, consumed by
   * `ProviderAgentCustodyRecoveryAdapter` (provider-agent-custody-recovery.ts) so daemon-restart
   * recovery reuses the exact same transaction fences as the live dispatch path.
   */
  recoveryPort(): ProviderAgentCustodyRecoveryPort {
    return {
      agentDispatchHandle: (custody) => this.#agentDispatchHandle(custody),
      failPreparedAgent: (custody) => this.#failPreparedAgent(custody),
      agentLookupResult: (handle, record) => this.#agentLookupResult(handle, record),
      activateAgent: (handle, agentResult) => this.#activateAgent(handle, agentResult),
      persistChildBridgeLoss: (input) => this.#persistChildBridgeLoss(input),
      fenceUnprovenAgent: (handle, evidenceDigest) => this.#fenceUnprovenAgent(handle, evidenceDigest),
    };
  }
}
