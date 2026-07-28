import {
  parseArtifactRef,
  parseIdentifier,
  parseTimestamp,
  type RunPlanDeclaration,
  type RunPlanDeclareRequest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedAgentContext, type CoreServiceOptions } from "./contracts.js";
import { integer, isRow, row, text, type Row } from "../persistence/row-codec.js";

export type AcceptedRunScope = Readonly<{
  artifactId: string;
  artifactRef: RunPlanDeclaration["acceptedScopeRef"];
  revision: number;
}>;

export function currentAcceptedRunScope(
  database: Database.Database,
  runId: string,
): AcceptedRunScope | null {
  const value = database.prepare(`
    SELECT intake.revision, artifact.artifact_id, artifact.relative_path, artifact.sha256
      FROM intakes intake
      JOIN artifacts artifact ON artifact.artifact_id=intake.accepted_scope_artifact_id
     WHERE intake.coordination_run_id=? AND intake.state='accepted'
       AND intake.accepted_scope_state='bound' AND artifact.registry_state='active'
     ORDER BY intake.updated_at DESC, intake.intake_id
     LIMIT 1
  `).get(runId);
  if (!isRow(value)) return null;
  return {
    artifactId: text(value, "artifact_id"),
    artifactRef: parseArtifactRef({
      path: text(value, "relative_path"),
      digest: text(value, "sha256"),
    }, "runPlan.acceptedScopeRef"),
    revision: integer(value, "revision"),
  };
}

export function latestRunPlanRow(database: Database.Database, runId: string): Row | null {
  const value = database.prepare(`
    SELECT * FROM run_plan_declarations
     WHERE run_id=? ORDER BY plan_revision DESC LIMIT 1
  `).get(runId);
  return isRow(value) ? value : null;
}

export type CurrentRunPlanBinding = Readonly<{
  acceptedScopeRef: RunPlanDeclaration["acceptedScopeRef"];
  currentPlanRef: RunPlanDeclaration["planArtifactRef"];
  planRevision: number;
  declaredTaskDenominator: number | null;
}>;

export function currentRunPlanBinding(
  database: Database.Database,
  runId: string,
): CurrentRunPlanBinding | null {
  const value = latestRunPlanRow(database, runId);
  if (value === null) return null;
  const declaration = projectDeclaration(value);
  return {
    acceptedScopeRef: declaration.acceptedScopeRef,
    currentPlanRef: declaration.planArtifactRef,
    planRevision: declaration.planRevision,
    declaredTaskDenominator: declaration.declaredTaskDenominator,
  };
}

function projectDeclaration(value: Row): RunPlanDeclaration {
  const denominator = value.declared_task_denominator;
  if (denominator !== null && (typeof denominator !== "number" || !Number.isSafeInteger(denominator))) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "stored run plan denominator is invalid");
  }
  return {
    runId: parseIdentifier<"CoordinationRunId">(text(value, "run_id"), "runPlan.runId"),
    planArtifactRef: parseArtifactRef({
      path: text(value, "plan_path"),
      digest: text(value, "plan_digest"),
    }, "runPlan.planArtifactRef"),
    acceptedScopeRef: parseArtifactRef({
      path: text(value, "accepted_scope_path"),
      digest: text(value, "accepted_scope_digest"),
    }, "runPlan.acceptedScopeRef"),
    acceptedScopeRevision: integer(value, "accepted_scope_revision"),
    planRevision: integer(value, "plan_revision"),
    declaredTaskDenominator: denominator,
    declaredByAgentId: parseIdentifier<"AgentId">(
      text(value, "declared_by_agent_id"),
      "runPlan.declaredByAgentId",
    ),
    declaredAt: parseTimestamp(
      new Date(integer(value, "declared_at")).toISOString(),
      "runPlan.declaredAt",
    ),
  };
}

export class RunPlanStore {
  readonly #database: Database.Database;
  readonly #clock: () => number;

  constructor(options: CoreServiceOptions) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
  }

  declare(context: AuthenticatedAgentContext, request: RunPlanDeclareRequest): RunPlanDeclaration {
    return this.#database.transaction(() => {
      if (request.runId !== context.coordinationRunId) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "run plan is outside the authenticated run");
      }
      this.#assertCurrentChair(context);
      const acceptedScope = currentAcceptedRunScope(this.#database, request.runId);
      if (acceptedScope === null) {
        throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "run has no active accepted scope");
      }
      if (acceptedScope.revision !== request.expectedAcceptedScopeRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "accepted scope revision changed");
      }
      const prior = latestRunPlanRow(this.#database, request.runId);
      const priorRevision = prior === null ? 0 : integer(prior, "plan_revision");
      if (priorRevision >= Number.MAX_SAFE_INTEGER) {
        throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", "run plan revision space is exhausted");
      }
      const planRevision = priorRevision + 1;
      this.#database.prepare(`
        INSERT INTO run_plan_declarations(
          run_id, plan_revision, plan_path, plan_digest,
          accepted_scope_artifact_id, accepted_scope_revision,
          accepted_scope_path, accepted_scope_digest,
          declared_task_denominator, declared_by_agent_id, declared_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        request.runId,
        planRevision,
        request.planArtifactRef.path,
        request.planArtifactRef.digest,
        acceptedScope.artifactId,
        acceptedScope.revision,
        acceptedScope.artifactRef.path,
        acceptedScope.artifactRef.digest,
        request.declaredTaskDenominator ?? null,
        context.agentId,
        this.#clock(),
      );
      const advanced = this.#database.prepare(`
        UPDATE runs SET revision=revision+1 WHERE run_id=?
      `).run(request.runId);
      if (advanced.changes !== 1) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "run plan declaration did not advance the run revision");
      }
      return projectDeclaration(row(this.#database.prepare(`
        SELECT * FROM run_plan_declarations WHERE run_id=? AND plan_revision=?
      `).get(request.runId, planRevision), "declared run plan"));
    })();
  }

  #assertCurrentChair(context: AuthenticatedAgentContext): void {
    const current = this.#database.prepare(`
      SELECT run.chair_agent_id, run.lifecycle_state, run.chair_generation, session.mode,
             lease.holder_agent_id, lease.status AS lease_status
        FROM runs run
        JOIN project_sessions session ON session.project_session_id=run.project_session_id
        JOIN run_chair_leases lease ON lease.project_session_id=run.project_session_id
         AND lease.run_id=run.run_id AND lease.lease_id=run.chair_lease_id
         AND lease.generation=run.chair_generation
       WHERE run.run_id=? AND run.project_session_id=?
         AND session.state IN ('active','visibility_degraded')
    `).get(context.coordinationRunId, context.projectSessionId);
    if (!isRow(current)) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "run cannot accept a plan declaration");
    }
    if (
      !["active", "visibility_degraded"].includes(text(current, "lifecycle_state")) ||
      text(current, "mode") !== "coordinated" ||
      text(current, "chair_agent_id") !== context.agentId ||
      text(current, "holder_agent_id") !== context.agentId ||
      text(current, "lease_status") !== "active"
    ) throw new ProjectFabricCoreError("TASK_NOT_OWNER", "run plan declaration is current-chair only");
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM capabilities WHERE run_id=? AND agent_id=? AND principal_generation=?
       AND revoked_at IS NULL AND expires_at>?
    `).get(context.coordinationRunId, context.agentId, context.principalGeneration, this.#clock()))) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "run plan chair capability is not live");
    }
  }
}
