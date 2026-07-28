import { Ajv2020 } from "ajv/dist/2020.js";
import { parseLaunchAdapterOutcomeV1 } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "./contracts.js";
import {
  DIGEST,
  exactDigest,
  jsonEvidenceDigest,
  nonEmptyString,
  positiveOutcomeInteger,
} from "./provider-agent-custody.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
  type Row,
} from "../persistence/row-codec.js";
import { assertProviderActionOwner } from "../application/provider-action-owner.js";
import {
  exactRecord,
  isoTimestamp,
  resourceAmounts,
  stale,
  type Digest,
  type LaunchAdapterContract,
  type LaunchAdapterOutcome,
  type LaunchAmbiguous,
  type LaunchOutcomeBase,
  type RetainedChairBridge,
} from "./launch-contracts.js";

/**
 * Byte-moved from `launch-custody.ts` (issue #354, S4e, plan §2 "S4e"): the project-session
 * launch family's settlement and startup recovery. This owns the transactional application of
 * adapter outcomes (activate/fail/ambiguous/overrun), reservation ledger settlement, and the
 * launch-specific portion of custody recovery — `recoverLaunchCustody` is called by
 * `LaunchCustodyService#recover` in the exact original ordering position.
 *
 * The retained-chair-bridge loss fence, previously a direct call into the sibling chair-recovery
 * family, is now reached through the narrow, transaction-safe `chairLoss` port (plan §2 "Inject a
 * transaction-safe chair-loss port").
 */

export type LaunchSettlementAdapterEffectsPort = Readonly<{
  lookup(input: Readonly<{
    providerAdapterId: string;
    providerActionId: string;
    providerContractDigest: Digest;
    attestationChallengeDigest: Digest;
  }>): Promise<unknown>;
  hasRetainedChairBridge?(entry: RetainedChairBridge): boolean;
}>;

export type LaunchChairLossPort = Readonly<{
  observeChairBridgeLoss(input: RetainedChairBridge & Readonly<{ reason: string }>): boolean;
}>;

export type LaunchSettlementOptions = Readonly<{
  database: Database.Database;
  clock: () => number;
  adapterContracts: { inspect(adapterId: string): Promise<LaunchAdapterContract> };
  adapterEffects: LaunchSettlementAdapterEffectsPort;
  chairLoss: LaunchChairLossPort;
}>;

export class LaunchSettlement {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #adapterContracts: LaunchSettlementOptions["adapterContracts"];
  readonly #adapterEffects: LaunchSettlementAdapterEffectsPort;
  readonly #chairLoss: LaunchChairLossPort;

