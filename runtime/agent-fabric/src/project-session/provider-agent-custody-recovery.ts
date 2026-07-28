import {
  FABRIC_OPERATIONS,
  parseOperationResult,
  type AgentCustodyResult,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { assertProviderActionOwner } from "../application/provider-action-owner.js";
import { integer, isRow, text } from "../persistence/row-codec.js";
import {
  exactDigest,
  jsonEvidenceDigest,
  type AgentDispatchHandle,
  type ProviderAgentCustodyRecoveryPort,
  type ProviderAgentCustodyRecoveryTally,
  type ProviderAgentEffectsPort,
} from "./provider-agent-custody.js";

export type ProviderAgentCustodyRecoveryAdapterOptions = Readonly<{
  database: Database.Database;
  agentEffects: ProviderAgentEffectsPort | undefined;
  custody: ProviderAgentCustodyRecoveryPort;
}>;

/**
 * Byte-moved from `LaunchCustodyService#recoverAgentCustody` (issue #354, S4b), split out of
 * `ProviderAgentCustodyAdapter` (provider-agent-custody.ts) to keep that module under the
 * repository's line-count ceiling. Preserves: lookups outside transactions, settlement inside
 * transactions, and the prepared/observable/active recovery pass ordering. The narrow
 * `ProviderAgentCustodyRecoveryPort` is bound closures onto `ProviderAgentCustodyAdapter`'s
 * private mutation methods so the transaction fences it triggers are byte-identical to before
 * the split.
 */
export class ProviderAgentCustodyRecoveryAdapter {
  readonly #database: Database.Database;
  readonly #agentEffects: ProviderAgentEffectsPort | undefined;
  readonly #custody: ProviderAgentCustodyRecoveryPort;

  constructor(options: ProviderAgentCustodyRecoveryAdapterOptions) {
    this.#database = options.database;
    this.#agentEffects = options.agentEffects;
    this.#custody = options.custody;
  }

  async recoverAgentCustody(result: ProviderAgentCustodyRecoveryTally, errors: unknown[]): Promise<void> {
    const prepared = this.#database.prepare(`
      SELECT c.*, p.payload_json, b.bridge_generation
       FROM provider_agent_custody c
        JOIN provider_actions p
          ON p.run_id=c.run_id AND p.adapter_id=c.adapter_id AND p.action_id=c.action_id
        JOIN agent_bridge_state b ON b.run_id=c.run_id AND b.agent_id=c.target_agent_id
       WHERE p.status='prepared'
         AND NOT EXISTS (
           SELECT 1 FROM lifecycle_rotation_custodies rotation
            WHERE rotation.run_id=c.run_id
              AND (
                (rotation.provider_action_adapter_id=c.adapter_id
                  AND rotation.provider_action_id=c.action_id) OR
                (rotation.source_adapter_id=c.adapter_id
                  AND rotation.source_custody_action_id=c.action_id)
              )
         )
       ORDER BY c.created_at, c.action_id
    `).all().filter(isRow);
    for (const custody of prepared) {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "run_id"),
        adapterId: text(custody, "adapter_id"),
        actionId: text(custody, "action_id"),
      }, "provider_agent");
      try {
        this.#database.transaction(() => this.#custody.failPreparedAgent(custody))();
        result.preparedFailed += 1;
        result.failed += 1;
      } catch (error: unknown) {
        errors.push(error);
        result.ambiguous += 1;
      }
    }

    if (this.#agentEffects !== undefined) {
      const observable = this.#database.prepare(`
        SELECT c.*, p.payload_json, b.bridge_generation
          FROM provider_agent_custody c
          JOIN provider_actions p
            ON p.run_id=c.run_id AND p.adapter_id=c.adapter_id AND p.action_id=c.action_id
          JOIN agent_bridge_state b ON b.run_id=c.run_id AND b.agent_id=c.target_agent_id
         WHERE p.status IN ('dispatched','accepted','ambiguous')
           AND NOT EXISTS (
             SELECT 1 FROM lifecycle_rotation_custodies rotation
              WHERE rotation.run_id=c.run_id
                AND (
                  (rotation.provider_action_adapter_id=c.adapter_id
                    AND rotation.provider_action_id=c.action_id) OR
                  (rotation.source_adapter_id=c.adapter_id
                    AND rotation.source_custody_action_id=c.action_id)
                )
           )
         ORDER BY c.created_at, c.action_id
      `).all().filter(isRow);
      for (const custody of observable) {
        assertProviderActionOwner(this.#database, {
          runId: text(custody, "run_id"),
          adapterId: text(custody, "adapter_id"),
          actionId: text(custody, "action_id"),
        }, "provider_agent");
        let handle: AgentDispatchHandle | undefined;
        let raw: unknown;
        try {
          const currentHandle = this.#custody.agentDispatchHandle(custody);
          handle = currentHandle;
          raw = await this.#agentEffects.lookup({ adapterId: currentHandle.adapterId, actionId: currentHandle.actionId });
          result.lookedUp += 1;
          const custodyResult = this.#custody.agentLookupResult(currentHandle, raw);
          this.#database.transaction(() => {
            this.#custody.activateAgent(currentHandle, custodyResult);
            if (
              currentHandle.bridgeCapable &&
              !this.#agentEffects?.hasRetainedBridge(custodyResult, currentHandle)
            ) {
              this.#custody.persistChildBridgeLoss({
                runId: currentHandle.runId,
                agentId: custodyResult.agentId,
                adapterId: custodyResult.adapterId,
                actionId: custodyResult.actionId,
                providerSessionRef: custodyResult.providerSessionRef,
                providerSessionGeneration: custodyResult.providerSessionGeneration,
                bridgeGeneration: custodyResult.bridgeGeneration,
                reason: "daemon restart found no retained child bridge",
              });
            }
          })();
          if (currentHandle.bridgeCapable) result.recoveryRequired += 1;
          else result.activated += 1;
        } catch (error: unknown) {
          if (handle === undefined) {
            errors.push(error);
            result.ambiguous += 1;
            continue;
          }
          const failedHandle = handle;
          const evidence = jsonEvidenceDigest({
            kind: "agent-custody-lookup-incomplete",
            adapterId: failedHandle.adapterId,
            actionId: failedHandle.actionId,
            error: error instanceof Error ? error.name : "lookup-error",
          });
          try {
            this.#database.transaction(() => this.#custody.fenceUnprovenAgent(failedHandle, evidence))();
          } catch (fenceError: unknown) {
            errors.push(fenceError);
          }
          result.ambiguous += 1;
        }
      }
    }

    const active = this.#database.prepare(`
      SELECT c.*, p.payload_json, b.bridge_generation, b.provider_session_ref,
             b.provider_session_generation, b.activation_evidence_digest
        FROM agent_bridge_state b
        JOIN provider_agent_custody c
          ON c.run_id=b.run_id AND c.adapter_id=b.adapter_id AND c.action_id=b.action_id
        JOIN provider_actions p
          ON p.run_id=c.run_id AND p.adapter_id=c.adapter_id AND p.action_id=c.action_id
       WHERE b.bridge_state='active'
         AND NOT EXISTS (
           SELECT 1 FROM lifecycle_rotation_custodies rotation
            WHERE rotation.run_id=c.run_id
              AND (
                (rotation.provider_action_adapter_id=c.adapter_id
                  AND rotation.provider_action_id=c.action_id) OR
                (rotation.source_adapter_id=c.adapter_id
                  AND rotation.source_custody_action_id=c.action_id)
              )
         )
       ORDER BY c.created_at, c.action_id
    `).all().filter(isRow);
    for (const custody of active) {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "run_id"),
        adapterId: text(custody, "adapter_id"),
        actionId: text(custody, "action_id"),
      }, "provider_agent");
      try {
        const handle = this.#custody.agentDispatchHandle(custody);
        const storedResult = parseOperationResult(
          handle.operation === "spawn" ? FABRIC_OPERATIONS.spawnAgent : FABRIC_OPERATIONS.attachAgent,
          {
            agentId: handle.targetAgentId,
            authorityId: handle.authorityId,
            adapterId: handle.adapterId,
            actionId: handle.actionId,
            providerSessionRef: text(custody, "provider_session_ref"),
            providerSessionGeneration: integer(custody, "provider_session_generation"),
            bridgeState: "active",
            bridgeGeneration: handle.bridgeGeneration,
            evidenceDigest: exactDigest(custody.activation_evidence_digest, "agent activation evidence"),
          },
        ) as AgentCustodyResult;
        if (!this.#agentEffects?.hasRetainedBridge(storedResult, handle)) {
          this.#database.transaction(() => this.#custody.persistChildBridgeLoss({
            runId: handle.runId,
            agentId: storedResult.agentId,
            adapterId: storedResult.adapterId,
            actionId: storedResult.actionId,
            providerSessionRef: storedResult.providerSessionRef,
            providerSessionGeneration: storedResult.providerSessionGeneration,
            bridgeGeneration: storedResult.bridgeGeneration,
            reason: "daemon restart found no retained child bridge",
          }))();
          result.recoveryRequired += 1;
        }
      } catch (error: unknown) {
        errors.push(error);
        result.ambiguous += 1;
      }
    }
  }
}
