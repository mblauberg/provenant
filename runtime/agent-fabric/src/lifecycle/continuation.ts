import type Database from "better-sqlite3";

import { assertProviderActionOwner, ProviderActionOwnerError } from "../application/provider-action-owner.js";
import { canonicalJson, stringDigest as sha256Digest, row as rowOrNotFound, text as stringField } from "../persistence/row-codec.js";
import { type LifecycleContinuationInput } from "./admission.js";
import { type LifecycleCustodyHead, type LifecycleRotationRepository } from "./rotation-repository.js";
import type { LifecycleFinalizer } from "./finalizer.js";
import type { AdapterSupervisor } from "../adapters/supervisor.js";

function privateSafeErrorMessage(error: unknown, privateValues: readonly string[], fallback: string): string {
  const message = error instanceof Error ? error.message : String(error);
  return privateValues.some((value) => message.includes(value)) ? fallback : message;
}

// Behaviour-preserving extraction of Fabric's private #scheduleLifecycleContinuation and
// #continueLifecycleRotation (plus the #lifecycleContinuations map they share). Bodies are
// unchanged, including the cross-map GENERIC-predecessor read and its ProviderActionOwnerError
// swallow: both are LIVE behaviour reachable via the non-injective NUL-delimited custody key
// (issue #362) and are deliberately preserved byte-identical. Calls back into Fabric-private
// state that stays behind (the generic-action ownership map, the closing flag, and event
// emission) are narrow injected function ports bound to the same Fabric instance, so observed
// behaviour is identical. This module owns the continuation map; its `size`/`pending()` surface
// is what Fabric's close() fixpoint (fabric.ts ~1422/1428) polls and drains.
export class LifecycleContinuation {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #lifecycleRotations: LifecycleRotationRepository;
  readonly #adapterSupervisor: AdapterSupervisor;
  readonly #finalizer: LifecycleFinalizer;
  readonly #fabricSocketPath: string | undefined;
  readonly #ensureReceiptScope: (runId: string, agentId: string) => Promise<void>;
  readonly #getGenericPredecessor: (runId: string, adapterId: string, actionId: string) => Promise<void> | undefined;
  readonly #isClosing: () => boolean;
  readonly #event: (runId: string, type: string, actorAgentId: string | null, payload: unknown) => void;
  readonly #continuations = new Map<string, Promise<void>>();

  constructor(dependencies: Readonly<{
    database: Database.Database;
    clock: () => number;
    lifecycleRotations: LifecycleRotationRepository;
    adapterSupervisor: AdapterSupervisor;
    finalizer: LifecycleFinalizer;
    fabricSocketPath: string | undefined;
    ensureReceiptScope: (runId: string, agentId: string) => Promise<void>;
    getGenericPredecessor: (runId: string, adapterId: string, actionId: string) => Promise<void> | undefined;
    isClosing: () => boolean;
    event: (runId: string, type: string, actorAgentId: string | null, payload: unknown) => void;
  }>) {
    this.#database = dependencies.database;
    this.#clock = dependencies.clock;
    this.#lifecycleRotations = dependencies.lifecycleRotations;
    this.#adapterSupervisor = dependencies.adapterSupervisor;
    this.#finalizer = dependencies.finalizer;
    this.#fabricSocketPath = dependencies.fabricSocketPath;
    this.#ensureReceiptScope = dependencies.ensureReceiptScope;
    this.#getGenericPredecessor = dependencies.getGenericPredecessor;
    this.#isClosing = dependencies.isClosing;
    this.#event = dependencies.event;
  }

  /** Live size of the continuation map, for Fabric's close() fixpoint while-condition. */
  get size(): number {
    return this.#continuations.size;
  }

  /** Live snapshot of in-flight continuation promises, for Fabric's close() Promise.allSettled. */
  pending(): IterableIterator<Promise<void>> {
    return this.#continuations.values();
  }

