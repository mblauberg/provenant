import type {
  ArtifactRef,
  OperatorActionIntent,
} from "@local/agent-fabric-protocol";
import { parseArtifactRef } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { supersedeFinalAcceptanceGates } from "../project-session/acceptance-cycle.js";
import { retireProjectSessionBridges } from "../project-session/bridge-retirement.js";
import { canonicalJson, sha256 } from "../persistence/row-codec.js";
import type { OperatorEffectOutcome, OperatorEffectRequest } from "./action-store.js";

export type Row = Record<string, unknown>;

export type EffectScope = {
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  principalGeneration: number;
  operation: string;
};

export type ProjectDaemonLifecycleDaemonStopPort = {
  request(input: Readonly<{
    custodyId: string;
    resultCorrelationDigest: string;
    operatorId: string;
    projectId: string;
    projectSessionId: string;
    principalGeneration: number;
    commandId: string;
    operation: "daemon-stop";
    token: { daemonInstanceGeneration: number; observedGlobalStateRevision: number };
  }>): Promise<"stopped" | "scheduled" | "busy">;
};

/**
 * Structural mirror of `GlobalLivenessSnapshot` from `../lifecycle/global-liveness.js`, duplicated
 * here (rather than imported) so this module depends only on the narrow liveness callback. The
 * facade owns the concrete lifecycle import and injects a callback built from the real function.
 */
export type GlobalLivenessSnapshotLike = {
  idle: boolean;
  failClosed: boolean;
  failure: "unknown-state" | "query-failed" | null;
  globalStateRevision: number | null;
  contributors: unknown;
};

export type GlobalLivenessReader = (input: Readonly<{
  now: number;
  daemonInstanceGeneration: number;
  excludeOperatorEffectCustodyId?: string;
}>) => GlobalLivenessSnapshotLike;

export interface ProjectDaemonLifecycleHostPort {
  effectScope(request: OperatorEffectRequest): EffectScope;
  custodyId(scope: EffectScope, commandId: string): string;
  effectCustody(scope: EffectScope, commandId: string): Row | null;
  custodyEffectRef(custody: Row): ArtifactRef;
  storeCustodyOutcome(scope: EffectScope, commandId: string, outcome: OperatorEffectOutcome): void;
}