  constructor(options: LaunchSettlementOptions) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#adapterContracts = options.adapterContracts;
    this.#adapterEffects = options.adapterEffects;
    this.#chairLoss = options.chairLoss;
  }

  /**
   * Byte-moved from `LaunchCustodyService#recover` (issue #354, S4e): the launch family's
   * prepared/observable recovery loops. The caller (`LaunchCustodyService#recover`) preserves
   * the exact original ordering: live handoff, chair recovery, provider-agent, launch (this
   * method), then retained-chair audit, before raising the final `AggregateError`.
   */
  async recoverLaunchCustody(result: {
    preparedFailed: number;
    lookedUp: number;
    activated: number;
    failed: number;
    ambiguous: number;
    recoveryRequired: number;
  }, errors: unknown[]): Promise<void> {
    const prepared = this.#database.prepare(`
      SELECT c.*
        FROM project_session_launch_custody c
        JOIN provider_actions p
          ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
       WHERE p.status='prepared'
       ORDER BY c.project_session_id, c.custody_attempt_generation
    `).all().filter(isRow);
    for (const custody of prepared) {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: text(custody, "provider_adapter_id"),
        actionId: text(custody, "provider_action_id"),
      }, "launch");
      try {
        this.#database.transaction(() => this.#failPrepared(custody))();
        result.preparedFailed += 1;
        result.failed += 1;
      } catch (error: unknown) {
        errors.push(error);
        result.ambiguous += 1;
      }
    }

    const observable = this.#database.prepare(`
      SELECT c.*
        FROM project_session_launch_custody c
        JOIN provider_actions p
          ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
       WHERE p.status IN ('dispatched','accepted','ambiguous')
       ORDER BY c.project_session_id, c.custody_attempt_generation
    `).all().filter(isRow);
    for (const custody of observable) {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: text(custody, "provider_adapter_id"),
        actionId: text(custody, "provider_action_id"),
      }, "launch");
      try {
        const providerAdapterId = text(custody, "provider_adapter_id");
        const providerActionId = text(custody, "provider_action_id");
        const providerContractDigest = exactDigest(custody.provider_contract_digest, "custody provider contract digest");
        const attestationChallengeDigest = exactDigest(
          custody.attestation_challenge_digest,
          "custody attestation challenge digest",
        );
        let contract: LaunchAdapterContract;
        try {
          contract = await this.#adapterContracts.inspect(providerAdapterId);
          if (`sha256:${sha256(canonicalJson(contract))}` !== providerContractDigest) {
            throw new Error("launch provider contract changed");
          }
        } catch (error: unknown) {
          const outcome = this.ambiguousOutcome(
            custody,
            "conflict",
            jsonEvidenceDigest(error instanceof Error ? error.message : error),
          );
          const disposition = this.#database.transaction(() => this.applyOutcome(custody, outcome))();
          result[disposition] += 1;
          continue;
        }
        let raw: unknown;
        try {
          raw = await this.#adapterEffects.lookup({
            providerAdapterId,
            providerActionId,
            providerContractDigest,
            attestationChallengeDigest,
          });
        } catch (error: unknown) {
          raw = this.ambiguousOutcome(
            custody,
            "adapter-error",
            jsonEvidenceDigest(error instanceof Error ? error.message : error),
          );
        }
        result.lookedUp += 1;
        const outcome = this.normaliseOutcome(custody, raw, "lookup", contract);
        const disposition = this.#database.transaction(() => this.applyOutcome(custody, outcome))();
        result[disposition] += 1;
      } catch (error: unknown) {
        errors.push(error);
        result.ambiguous += 1;
      }
    }
  }

  #failPrepared(custody: Row): void {
    const adapterId = text(custody, "provider_adapter_id");
    const actionId = text(custody, "provider_action_id");
    const now = this.#clock();
    const proof = {
      schemaVersion: 1,
      kind: "core-pre-dispatch-no-effect",
      providerAdapterId: adapterId,
      providerActionId: actionId,
      observedAt: new Date(now).toISOString(),
      proof: { executionCount: 0, durableStatus: "prepared" },
    };
    const changed = this.#database.prepare(`
      UPDATE provider_actions
         SET status='terminal', history_json='["prepared","terminal"]',
             execution_count=0, effect_count=0, idempotency_proven=1,
             result_json=?, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status='prepared'
    `).run(canonicalJson({ ...proof, digest: jsonEvidenceDigest(proof) }), now, adapterId, actionId);
    if (changed.changes !== 1) stale("prepared launch changed during recovery");
    this.#releaseReservation(text(custody, "reservation_id"));
    this.#terminaliseFailedLaunch(custody, now);
  }

  /**
   * Byte-moved from `LaunchCustodyService#normaliseOutcome`; public so `LaunchService`'s
   * `dispatchPrepared` can normalise the same adapter response shape it observes on the
   * dispatch-return path.
   */
  normaliseOutcome(
    custody: Row,
    value: unknown,
    observationKind: "dispatch-return" | "lookup",
    contract: LaunchAdapterContract,
  ): LaunchAdapterOutcome {
    const raw = value;
    const conflict = (reason: LaunchAmbiguous["outcome"]["reasonCode"]): LaunchAmbiguous =>
      this.ambiguousOutcome(custody, reason, jsonEvidenceDigest(raw), observationKind);
    try {
      value = parseLaunchAdapterOutcomeV1(value);
    } catch {
      return conflict("malformed");
    }
    try {
      const root = exactRecord(value, "launch_adapter_outcome_v1", [
        "schemaVersion", "providerAdapterId", "providerActionId", "providerContractDigest",
        "observationKind", "observedAt", "outcome",
      ]);
      if (
        root.schemaVersion !== 1 ||
        root.providerAdapterId !== text(custody, "provider_adapter_id") ||
        root.providerActionId !== text(custody, "provider_action_id") ||
        root.providerContractDigest !== text(custody, "provider_contract_digest") ||
        root.observationKind !== observationKind
      ) return conflict("conflict");
      const base: LaunchOutcomeBase = {
        schemaVersion: 1,
        providerAdapterId: text(custody, "provider_adapter_id"),
        providerActionId: text(custody, "provider_action_id"),
        providerContractDigest: exactDigest(custody.provider_contract_digest, "custody provider contract digest"),
        observationKind,
        observedAt: isoTimestamp(root.observedAt, "launch_adapter_outcome_v1.observedAt"),
      };
      const tagged = exactRecord(root.outcome, "launch_adapter_outcome_v1.outcome", ["kind"], [
        "providerSessionRef", "providerSessionGeneration", "effectDigest", "resourceUsage",
        "failureCode", "noEffectProof", "reasonCode", "evidenceDigest",
      ]);
      if (tagged.kind === "terminal-success") {
        const success = exactRecord(root.outcome, "launch_adapter_outcome_v1.outcome", [
          "kind", "providerSessionRef", "providerSessionGeneration", "effectDigest", "resourceUsage",
        ]);
        const reservation = row(this.#database.prepare(`
          SELECT amounts_json FROM resource_reservations WHERE reservation_id=?
        `).get(text(custody, "reservation_id")), "launch reservation");
        const expected = resourceAmounts(
          JSON.parse(text(reservation, "amounts_json")) as unknown,
          "launch reservation amounts",
        );
        if (!isRow(success.resourceUsage)) return conflict("conflict");
        if (canonicalJson(Object.keys(success.resourceUsage).sort()) !== canonicalJson(Object.keys(expected).sort())) {
          return conflict("conflict");
        }
        const resourceUsage: Record<string, number | "unknown"> = {};
        for (const [unit, usage] of Object.entries(success.resourceUsage)) {
          if (usage !== "unknown" && (typeof usage !== "number" || !Number.isSafeInteger(usage) || usage < 0)) {
            return conflict("conflict");
          }
          resourceUsage[unit] = usage;
        }
        return {
          ...base,
          outcome: {
            kind: "terminal-success",
            providerSessionRef: nonEmptyString(success.providerSessionRef, "launch outcome providerSessionRef"),
            providerSessionGeneration: positiveOutcomeInteger(success.providerSessionGeneration),
            effectDigest: exactDigest(success.effectDigest, "launch outcome effectDigest"),
            resourceUsage,
          },
        };
      }
      if (tagged.kind === "terminal-no-effect") {
        const failure = exactRecord(root.outcome, "launch_adapter_outcome_v1.outcome", [
          "kind", "failureCode", "noEffectProof",
        ]);
        const proof = exactRecord(failure.noEffectProof, "launch no-effect proof", ["schemaId", "proof", "digest"]);
        if (!isRow(proof.proof)) return conflict("conflict");
        const proofDigest = exactDigest(proof.digest, "launch no-effect proof digest");
        if (jsonEvidenceDigest(proof.proof) !== proofDigest) return conflict("conflict");
        const proofSchemaId = nonEmptyString(proof.schemaId, "launch no-effect proof schema");
        const proofSchema = contract.noEffectProofSchemas[proofSchemaId];
        if (proofSchema === undefined) return conflict("conflict");
        try {
          const validate = new Ajv2020({ allErrors: true, strict: true }).compile(proofSchema);
          if (!validate(proof.proof)) return conflict("conflict");
        } catch {
          return conflict("conflict");
        }
        return {
          ...base,
          outcome: {
            kind: "terminal-no-effect",
            failureCode: nonEmptyString(failure.failureCode, "launch failure code"),
            noEffectProof: {
              schemaId: proofSchemaId,
              proof: proof.proof,
              digest: proofDigest,
            },
          },
        };
      }
      if (tagged.kind === "ambiguous") {
        const ambiguous = exactRecord(root.outcome, "launch_adapter_outcome_v1.outcome", [
          "kind", "reasonCode", "evidenceDigest",
        ]);
        const reason = ambiguous.reasonCode;
        if (
          reason !== "absent" && reason !== "transport-error" && reason !== "adapter-error" &&
          reason !== "malformed" && reason !== "incomplete" && reason !== "conflict" &&
          reason !== "missing-resume-reference"
        ) {
          return conflict("conflict");
        }
        if (ambiguous.evidenceDigest !== null && (typeof ambiguous.evidenceDigest !== "string" || !DIGEST.test(ambiguous.evidenceDigest))) {
          return conflict("conflict");
        }
        return {
          ...base,
          outcome: {
            kind: "ambiguous",
            reasonCode: reason,
            evidenceDigest: ambiguous.evidenceDigest as Digest | null,
          },
        };
      }
      return conflict("conflict");
    } catch (error: unknown) {
      if (error instanceof ProjectFabricCoreError) return conflict("conflict");
      return conflict("malformed");
    }
  }

  /**
   * Byte-moved from `LaunchCustodyService#ambiguousOutcome`; public for `LaunchService`'s
   * dispatch-return path.
   */
  ambiguousOutcome(
    custody: Row,
    reasonCode: LaunchAmbiguous["outcome"]["reasonCode"],
    evidenceDigest: Digest | null,
    observationKind: "dispatch-return" | "lookup" = "lookup",
  ): LaunchAmbiguous {
    return {
      schemaVersion: 1,
      providerAdapterId: text(custody, "provider_adapter_id"),
      providerActionId: text(custody, "provider_action_id"),
      providerContractDigest: exactDigest(custody.provider_contract_digest, "custody provider contract digest"),
      observationKind,
      observedAt: new Date(this.#clock()).toISOString(),
      outcome: { kind: "ambiguous", reasonCode, evidenceDigest },
    };
  }

  /**
   * Byte-moved from `LaunchCustodyService#applyOutcome`; public so both `dispatchPrepared` (via
   * `LaunchService`) and `recoverLaunchCustody` (above) can apply an outcome inside the same
   * transaction boundary the original code used. The retained-chair-bridge loss fence at the end
   * of the success path now goes through the injected `chairLoss` port instead of reaching
   * directly into the sibling chair-recovery family.
   */
  applyOutcome(
    custody: Row,
    outcome: LaunchAdapterOutcome,
  ): "activated" | "failed" | "ambiguous" | "recoveryRequired" {
    const now = this.#clock();
    const adapterId = text(custody, "provider_adapter_id");
    const actionId = text(custody, "provider_action_id");
    assertProviderActionOwner(this.#database, {
      runId: text(custody, "coordination_run_id"),
      adapterId,
      actionId,
    }, "launch");
    const serialized = canonicalJson(outcome);
    if (outcome.outcome.kind === "ambiguous") {
      this.#database.prepare(`
        UPDATE provider_actions
           SET status='ambiguous', history_json='["prepared","dispatched","ambiguous"]',
               result_json=?, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
      `).run(serialized, now, adapterId, actionId);
      this.#database.prepare(`
        UPDATE project_sessions SET state='launch_ambiguous', revision=revision+1, updated_at=?
         WHERE project_session_id=? AND state='launching'
      `).run(now, text(custody, "project_session_id"));
      this.#database.prepare(`
        UPDATE runs SET lifecycle_state='launch_ambiguous', revision=revision+1
         WHERE run_id=? AND lifecycle_state='launching'
      `).run(text(custody, "coordination_run_id"));
      return "ambiguous";
    }
    if (outcome.outcome.kind === "terminal-no-effect") {
      this.#database.prepare(`
        UPDATE provider_actions
           SET status='terminal', history_json='["prepared","dispatched","terminal"]',
               result_json=?, idempotency_proven=1,
               journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
      `).run(serialized, now, adapterId, actionId);
      this.#releaseReservation(text(custody, "reservation_id"));
      this.#terminaliseFailedLaunch(custody, now);
      return "failed";
    }

    const settlement = this.#settleSuccessfulReservation(
      text(custody, "reservation_id"),
      outcome.outcome.resourceUsage,
    );
    this.#database.prepare(`
      UPDATE provider_actions
         SET status='terminal', history_json='["prepared","dispatched","accepted","terminal"]',
             effect_count=1, idempotency_proven=1, provider_session_generation=?,
             result_json=?, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
    `).run(
      outcome.outcome.providerSessionGeneration,
      serialized,
      now,
      adapterId,
      actionId,
    );
    if (settlement === "overrun") {
      const frozen = this.#database.prepare(`
        UPDATE run_chair_leases SET status='frozen',updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND status='active'
      `).run(
        now,
        text(custody, "project_session_id"),
        text(custody, "coordination_run_id"),
        text(custody, "chair_lease_id"),
      );
      if (frozen.changes !== 1) stale("launch overrun chair lease changed");
      this.#database.prepare(`
        UPDATE project_sessions SET state='recovery_required', revision=revision+1, updated_at=?
         WHERE project_session_id=? AND state IN ('launching','launch_ambiguous')
      `).run(now, text(custody, "project_session_id"));
      this.#database.prepare(`
        UPDATE runs SET lifecycle_state='recovery_required', revision=revision+1
         WHERE run_id=? AND lifecycle_state IN ('launching','launch_ambiguous')
      `).run(text(custody, "coordination_run_id"));
      return "recoveryRequired";
    }
    this.#database.prepare(`
      UPDATE agents SET provider_session_ref=?, lifecycle='ready'
       WHERE run_id=? AND agent_id=?
    `).run(
      outcome.outcome.providerSessionRef,
      text(custody, "coordination_run_id"),
      text(custody, "chair_agent_id"),
    );
    this.#database.prepare(`
      INSERT INTO provider_state(
        run_id, agent_id, provider_session_generation, context_revision, reconciled_checkpoint_sha256
      ) VALUES (?, ?, ?, NULL, NULL)
      ON CONFLICT(run_id, agent_id) DO UPDATE SET
        provider_session_generation=excluded.provider_session_generation,
        context_revision=NULL,
        reconciled_checkpoint_sha256=NULL
    `).run(
      text(custody, "coordination_run_id"),
      text(custody, "chair_agent_id"),
      outcome.outcome.providerSessionGeneration,
    );
    this.#database.prepare(`
      INSERT INTO launched_chair_bridge_state(
        project_session_id, coordination_run_id, chair_agent_id,
        provider_adapter_id, provider_action_id, provider_contract_digest, provider_session_ref,
        provider_session_generation, principal_generation, bridge_generation,
        capability_hash, activation_evidence_digest, state, revision, created_at, updated_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, 1, ?, ?, 'active', 1, ?, ?)
    `).run(
      text(custody, "project_session_id"),
      text(custody, "coordination_run_id"),
      text(custody, "chair_agent_id"),
      adapterId,
      actionId,
      text(custody, "provider_contract_digest"),
      outcome.outcome.providerSessionRef,
      outcome.outcome.providerSessionGeneration,
      text(custody, "capability_hash"),
      outcome.outcome.effectDigest,
      now,
      now,
    );
    this.#database.prepare(`
      UPDATE project_sessions
         SET state='active', membership_revision=membership_revision+1,
             revision=revision+1, updated_at=?
       WHERE project_session_id=? AND state IN ('launching','launch_ambiguous')
    `).run(now, text(custody, "project_session_id"));
    this.#database.prepare(`
      UPDATE runs SET lifecycle_state='active', revision=revision+1
       WHERE run_id=? AND lifecycle_state IN ('launching','launch_ambiguous')
    `).run(text(custody, "coordination_run_id"));
    this.#database.prepare(`
      UPDATE project_session_memberships
         SET state='reconciled', revision=revision+1, updated_at=?
       WHERE project_session_id=? AND coordination_run_id=?
         AND member_kind='provider-action' AND member_adapter_id=? AND member_id=? AND state='active'
    `).run(
      now,
      text(custody, "project_session_id"),
      text(custody, "coordination_run_id"),
      adapterId,
      actionId,
    );
    const retainedEntry: RetainedChairBridge = {
      projectSessionId: text(custody, "project_session_id"),
      runId: text(custody, "coordination_run_id"),
      agentId: text(custody, "chair_agent_id"),
      principalGeneration: 1,
      adapterId,
      actionId,
      providerSessionRef: outcome.outcome.providerSessionRef,
      providerSessionGeneration: outcome.outcome.providerSessionGeneration,
      bridgeGeneration: 1,
    };
    if (
      this.#adapterEffects.hasRetainedChairBridge !== undefined &&
      !this.#adapterEffects.hasRetainedChairBridge(retainedEntry)
    ) {
      this.#chairLoss.observeChairBridgeLoss({
        ...retainedEntry,
        reason: "retained chair bridge closed during activation commit",
      });
      return "recoveryRequired";
    }
    return "activated";
  }

  #terminaliseFailedLaunch(custody: Row, now: number): void {
    this.#database.prepare("UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL")
      .run(now, text(custody, "capability_hash"));
    this.#database.prepare(`
      UPDATE run_chair_leases SET status='revoked', updated_at=?
       WHERE lease_id=? AND status IN ('active','frozen')
    `).run(now, text(custody, "chair_lease_id"));
    this.#database.prepare(`
      UPDATE agents SET lifecycle='suspended' WHERE run_id=? AND agent_id=?
    `).run(text(custody, "coordination_run_id"), text(custody, "chair_agent_id"));
    this.#database.prepare(`
      UPDATE project_sessions
         SET state='launch_failed', membership_revision=membership_revision+1,
             revision=revision+1, updated_at=?
       WHERE project_session_id=? AND state IN ('launching','launch_ambiguous')
    `).run(now, text(custody, "project_session_id"));
    this.#database.prepare(`
      UPDATE runs SET lifecycle_state='launch_failed', revision=revision+1
       WHERE run_id=? AND lifecycle_state IN ('launching','launch_ambiguous')
    `).run(text(custody, "coordination_run_id"));
    this.#database.prepare(`
      UPDATE project_session_memberships
         SET state=CASE
               WHEN member_kind IN ('coordination-run','lease') THEN 'abandoned'
               ELSE 'reconciled'
             END,
             abandoned_reason=CASE
               WHEN member_kind IN ('coordination-run','lease') THEN 'launch-failed'
               ELSE NULL
             END,
             revision=revision+1, updated_at=?
       WHERE project_session_id=? AND coordination_run_id=? AND state='active'
    `).run(now, text(custody, "project_session_id"), text(custody, "coordination_run_id"));
  }

  #releaseReservation(reservationId: string): void {
    const dimensions = this.#database.prepare(`
      SELECT scope_id, unit_key, amount, consumed, released
        FROM resource_reservation_dimensions WHERE reservation_id=?
    `).all(reservationId).filter(isRow);
    for (const dimension of dimensions) {
      const remainder = integer(dimension, "amount") - integer(dimension, "consumed") - integer(dimension, "released");
      const changed = this.#database.prepare(`
        UPDATE resource_dimensions SET reserved=reserved-?
         WHERE scope_id=? AND unit_key=? AND reserved>=?
      `).run(remainder, text(dimension, "scope_id"), text(dimension, "unit_key"), remainder);
      if (changed.changes !== 1) throw new Error("launch reservation release ledger changed");
      this.#database.prepare(`
        UPDATE resource_reservation_dimensions SET released=released+?
         WHERE reservation_id=? AND scope_id=? AND unit_key=?
      `).run(remainder, reservationId, text(dimension, "scope_id"), text(dimension, "unit_key"));
    }
    this.#database.prepare(`
      UPDATE resource_reservations SET state='released', revision=revision+1, updated_at=?
       WHERE reservation_id=?
    `).run(this.#clock(), reservationId);
  }

  #settleSuccessfulReservation(
    reservationId: string,
    usage: Readonly<Record<string, number | "unknown">>,
  ): "settled" | "overrun" {
    const reservation = row(this.#database.prepare(`
      SELECT amounts_json FROM resource_reservations WHERE reservation_id=?
    `).get(reservationId), "launch reservation");
    const amounts = JSON.parse(text(reservation, "amounts_json")) as Record<string, number>;
    if (Object.entries(usage).some(([unit, consumed]) => consumed !== "unknown" && consumed > (amounts[unit] ?? -1))) {
      return "overrun";
    }
    const dimensions = this.#database.prepare(`
      SELECT scope_id, unit_key, amount FROM resource_reservation_dimensions WHERE reservation_id=?
    `).all(reservationId).filter(isRow);
    for (const dimension of dimensions) {
      const unit = text(dimension, "unit_key");
      const amount = integer(dimension, "amount");
      const consumed = usage[unit];
      if (consumed === undefined) throw new Error("validated launch usage dimension is missing");
      if (consumed === "unknown") {
        const changed = this.#database.prepare(`
          UPDATE resource_dimensions SET reserved=reserved-?, usage_unknown=1
           WHERE scope_id=? AND unit_key=? AND reserved>=?
        `).run(amount, text(dimension, "scope_id"), unit, amount);
        if (changed.changes !== 1) throw new Error("launch unknown-usage ledger changed");
        this.#database.prepare(`
          UPDATE resource_reservation_dimensions SET usage_unknown=1
           WHERE reservation_id=? AND scope_id=? AND unit_key=?
        `).run(reservationId, text(dimension, "scope_id"), unit);
      } else {
        const changed = this.#database.prepare(`
          UPDATE resource_dimensions SET reserved=reserved-?, used=used+?
           WHERE scope_id=? AND unit_key=? AND reserved>=?
        `).run(amount, consumed, text(dimension, "scope_id"), unit, amount);
        if (changed.changes !== 1) throw new Error("launch usage ledger changed");
        this.#database.prepare(`
          UPDATE resource_reservation_dimensions SET consumed=?, released=?
           WHERE reservation_id=? AND scope_id=? AND unit_key=?
        `).run(consumed, amount - consumed, reservationId, text(dimension, "scope_id"), unit);
      }
    }
    this.#database.prepare(`
      UPDATE resource_reservations SET state='reconciled', revision=revision+1, updated_at=?
       WHERE reservation_id=?
    `).run(this.#clock(), reservationId);
    return "settled";
  }
}