  schedule(input: LifecycleContinuationInput): void {
    assertProviderActionOwner(this.#database, {
      runId: input.runId,
      adapterId: input.adapterId,
      actionId: input.actionId,
    }, "lifecycle");
    const key = `lifecycle\0${input.runId}\0${input.agentId}\0${input.custodyId}`;
    if (this.#continuations.has(key)) return;
    const predecessor = this.#getGenericPredecessor(input.runId, input.adapterId, input.callerActionId);
    const continuation = (async () => {
      if (predecessor !== undefined) await predecessor;
      if (!this.#isClosing()) await this.#continueLifecycleRotation(input);
    })();
    this.#continuations.set(key, continuation);
    void continuation.catch((error: unknown) => {
      if (error instanceof ProviderActionOwnerError) return;
      this.#event(input.runId, "lifecycle-continuation-failed", input.agentId, {
        custodyId: input.custodyId,
        message: privateSafeErrorMessage(
          error,
          [input.launchAttestationChallenge],
          "lifecycle replacement provider failed",
        ),
      });
    }).finally(() => {
      if (this.#continuations.get(key) === continuation) this.#continuations.delete(key);
    });
  }

  async #continueLifecycleRotation(input: LifecycleContinuationInput): Promise<void> {
    assertProviderActionOwner(this.#database, {
      runId: input.runId,
      adapterId: input.adapterId,
      actionId: input.actionId,
    }, "lifecycle");
    await this.#ensureReceiptScope(input.runId, input.agentId);
    let head: LifecycleCustodyHead = this.#database.transaction(() => this.#lifecycleRotations.appendInCurrentTransaction({
      runId: input.runId,
      agentId: input.agentId,
      custodyId: input.custodyId,
      expectedRevision: 1,
      state: "prepared",
      recordedAt: this.#clock(),
    })).immediate();
    this.#database.transaction(() => {
      this.#database.prepare(`
        INSERT INTO capabilities(token_hash,run_id,agent_id,principal_generation,expires_at)
        VALUES (?,?,?,?,?)
      `).run(
        input.stagedCapabilityHash,
        input.runId,
        input.agentId,
        input.targetPrincipalGeneration,
        input.capabilityExpiresAt,
      );
      this.#database.prepare(`
        INSERT INTO provider_agent_custody(
          run_id,action_id,operation,actor_agent_id,target_agent_id,authority_id,
          adapter_id,bridge_contract_digest,bridge_capable,capability_hash,
          capability_expires_at,principal_generation,requested_provider_session_ref,
          intent_digest,created_at
        ) VALUES (?,?, 'spawn',?,?,?,?,?,1,?,?,?,?,?,?)
      `).run(
        input.runId, input.actionId, input.agentId, input.agentId,
        input.authorityId, input.adapterId, input.bridgeContractDigest,
        input.stagedCapabilityHash, input.capabilityExpiresAt,
        input.targetPrincipalGeneration, null,
        sha256Digest(canonicalJson(input.providerPayload)), this.#clock(),
      );
      assertProviderActionOwner(this.#database, {
        runId: input.runId,
        adapterId: input.adapterId,
        actionId: input.actionId,
      }, "lifecycle");
      const dispatched = this.#database.prepare(`
        UPDATE provider_actions
           SET status='dispatched',history_json='["prepared","dispatched"]',
               execution_count=1,updated_at=?
         WHERE run_id=? AND adapter_id=? AND action_id=? AND status='prepared'
      `).run(this.#clock(), input.runId, input.adapterId, input.actionId);
      if (dispatched.changes !== 1) throw new Error("lifecycle replacement dispatch claim failed");
      head = this.#lifecycleRotations.appendInCurrentTransaction({
        runId: input.runId,
        agentId: input.agentId,
        custodyId: input.custodyId,
        expectedRevision: head.revision,
        state: "dispatched",
        recordedAt: this.#clock(),
      });
    }).immediate();
    assertProviderActionOwner(this.#database, {
      runId: input.runId,
      adapterId: input.adapterId,
      actionId: input.actionId,
    }, "lifecycle");
    const result = await this.#adapterSupervisor.provisionAgent(input.adapterId, {
      schemaVersion: 1,
      runId: input.runId,
      operation: "spawn",
      actionId: input.actionId,
      targetAgentId: input.agentId,
      authorityId: input.authorityId,
      bridgeGeneration: input.targetBridgeGeneration,
      bridgeContractDigest: input.bridgeContractDigest,
      payload: input.providerPayload,
      lifecycleAttestation: {
        custodyId: input.custodyId,
        checkpointDigest: `sha256:${input.checkpointSha256}`,
        challengeDigest: input.launchAttestationChallengeDigest,
      },
    }, {
      capability: input.stagedCapability,
      socketPath: this.#fabricSocketPath as string,
      expectedPrincipal: {
        agentId: input.agentId,
        projectSessionId: stringField(
          rowOrNotFound(
            this.#database.prepare("SELECT project_session_id FROM runs WHERE run_id=?").get(input.runId),
            "lifecycle rotation run",
          ),
          "project_session_id",
        ),
        runId: input.runId,
        principalGeneration: input.targetPrincipalGeneration,
      },
      lifecycleAttestation: {
        challenge: input.launchAttestationChallenge,
        custodyId: input.custodyId,
        checkpointDigest: `sha256:${input.checkpointSha256}`,
        challengeDigest: input.launchAttestationChallengeDigest,
      },
    });
    assertProviderActionOwner(this.#database, {
      runId: input.runId,
      adapterId: input.adapterId,
      actionId: input.actionId,
    }, "lifecycle");
    if (result.providerSessionGeneration !== input.targetProviderGeneration) {
      const proof = {
        schemaVersion: 1,
        kind: "integrity-quarantine",
        sourceState: head.state,
        reason: "provider-result-reserved-generation-crossed",
        providerActionRef: { runId: input.runId, adapterId: input.adapterId, actionId: input.actionId },
        evidenceDigest: sha256Digest(canonicalJson({
          expectedProviderSessionGeneration: input.targetProviderGeneration,
          observedProviderSessionGeneration: result.providerSessionGeneration,
        })),
      };
      await this.#finalizer.finalizeRotationAdopted(
        input,
        head,
        sha256Digest(canonicalJson(proof)),
        null,
        {
          disposition: "quarantined",
          proofKind: "integrity-quarantine",
          transitionProof: proof,
        },
      );
      return;
    }
    const terminalEvidenceDigest = sha256Digest(canonicalJson(result));
    this.#database.transaction(() => {
      assertProviderActionOwner(this.#database, {
        runId: input.runId,
        adapterId: input.adapterId,
        actionId: input.actionId,
      }, "lifecycle");
      const terminalized = this.#database.prepare(`
        UPDATE provider_actions
           SET status='terminal',history_json='["prepared","dispatched","accepted","terminal"]',
               effect_count=1,idempotency_proven=1,result_json=?,updated_at=?
         WHERE run_id=? AND adapter_id=? AND action_id=? AND status='dispatched'
      `).run(canonicalJson(result), this.#clock(), input.runId, input.adapterId, input.actionId);
      if (terminalized.changes !== 1) throw new Error("lifecycle replacement action changed before terminal commit");
      head = this.#lifecycleRotations.appendInCurrentTransaction({
        runId: input.runId,
        agentId: input.agentId,
        custodyId: input.custodyId,
        expectedRevision: head.revision,
        state: "provider-terminal",
        terminalEvidenceDigest,
        recordedAt: this.#clock(),
      });
      head = this.#lifecycleRotations.appendInCurrentTransaction({
        runId: input.runId,
        agentId: input.agentId,
        custodyId: input.custodyId,
        expectedRevision: head.revision,
        state: "committing",
        terminalEvidenceDigest,
        recordedAt: this.#clock(),
      });
    }).immediate();
    await this.#finalizer.finalizeRotationAdopted(input, head, terminalEvidenceDigest, result);
  }
}