function isRow(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function row(value: unknown, label: string): Row {
  if (!isRow(value)) throw new ProjectFabricCoreError("NOT_FOUND", `${label} was not found`);
  return value;
}

function text(value: Row, field: string): string {
  const candidate = value[field];
  if (typeof candidate !== "string") throw new Error(`${field} is not text`);
  return candidate;
}

function integer(value: Row, field: string): number {
  const candidate = value[field];
  if (typeof candidate !== "number" || !Number.isSafeInteger(candidate)) throw new Error(`${field} is not an integer`);
  return candidate;
}

/**
 * Byte-moved from `ProductionOperatorActions` (S4f): the project/daemon operator lifecycle family
 * — `#drainProject`, `#stopProject`, `#drainDaemon`, `#finishDaemonDrain`, `#stopDaemon`,
 * `#projectObligations`, `#globalRevision`, `#persistLifecycleReceipt`, `#lifecycleReceipt`, plus
 * the `project-session-drain`/`project-session-stop`/`daemon-drain`/`daemon-stop` branches of
 * `#read`, `#dispatchOwned`, and `#observeOwned`. Preserves: drain obligation reads and receipt
 * persistence ordered exactly as before; project stop bridge retirement and outcome commit inside
 * the same transaction as the terminal `project_sessions` update; daemon receipt/epoch/global-revision
 * checks all occur before daemon-stop adapter I/O; custody settlement after daemon-stop I/O remains
 * outside any transaction, matching the original ordering. The facade still owns `#custodyId`,
 * `#effectScope`, `#effectCustody`, `#custodyEffectRef`, and `#storeCustodyOutcome` — injected here
 * as a narrow host port — and still owns the `../lifecycle/global-liveness.js` import, injected here as
 * a narrow liveness callback.
 */
export class ProjectDaemonLifecycleActions {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #host: ProjectDaemonLifecycleHostPort;
  readonly #liveness: GlobalLivenessReader;
  readonly #daemonStop: ProjectDaemonLifecycleDaemonStopPort | undefined;
  readonly #retireVolatileProjectSession: ((projectSessionId: string) => void) | undefined;
  readonly #fault: (label: string) => void;

  constructor(options: {
    database: Database.Database;
    clock: () => number;
    host: ProjectDaemonLifecycleHostPort;
    liveness: GlobalLivenessReader;
    daemonStop?: ProjectDaemonLifecycleDaemonStopPort;
    retireVolatileProjectSession?: (projectSessionId: string) => void;
    fault?: (label: string) => void;
  }) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#host = options.host;
    this.#liveness = options.liveness;
    this.#daemonStop = options.daemonStop;
    this.#retireVolatileProjectSession = options.retireVolatileProjectSession;
    this.#fault = options.fault ?? (() => undefined);
  }

  readProjectSessionLifecycle(
    intent: Extract<OperatorActionIntent, { kind: "project-session-drain" | "project-session-stop" }>,
  ): {
    kind: "project-session-lifecycle";
    revision: number;
    sessionGeneration: number;
    globalStateRevision: number;
    lifecycleState: string;
    drainReceiptRef: ArtifactRef | null;
  } {
    const session = row(this.#database.prepare(`
      SELECT revision, generation, state FROM project_sessions WHERE project_session_id=?
    `).get(intent.projectSessionId), "project session lifecycle");
    return {
      kind: "project-session-lifecycle",
      revision: integer(session, "revision"),
      sessionGeneration: integer(session, "generation"),
      globalStateRevision: this.#globalRevision(),
      lifecycleState: text(session, "state"),
      drainReceiptRef: intent.kind === "project-session-stop" ? intent.drainReceiptRef : null,
    };
  }

  readDaemonLifecycle(
    intent: Extract<OperatorActionIntent, { kind: "daemon-drain" | "daemon-stop" }>,
  ): {
    kind: "daemon-lifecycle";
    revision: number;
    daemonGeneration: number;
    globalStateRevision: number;
    lifecycleState: string;
    drainReceiptRef: ArtifactRef | null;
  } {
    const epoch = row(this.#database.prepare(`
      SELECT instance_generation, state FROM daemon_runtime_epochs
       WHERE instance_generation=?
    `).get(intent.expectedDaemonGeneration), "daemon runtime epoch");
    const globalStateRevision = this.#globalRevision();
    return {
      kind: "daemon-lifecycle",
      revision: globalStateRevision,
      daemonGeneration: integer(epoch, "instance_generation"),
      globalStateRevision,
      lifecycleState: text(epoch, "state"),
      drainReceiptRef: intent.kind === "daemon-stop" ? intent.drainReceiptRef : null,
    };
  }

  drainProject(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-drain" }> },
  ): OperatorEffectOutcome {
    const intent = request.intent;
    this.#database.transaction(() => {
      const session = row(this.#database.prepare(`
        SELECT revision, generation, state FROM project_sessions WHERE project_session_id=?
      `).get(intent.projectSessionId), "project session lifecycle");
      if (
        integer(session, "revision") !== intent.expectedSessionRevision ||
        integer(session, "generation") !== intent.expectedSessionGeneration ||
        this.#globalRevision() !== intent.expectedGlobalStateRevision
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project drain authority changed");
      }
      if (!["active", "visibility_degraded"].includes(text(session, "state"))) {
        throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "project drain requires an active session");
      }
      const changed = this.#database.prepare(`
        UPDATE project_sessions
           SET state='quiescing', revision=revision+1, updated_at=?
         WHERE project_session_id=? AND revision=? AND generation=?
      `).run(this.#clock(), intent.projectSessionId, intent.expectedSessionRevision, intent.expectedSessionGeneration);
      if (changed.changes !== 1) throw new ProjectFabricCoreError("STALE_REVISION", "project drain raced another transition");
      const runs = this.#database.prepare(`
        SELECT run_id, revision FROM runs
         WHERE project_session_id=? AND lifecycle_state NOT IN ('closed','cancelled','launch_failed')
         ORDER BY run_id
      `).all(intent.projectSessionId) as Row[];
      for (const run of runs) {
        this.#database.prepare(`
          UPDATE runs SET lifecycle_state='quiescing', revision=revision+1
           WHERE run_id=? AND revision=?
        `).run(text(run, "run_id"), integer(run, "revision"));
      }
    })();
    const session = row(this.#database.prepare(`
      SELECT revision, generation FROM project_sessions WHERE project_session_id=?
    `).get(intent.projectSessionId), "drained project session");
    const obligations = this.#projectObligations(
      intent.projectSessionId,
      this.#host.custodyId(this.#host.effectScope(request), request.commandId),
    );
    if (!obligations.settled) return { status: "pending", phase: "accepted" };
    const receipt = this.#persistLifecycleReceipt({
      scope: this.#host.effectScope(request),
      kind: "project-session-drain",
      commandId: request.commandId,
      projectSessionId: intent.projectSessionId,
      sessionRevision: integer(session, "revision"),
      sessionGeneration: integer(session, "generation"),
      globalStateRevision: this.#globalRevision(),
      obligations,
    });
    return {
      status: "committed",
      afterState: { lifecycleState: "quiescing", obligationsSettled: true },
      effectRef: receipt,
    };
  }

  observeProjectDrain(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-drain" }> },
  ): OperatorEffectOutcome {
    const session = row(this.#database.prepare(`
      SELECT revision, generation, state FROM project_sessions WHERE project_session_id=?
    `).get(request.intent.projectSessionId), "project session lifecycle");
    if (text(session, "state") !== "quiescing") {
      return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    }
    const obligations = this.#projectObligations(
      request.intent.projectSessionId,
      this.#host.custodyId(this.#host.effectScope(request), request.commandId),
    );
    if (!obligations.settled) return { status: "pending", phase: "observing" };
    const receipt = this.#persistLifecycleReceipt({
      scope: this.#host.effectScope(request),
      kind: "project-session-drain",
      commandId: request.commandId,
      projectSessionId: request.intent.projectSessionId,
      sessionRevision: integer(session, "revision"),
      sessionGeneration: integer(session, "generation"),
      globalStateRevision: this.#globalRevision(),
      obligations,
    });
    return {
      status: "committed",
      afterState: { lifecycleState: "quiescing", obligationsSettled: true },
      effectRef: receipt,
    };
  }

  observeProjectStop(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-stop" }> },
  ): OperatorEffectOutcome {
    const session = row(this.#database.prepare(`
      SELECT state, terminal_path_json FROM project_sessions WHERE project_session_id=?
    `).get(request.intent.projectSessionId), "project session stop observation");
    const expectedTerminal = canonicalJson({
      kind: "cancelled",
      reason: `operator stop ${request.commandId}`,
    });
    if (text(session, "state") === "cancelled" && session.terminal_path_json === expectedTerminal) {
      return { status: "committed", afterState: { lifecycleState: "cancelled" } };
    }
    if (text(session, "state") === "quiescing") {
      return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    }
    const effectCustody = row(
      this.#host.effectCustody(this.#host.effectScope(request), request.commandId),
      "operator effect custody",
    );
    return { status: "ambiguous", effectRef: this.#host.custodyEffectRef(effectCustody) };
  }

  stopProject(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-stop" }> },
  ): OperatorEffectOutcome {
    const intent = request.intent;
    const receipt = this.#lifecycleReceipt(intent.drainReceiptRef, this.#host.effectScope(request));
    if (
      receipt.kind !== "project-session-drain" ||
      receipt.project_session_id !== intent.projectSessionId ||
      receipt.session_revision !== intent.expectedSessionRevision ||
      receipt.session_generation !== intent.expectedSessionGeneration ||
      receipt.global_state_revision !== intent.expectedGlobalStateRevision
    ) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "project stop drain receipt does not bind current state");
    }
    const obligations = this.#projectObligations(
      intent.projectSessionId,
      this.#host.custodyId(this.#host.effectScope(request), request.commandId),
    );
    if (!obligations.settled || this.#globalRevision() !== intent.expectedGlobalStateRevision) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "project stop obligations changed after drain");
    }
    this.#database.transaction(() => {
      const session = row(this.#database.prepare(`
        SELECT revision, generation, state FROM project_sessions WHERE project_session_id=?
      `).get(intent.projectSessionId), "project session stop");
      if (
        integer(session, "revision") !== intent.expectedSessionRevision ||
        integer(session, "generation") !== intent.expectedSessionGeneration ||
        text(session, "state") !== "quiescing"
      ) throw new ProjectFabricCoreError("STALE_GENERATION", "project stop authority changed");
      const superseded = supersedeFinalAcceptanceGates({
        database: this.#database,
        projectSessionId: intent.projectSessionId,
        cause: { kind: "operator-command", ref: request.commandId },
        reason: "project session stopped from quiescing",
        now: this.#clock(),
      });
      const runs = this.#database.prepare(`
        SELECT run_id, revision FROM runs WHERE project_session_id=? AND lifecycle_state='quiescing'
      `).all(intent.projectSessionId) as Row[];
      for (const run of runs) {
        this.#database.prepare(`
          UPDATE runs SET lifecycle_state='cancelled', revision=revision+1 WHERE run_id=? AND revision=?
        `).run(text(run, "run_id"), integer(run, "revision"));
      }
      this.#database.prepare(`
        UPDATE run_chair_leases SET status='revoked', updated_at=?
         WHERE project_session_id=? AND status IN ('active','frozen')
           AND EXISTS (
             SELECT 1 FROM runs run
              WHERE run.project_session_id=run_chair_leases.project_session_id
                AND run.run_id=run_chair_leases.run_id
                AND run.chair_lease_id=run_chair_leases.lease_id
                AND run.chair_generation=run_chair_leases.generation
           )
      `).run(this.#clock(), intent.projectSessionId);
      this.#database.prepare(`
        UPDATE task_owner_leases SET status='revoked', updated_at=?
         WHERE project_session_id=? AND status IN ('active','frozen')
      `).run(this.#clock(), intent.projectSessionId);
      this.#database.prepare(`
        UPDATE capabilities SET revoked_at=?
         WHERE run_id IN (SELECT run_id FROM runs WHERE project_session_id=?) AND revoked_at IS NULL
      `).run(this.#clock(), intent.projectSessionId);
      this.#database.prepare(`
        UPDATE agents SET lifecycle='archived'
         WHERE run_id IN (SELECT run_id FROM runs WHERE project_session_id=?)
      `).run(intent.projectSessionId);
      const stopReason = `operator stop ${request.commandId}`;
      const membership = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='abandoned', abandoned_reason=?, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND state='active' AND (
           (member_kind='coordination-run' AND member_id=coordination_run_id) OR
           (member_kind='lease' AND EXISTS (
             SELECT 1 FROM runs run
              WHERE run.project_session_id=project_session_memberships.project_session_id
                AND run.run_id=project_session_memberships.coordination_run_id
                AND run.chair_lease_id=project_session_memberships.member_id
           ))
         )
      `).run(stopReason, this.#clock(), intent.projectSessionId);
      const terminalPath = canonicalJson({ kind: "cancelled", reason: stopReason });
      const changed = this.#database.prepare(`
        UPDATE project_sessions
           SET state='cancelled', terminal_path_json=?,
               membership_revision=membership_revision+?,
               revision=revision+1, updated_at=?
         WHERE project_session_id=? AND revision=? AND generation=? AND state='quiescing'
      `).run(
        terminalPath,
        membership.changes + superseded.membershipChanges + superseded.gateChanges > 0 ? 1 : 0,
        this.#clock(),
        intent.projectSessionId,
        intent.expectedSessionRevision,
        intent.expectedSessionGeneration,
      );
      if (changed.changes !== 1) throw new ProjectFabricCoreError("STALE_REVISION", "project stop raced another transition");
      retireProjectSessionBridges(this.#database, {
        projectSessionId: intent.projectSessionId,
        sourceKind: "project-session-stop",
        terminalKind: "cancelled",
        terminalRef: terminalPath,
        ownerOperatorId: this.#host.effectScope(request).operatorId,
        ownerRef: request.commandId,
        now: this.#clock(),
      });
      this.#fault("project-stop:after-bridges");
      this.#host.storeCustodyOutcome(this.#host.effectScope(request), request.commandId, {
        status: "committed",
        afterState: { lifecycleState: "cancelled" },
      });
    })();
    try { this.#retireVolatileProjectSession?.(intent.projectSessionId); } catch { /* durable fencing already committed */ }
    return { status: "committed", afterState: { lifecycleState: "cancelled" } };
  }

  drainDaemon(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "daemon-drain" }> },
  ): OperatorEffectOutcome {
    const intent = request.intent;
    const updated = this.#database.prepare(`
      UPDATE daemon_runtime_epochs
         SET state='quiescing', observed_global_revision=?, heartbeat_at=?
       WHERE instance_generation=? AND state='running'
         AND ?=(SELECT revision FROM daemon_global_state WHERE singleton=1)
    `).run(
      intent.expectedGlobalStateRevision,
      this.#clock(),
      intent.expectedDaemonGeneration,
      intent.expectedGlobalStateRevision,
    );
    if (updated.changes !== 1) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "daemon drain epoch or global revision changed");
    }
    return this.#finishDaemonDrain(request.commandId, intent.expectedDaemonGeneration, this.#host.effectScope(request));
  }

  observeDaemonDrain(commandId: string, daemonInstanceGeneration: number, request: OperatorEffectRequest): OperatorEffectOutcome {
    return this.#finishDaemonDrain(commandId, daemonInstanceGeneration, this.#host.effectScope(request));
  }

  #finishDaemonDrain(
    commandId: string,
    daemonInstanceGeneration: number,
    scope: EffectScope,
  ): OperatorEffectOutcome {
    const epoch = row(this.#database.prepare(`
      SELECT state FROM daemon_runtime_epochs WHERE instance_generation=?
    `).get(daemonInstanceGeneration), "daemon runtime epoch");
    if (text(epoch, "state") !== "quiescing") {
      return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    }
    const liveness = this.#liveness({
      now: this.#clock(),
      daemonInstanceGeneration,
      excludeOperatorEffectCustodyId: this.#host.custodyId(scope, commandId),
    });
    if (liveness.failClosed || !liveness.idle || liveness.globalStateRevision === null) {
      return { status: "pending", phase: "observing" };
    }
    this.#database.prepare(`
      UPDATE daemon_runtime_epochs SET observed_global_revision=?, heartbeat_at=?
       WHERE instance_generation=? AND state='quiescing'
    `).run(liveness.globalStateRevision, this.#clock(), daemonInstanceGeneration);
    const receipt = this.#persistLifecycleReceipt({
      scope,
      kind: "daemon-drain",
      commandId,
      daemonInstanceGeneration,
      globalStateRevision: liveness.globalStateRevision,
      obligations: liveness.contributors,
    });
    return {
      status: "committed",
      afterState: { lifecycleState: "quiescing", obligationsSettled: true },
      effectRef: receipt,
    };
  }

  async stopDaemon(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "daemon-stop" }> },
  ): Promise<OperatorEffectOutcome> {
    const intent = request.intent;
    const receipt = this.#lifecycleReceipt(intent.drainReceiptRef, this.#host.effectScope(request));
    if (
      receipt.kind !== "daemon-drain" ||
      receipt.daemon_instance_generation !== intent.expectedDaemonGeneration ||
      receipt.global_state_revision !== intent.expectedGlobalStateRevision ||
      this.#globalRevision() !== intent.expectedGlobalStateRevision
    ) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "daemon stop drain receipt does not bind current state");
    }
    const epoch = row(this.#database.prepare(`
      SELECT state, observed_global_revision FROM daemon_runtime_epochs WHERE instance_generation=?
    `).get(intent.expectedDaemonGeneration), "daemon runtime epoch");
    if (text(epoch, "state") !== "quiescing" || integer(epoch, "observed_global_revision") !== intent.expectedGlobalStateRevision) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "daemon stop epoch changed");
    }
    const scope = this.#host.effectScope(request);
    const liveness = this.#liveness({
      now: this.#clock(),
      daemonInstanceGeneration: intent.expectedDaemonGeneration,
      excludeOperatorEffectCustodyId: this.#host.custodyId(scope, request.commandId),
    });
    if (!liveness.idle || liveness.failClosed || liveness.globalStateRevision !== intent.expectedGlobalStateRevision) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "daemon stop is busy or global state changed");
    }
    const port = this.#daemonStop;
    if (port === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "guarded daemon stop owner is unavailable");
    }
    const custodyId = this.#host.custodyId(scope, request.commandId);
    const stopCustody = row(this.#database.prepare(`
      SELECT result_correlation_digest, state FROM operator_daemon_stop_custody
       WHERE daemon_instance_generation=? AND custody_id=?
         AND operator_id=? AND project_id=? AND project_session_id=?
         AND principal_generation=? AND command_id=? AND operation='daemon-stop'
    `).get(
      intent.expectedDaemonGeneration,
      custodyId,
      scope.operatorId,
      scope.projectId,
      scope.projectSessionId,
      scope.principalGeneration,
      request.commandId,
    ), "daemon stop custody");
    if (text(stopCustody, "state") !== "prepared") {
      const state = text(stopCustody, "state");
      if (state === "scheduled") return { status: "pending", phase: "accepted" };
      if (state === "stopped") return { status: "committed", afterState: { lifecycleState: "stopped" } };
      return { status: "rejected", code: "state-changed", evidenceRefs: [intent.drainReceiptRef] };
    }
    const resultCorrelationDigest = text(stopCustody, "result_correlation_digest");
    let outcome: "stopped" | "scheduled" | "busy";
    try {
      outcome = await port.request({
        custodyId,
        resultCorrelationDigest,
        operatorId: scope.operatorId,
        projectId: scope.projectId,
        projectSessionId: scope.projectSessionId,
        principalGeneration: scope.principalGeneration,
        commandId: request.commandId,
        operation: "daemon-stop",
        token: {
          daemonInstanceGeneration: intent.expectedDaemonGeneration,
          observedGlobalStateRevision: intent.expectedGlobalStateRevision,
        },
      });
    } catch (error: unknown) {
      this.#database.prepare(`
        UPDATE operator_daemon_stop_custody SET state='failed', result_json=?, updated_at=?
         WHERE custody_id=? AND state='prepared'
      `).run(
        canonicalJson({ message: error instanceof Error ? error.message : String(error), resultCorrelationDigest }),
        this.#clock(),
        custodyId,
      );
      throw error;
    }
    if (outcome === "stopped") {
      this.#database.prepare(`
        UPDATE operator_daemon_stop_custody SET state='stopped', result_json=?, updated_at=?
         WHERE custody_id=? AND state='prepared'
      `).run(canonicalJson({ outcome: "stopped", resultCorrelationDigest }), this.#clock(), custodyId);
      return { status: "committed", afterState: { lifecycleState: "stopped" } };
    }
    if (outcome === "scheduled") {
      const scheduled = this.#database.prepare(`
        UPDATE operator_daemon_stop_custody SET state='scheduled', result_json=?, updated_at=?
         WHERE custody_id=? AND state='prepared'
      `).run(canonicalJson({ outcome: "scheduled", resultCorrelationDigest }), this.#clock(), custodyId);
      if (scheduled.changes !== 1) throw new ProjectFabricCoreError("CONFLICT", "daemon stop custody changed while scheduling");
      return { status: "pending", phase: "accepted" };
    }
    this.#database.prepare(`
      UPDATE operator_daemon_stop_custody SET state='rejected', result_json=?, updated_at=?
       WHERE custody_id=? AND state='prepared'
    `).run(canonicalJson({ outcome: "busy", resultCorrelationDigest }), this.#clock(), custodyId);
    return { status: "rejected", code: "state-changed", evidenceRefs: [intent.drainReceiptRef] };
  }

  observeDaemonStop(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "daemon-stop" }> },
  ): OperatorEffectOutcome {
    const scope = this.#host.effectScope(request);
    const custodyId = this.#host.custodyId(scope, request.commandId);
    const stopCustody = row(this.#database.prepare(`
      SELECT state FROM operator_daemon_stop_custody
       WHERE daemon_instance_generation=? AND custody_id=?
         AND operator_id=? AND project_id=? AND project_session_id=?
         AND principal_generation=? AND command_id=? AND operation='daemon-stop'
    `).get(
      request.intent.expectedDaemonGeneration,
      custodyId,
      scope.operatorId,
      scope.projectId,
      scope.projectSessionId,
      scope.principalGeneration,
      request.commandId,
    ), "daemon stop custody");
    const stopState = text(stopCustody, "state");
    const epoch = row(this.#database.prepare(`
      SELECT state, observed_global_revision FROM daemon_runtime_epochs WHERE instance_generation=?
    `).get(request.intent.expectedDaemonGeneration), "daemon runtime epoch");
    if (text(epoch, "state") === "stopped") {
      this.#database.prepare(`
        UPDATE operator_daemon_stop_custody
           SET state='stopped', result_json=?, updated_at=?
         WHERE daemon_instance_generation=? AND custody_id=?
           AND operator_id=? AND project_id=? AND project_session_id=?
           AND principal_generation=? AND command_id=? AND operation='daemon-stop'
           AND state IN ('prepared','scheduled','failed')
      `).run(
        canonicalJson({ stopped: true, observedBy: "daemon-runtime-epoch" }),
        this.#clock(),
        request.intent.expectedDaemonGeneration,
        custodyId,
        scope.operatorId,
        scope.projectId,
        scope.projectSessionId,
        scope.principalGeneration,
        request.commandId,
      );
      return { status: "committed", afterState: { lifecycleState: "stopped" } };
    }
    if (stopState === "rejected") {
      return { status: "rejected", code: "state-changed", evidenceRefs: [request.intent.drainReceiptRef] };
    }
    if (stopState === "failed") {
      const effectCustody = row(this.#host.effectCustody(scope, request.commandId), "operator effect custody");
      return { status: "ambiguous", effectRef: this.#host.custodyEffectRef(effectCustody) };
    }
    if (text(epoch, "state") === "quiescing") return { status: "pending", phase: "observing" };
    return { status: "rejected", code: "state-changed", evidenceRefs: [] };
  }

  #projectObligations(projectSessionId: string, excludeOperatorEffectCustodyId?: string): {
    settled: boolean;
    tasks: number;
    leases: number;
    providerActions: number;
    operatorEffects: number;
    memberships: number;
    requiredDeliveries: number;
    gates: number;
    barriers: number;
    artifacts: number;
  } {
    const count = (sql: string): number => integer(row(this.#database.prepare(sql).get(projectSessionId), "project obligation"), "count");
    const result = {
      tasks: count(`SELECT COUNT(*) AS count FROM tasks task JOIN runs run ON run.run_id=task.run_id
        WHERE run.project_session_id=? AND task.state NOT IN ('complete','cancelled','degraded')`),
      leases: count(`SELECT
        (SELECT COUNT(*) FROM leases lease JOIN runs run ON run.run_id=lease.run_id
          WHERE run.project_session_id=session.project_session_id
            AND lease.status IN ('active','quarantined')) +
        (SELECT COUNT(*) FROM task_owner_leases lease
          WHERE lease.project_session_id=session.project_session_id
            AND lease.status IN ('active','frozen')) +
        (SELECT COUNT(*) FROM run_chair_leases lease JOIN runs run ON run.run_id=lease.run_id
          WHERE lease.project_session_id=session.project_session_id
            AND lease.status IN ('active','frozen')
            AND (lease.lease_id<>run.chair_lease_id OR lease.generation<>run.chair_generation)) AS count
        FROM project_sessions session WHERE session.project_session_id=?`),
      providerActions: count(`SELECT COUNT(*) AS count FROM provider_actions action JOIN runs run ON run.run_id=action.run_id
        WHERE run.project_session_id=? AND action.status IN ('prepared','dispatched','accepted','ambiguous','quarantined')`),
      operatorEffects: integer(row(this.#database.prepare(`
        SELECT COUNT(*) AS count FROM operator_effect_custody
         WHERE project_session_id=?
           AND state IN ('prepared','dispatching','conflict','ambiguous','quarantined','failed')
           AND (? IS NULL OR custody_id<>?)
      `).get(
        projectSessionId,
        excludeOperatorEffectCustodyId ?? null,
        excludeOperatorEffectCustodyId ?? null,
      ), "project operator-effect obligation"), "count"),
      memberships: count(`SELECT COUNT(*) AS count FROM project_session_memberships membership
        WHERE membership.project_session_id=? AND membership.required=1 AND membership.state='active'
          AND NOT (
            (membership.member_kind='coordination-run'
              AND membership.member_id=membership.coordination_run_id) OR
            (membership.member_kind='lease' AND EXISTS (
              SELECT 1 FROM runs run JOIN run_chair_leases lease
                ON lease.project_session_id=run.project_session_id
               AND lease.run_id=run.run_id
               AND lease.lease_id=run.chair_lease_id
               AND lease.generation=run.chair_generation
               AND lease.status IN ('active','frozen')
             WHERE run.project_session_id=membership.project_session_id
               AND run.run_id=membership.coordination_run_id
               AND membership.member_id=run.chair_lease_id
            ))
          )`),
      requiredDeliveries: count(`SELECT COUNT(*) AS count FROM result_deliveries
        WHERE project_session_id=? AND required=1 AND state NOT IN ('consumed','abandoned')`),
      gates: count(`SELECT COUNT(*) AS count FROM scoped_gates
        WHERE project_session_id=? AND status IN ('pending','deferred')`),
      barriers: count(`SELECT COUNT(*) AS count FROM barriers barrier JOIN runs run ON run.run_id=barrier.run_id
        WHERE run.project_session_id=? AND barrier.state<>'closed'`),
      artifacts: count(`SELECT COUNT(*) AS count FROM task_expected_artifacts expected
        JOIN runs run ON run.run_id=expected.run_id
        JOIN tasks task ON task.run_id=expected.run_id AND task.task_id=expected.task_id
        WHERE run.project_session_id=? AND task.state NOT IN ('cancelled','degraded') AND NOT EXISTS (
          SELECT 1 FROM artifacts artifact WHERE artifact.run_id=expected.run_id
            AND artifact.task_id=expected.task_id AND artifact.relative_path=expected.relative_path
        )`),
    };
    return { settled: Object.values(result).every((value) => value === 0), ...result };
  }

  #globalRevision(): number {
    return integer(row(this.#database.prepare(`
      SELECT revision FROM daemon_global_state WHERE singleton=1
    `).get(), "daemon global state"), "revision");
  }

  #persistLifecycleReceipt(input: {
    scope: EffectScope;
    kind: "project-session-drain" | "daemon-drain";
    commandId: string;
    projectSessionId?: string;
    daemonInstanceGeneration?: number;
    sessionRevision?: number;
    sessionGeneration?: number;
    globalStateRevision: number;
    obligations: unknown;
  }): ArtifactRef {
    const receiptValue = {
      schemaVersion: 1,
      kind: input.kind,
      operatorId: input.scope.operatorId,
      projectId: input.scope.projectId,
      authoritySessionId: input.scope.projectSessionId,
      commandId: input.commandId,
      projectSessionId: input.projectSessionId ?? null,
      daemonInstanceGeneration: input.daemonInstanceGeneration ?? null,
      sessionRevision: input.sessionRevision ?? null,
      sessionGeneration: input.sessionGeneration ?? null,
      globalStateRevision: input.globalStateRevision,
      obligations: input.obligations,
    };
    const digest = `sha256:${sha256(canonicalJson(receiptValue))}`;
    const identity = sha256(canonicalJson({
      operatorId: input.scope.operatorId,
      projectId: input.scope.projectId,
      authoritySessionId: input.scope.projectSessionId,
      commandId: input.commandId,
    })).slice(0, 32);
    const path = `.agent-fabric/lifecycle-receipts/${input.kind}-${identity}.json`;
    this.#database.prepare(`
      INSERT INTO operator_lifecycle_receipts(
        relative_path, sha256, kind, operator_id, project_id, authority_session_id,
        project_session_id, daemon_instance_generation,
        session_revision, session_generation, global_state_revision, command_id,
        receipt_json, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      ON CONFLICT(kind, operator_id, project_id, authority_session_id, command_id) DO NOTHING
    `).run(
      path,
      digest,
      input.kind,
      input.scope.operatorId,
      input.scope.projectId,
      input.scope.projectSessionId,
      input.projectSessionId ?? null,
      input.daemonInstanceGeneration ?? null,
      input.sessionRevision ?? null,
      input.sessionGeneration ?? null,
      input.globalStateRevision,
      input.commandId,
      canonicalJson(receiptValue),
      this.#clock(),
    );
    const stored = row(this.#database.prepare(`
      SELECT relative_path, sha256 FROM operator_lifecycle_receipts
       WHERE kind=? AND operator_id=? AND project_id=? AND authority_session_id=? AND command_id=?
    `).get(
      input.kind,
      input.scope.operatorId,
      input.scope.projectId,
      input.scope.projectSessionId,
      input.commandId,
    ), "operator lifecycle receipt");
    if (text(stored, "relative_path") !== path || text(stored, "sha256") !== digest) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "lifecycle receipt command changed");
    }
    return parseArtifactRef({ path, digest }, "operatorLifecycle.receipt");
  }

  #lifecycleReceipt(reference: ArtifactRef, scope: EffectScope): Row {
    return row(this.#database.prepare(`
      SELECT * FROM operator_lifecycle_receipts
       WHERE relative_path=? AND sha256=?
         AND operator_id=? AND project_id=? AND authority_session_id=?
    `).get(
      reference.path,
      reference.digest,
      scope.operatorId,
      scope.projectId,
      scope.projectSessionId,
    ), "operator lifecycle drain receipt");
  }
}
