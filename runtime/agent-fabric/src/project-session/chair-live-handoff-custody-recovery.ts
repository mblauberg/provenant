import type { ChairLiveHandoffIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { assertProviderActionOwner } from "../application/provider-action-owner.js";
import { isRow, text } from "../persistence/row-codec.js";
import { jsonEvidenceDigest } from "./provider-agent-custody.js";
import type {
  ChairLiveHandoffCustodyRecoveryPort,
  ChairLiveHandoffDispatchHandle,
} from "./chair-live-handoff-custody.js";

type Digest = Sha256Digest;

export type ChairLiveHandoffCustodyRecoveryAdapterOptions = Readonly<{
  database: Database.Database;
  custody: ChairLiveHandoffCustodyRecoveryPort;
}>;

export type ChairLiveHandoffRecoveryTally = {
  preparedFailed: number;
  lookedUp: number;
  activated: number;
  failed: number;
  ambiguous: number;
  recoveryRequired: number;
};

/**
 * Byte-moved from `LaunchCustodyService#recover()`'s chair-live-handoff phase
 * (`#recoverChairLiveHandoffCustody`), split out of `ChairLiveHandoffCustodyAdapter`
 * (chair-live-handoff-custody.ts) to keep that module under the repository's line-count ceiling
 * — same split pattern as `ProviderAgentCustodyRecoveryAdapter` for provider-agent-custody.ts
 * (S4b). Preserves: mark every prepared-before-restart custody row no-effect first, then
 * reconcile every dispatched/ambiguous row, tallying and continuing past per-row errors exactly
 * as before. The narrow `ChairLiveHandoffCustodyRecoveryPort` is bound closures onto
 * `ChairLiveHandoffCustodyAdapter`'s private mutation methods so the transaction fences it
 * triggers are byte-identical to before the split.
 */
export class ChairLiveHandoffCustodyRecoveryAdapter {
  readonly #database: Database.Database;
  readonly #custody: ChairLiveHandoffCustodyRecoveryPort;

  constructor(options: ChairLiveHandoffCustodyRecoveryAdapterOptions) {
    this.#database = options.database;
    this.#custody = options.custody;
  }

  async recoverChairLiveHandoffCustody(
    result: ChairLiveHandoffRecoveryTally,
    errors: unknown[],
  ): Promise<void> {
    const prepared = this.#database.prepare(`
      SELECT * FROM chair_live_handoff_custody WHERE state='prepared'
       ORDER BY created_at, custody_id
    `).all().filter(isRow);
    for (const custody of prepared) {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: text(custody, "provider_adapter_id"),
        actionId: text(custody, "promotion_action_id"),
      }, "chair_live_handoff");
      try {
        const intentValue: unknown = JSON.parse(text(custody, "intent_json"));
        if (!isRow(intentValue) || intentValue.kind !== "chair-live-handoff") {
          throw new Error("stored prepared chair live handoff intent is invalid");
        }
        const handle: ChairLiveHandoffDispatchHandle = {
          schemaVersion: 1,
          custodyId: text(custody, "custody_id"),
          promotionActionId: text(custody, "promotion_action_id"),
          intent: intentValue as ChairLiveHandoffIntent,
          intentDigest: text(custody, "intent_digest") as Digest,
          inspectionDigest: jsonEvidenceDigest({ custodyId: text(custody, "custody_id") }),
          operatorId: text(custody, "operator_id"),
          operatorCommandId: text(custody, "operator_command_id"),
        };
        this.#custody.markNoEffect(handle, "prepared-before-restart");
        result.preparedFailed += 1;
        result.failed += 1;
      } catch (error: unknown) {
        errors.push(error);
        result.ambiguous += 1;
        result.recoveryRequired += 1;
      }
    }
    const observable = this.#database.prepare(`
      SELECT * FROM chair_live_handoff_custody
       WHERE state IN ('dispatched','ambiguous')
       ORDER BY created_at, custody_id
    `).all().filter(isRow);
    for (const custody of observable) {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: text(custody, "provider_adapter_id"),
        actionId: text(custody, "promotion_action_id"),
      }, "chair_live_handoff");
      try {
        const reconciled = await this.#custody.reconcile(
          text(custody, "operator_id"),
          text(custody, "operator_command_id"),
        );
        result.lookedUp += 1;
        if (reconciled.status === "committed") result.activated += 1;
        else if (reconciled.status === "no-effect") result.failed += 1;
        else {
          result.ambiguous += 1;
          result.recoveryRequired += 1;
        }
      } catch (error: unknown) {
        errors.push(error);
        result.ambiguous += 1;
        result.recoveryRequired += 1;
      }
    }
  }
}
