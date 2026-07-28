import {
  assertChairMutationAuthority,
  parseIdentifier,
  type WorkstreamCreateRequest,
  type WorkstreamProjection,
  type WorkstreamSettleRequest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import type { CommandJournal } from "../application/command-journal.js";
import type { TeamCreateInput, TeamResult } from "../core/contracts.js";
import type { AuthorityInput } from "../domain/types.js";
import type { HierarchicalAdmissionStore } from "../resources/store.js";
import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
  type CoreServiceOptions,
} from "./contracts.js";
import { touchProjectSessionMembershipRevision } from "./membership-store.js";
import { canonicalJson, integer, isRow, row, sha256, text, type Row } from "../persistence/row-codec.js";

type WorkstreamStoreOptions = CoreServiceOptions & {
  commandJournal: CommandJournal;
  resources: HierarchicalAdmissionStore;
  createTeam(runId: string, actorAgentId: string, input: TeamCreateInput): TeamResult;
};

function isProjection(value: unknown): value is WorkstreamProjection {
  return isRow(value) &&
    typeof value.workstreamId === "string" &&
    typeof value.projectSessionId === "string" &&
    typeof value.coordinationRunId === "string" &&
    typeof value.deliveryRunId === "string" &&
    typeof value.teamId === "string" &&
    typeof value.rootTaskId === "string" &&
    typeof value.leadAgentId === "string" &&
    typeof value.authorityId === "string" &&
    typeof value.budgetId === "string" &&
    typeof value.teamScopeId === "string" &&
    ["active", "complete", "cancelled", "degraded"].includes(String(value.state)) &&
    Number.isSafeInteger(value.revision) && Number.isSafeInteger(value.membershipRevision);
}

export class CoordinatedWorkstreamStore {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #commands: CommandJournal;
  readonly #resources: HierarchicalAdmissionStore;
  readonly #createTeam: WorkstreamStoreOptions["createTeam"];

  constructor(options: WorkstreamStoreOptions) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
    this.#commands = options.commandJournal;
    this.#resources = options.resources;
    this.#createTeam = options.createTeam;
  }

  create(context: AuthenticatedAgentContext, request: WorkstreamCreateRequest): WorkstreamProjection {
    return this.#commands.execute(
      context.coordinationRunId,
      context.agentId,
      request.command.commandId,
      request,
      isProjection,
      () => {
        const lifecycle = this.#assertChair(context, request.command, {
          expectedSessionGeneration: request.expectedSessionGeneration,
          expectedMembershipRevision: request.expectedMembershipRevision,
          requireCoordinated: true,
        });
        if (isRow(this.#database.prepare(`
          SELECT 1 FROM workstreams WHERE workstream_id=? OR delivery_run_id=? LIMIT 1
        `).get(request.workstreamId, request.deliveryRunId))) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "workstream or delivery-run identity is already used");
        }
        this.#assertExactAgentScopes(request);
        const launchPacket = this.#database.prepare(`
          SELECT artifact_id FROM artifacts
           WHERE project_session_id=? AND run_id=?
             AND source_kind='project-file' AND evidence_kind='artifact'
             AND relative_path=? AND sha256=? AND registry_state='active'
        `).get(
          context.projectSessionId,
          context.coordinationRunId,
          request.launchPacketRef.path,
          request.launchPacketRef.digest,
        );
        if (!isRow(launchPacket)) {
          const registeredElsewhere = this.#database.prepare(`
            SELECT project_session_id, run_id FROM artifacts
             WHERE relative_path=? AND sha256=? AND registry_state='active'
             LIMIT 1
          `).get(request.launchPacketRef.path, request.launchPacketRef.digest);
          if (
            isRow(registeredElsewhere) &&
            (registeredElsewhere.project_session_id !== context.projectSessionId ||
              registeredElsewhere.run_id !== context.coordinationRunId)
          ) {
            throw new ProjectFabricCoreError("WRONG_PROJECT", "workstream launch packet is registered outside the authenticated run");
          }
          throw new ProjectFabricCoreError(
            "BARRIER_PRECONDITION_FAILED",
            "workstream launch packet is not an active registered project artifact",
          );
        }
        const team = this.#createTeam(context.coordinationRunId, context.agentId, {
          teamId: request.team.teamId,
          leader: {
            agentId: request.team.leader.agentId,
            authority: this.#mutableAuthority(request.team.leader.authority),
          },
          rootTask: { ...request.team.rootTask },
          initialMembers: request.team.initialMembers.map((member) => ({
            agentId: member.agentId,
            authority: this.#mutableAuthority(member.authority),
          })),
          discussionGroups: request.team.discussionGroups.map((group) => ({
            groupId: group.groupId,
            memberAgentIds: [...group.memberAgentIds],
          })),
          reservedBudget: { ...request.team.reservedBudget },
          commandId: `${request.command.commandId}:team`,
        });
        if (team.leader?.authorityId === undefined || team.rootTask === undefined) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "atomic workstream team creation returned an incomplete result");
        }
        const runScope = row(this.#database.prepare(`
          SELECT scope_id, scope_kind, owner_ref FROM resource_scopes
           WHERE scope_id=? AND project_session_id=? AND coordination_run_id=? AND state='active'
        `).get(
          request.resources.runScopeId,
          context.projectSessionId,
          context.coordinationRunId,
        ), "workstream run resource scope");
        if (text(runScope, "scope_kind") !== "coordination-run" || text(runScope, "owner_ref") !== context.coordinationRunId) {
          throw new ProjectFabricCoreError("AUTHORITY_WIDENING", "workstream run scope is not the authenticated coordination run");
        }
        this.#resources.defineChildScope(context, {
          scopeId: request.resources.teamScopeId,
          parentScopeId: request.resources.runScopeId,
          kind: "team",
          ownerRef: request.team.teamId,
          limits: request.resources.teamLimits,
        });
        for (const scope of request.resources.agentScopes) {
          this.#resources.defineChildScope(context, {
            scopeId: scope.scopeId,
            parentScopeId: request.resources.teamScopeId,
            kind: "agent",
            ownerRef: scope.agentId,
            limits: scope.limits,
          });
        }
        this.#fault("workstream:create:after-team-and-resources");
        const now = this.#clock();
        this.#database.prepare(`
          INSERT INTO workstreams(
            workstream_id, project_session_id, coordination_run_id, fabric_task_id,
            lead_agent_id, delivery_run_id, revision, state, created_at, updated_at
          ) VALUES (?, ?, ?, ?, ?, ?, 1, 'active', ?, ?)
        `).run(
          request.workstreamId,
          context.projectSessionId,
          context.coordinationRunId,
          request.team.rootTask.taskId,
          request.team.leader.agentId,
          request.deliveryRunId,
          now,
          now,
        );
        this.#database.prepare(`
          INSERT INTO workstream_custody(
            workstream_id, input_digest, launch_packet_artifact_id,
            launch_packet_path, launch_packet_digest,
            team_id, root_task_id, authority_id, budget_id, run_scope_id, team_scope_id, created_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        `).run(
          request.workstreamId,
          `sha256:${sha256(canonicalJson(request))}`,
          text(launchPacket, "artifact_id"),
          request.launchPacketRef.path,
          request.launchPacketRef.digest,
          request.team.teamId,
          request.team.rootTask.taskId,
          team.leader.authorityId,
          team.budgetId,
          request.resources.runScopeId,
          request.resources.teamScopeId,
          now,
        );
        this.#database.prepare(`
          INSERT INTO project_session_memberships(
            project_session_id, coordination_run_id, member_kind, member_id,
            required, state, revision, abandoned_reason, created_at, updated_at
          ) VALUES (?, ?, 'workstream', ?, 1, 'active', 1, NULL, ?, ?)
        `).run(context.projectSessionId, context.coordinationRunId, request.workstreamId, now, now);
        this.#fault("workstream:create:after-membership");
        const projected = this.project(context.projectSessionId, request.workstreamId);
        if (projected.membershipRevision !== lifecycle.membershipRevision + 1) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "workstream creation did not advance membership exactly once");
        }
        return projected;
      },
    );
  }

  settle(context: AuthenticatedAgentContext, request: WorkstreamSettleRequest): WorkstreamProjection {
    return this.#commands.execute(
      context.coordinationRunId,
      context.agentId,
      request.command.commandId,
      request,
      isProjection,
      () => {
        this.#assertChair(context, request.command, {
          expectedSessionGeneration: request.expectedSessionGeneration,
          expectedMembershipRevision: request.expectedMembershipRevision,
          requireCoordinated: true,
        });
        const workstream = this.#workstream(request.workstreamId);
        if (
          text(workstream, "project_session_id") !== context.projectSessionId ||
          text(workstream, "coordination_run_id") !== context.coordinationRunId
        ) throw new ProjectFabricCoreError("WRONG_PROJECT", "workstream is outside the authenticated run");
        if (integer(workstream, "revision") !== request.expectedWorkstreamRevision) {
          throw new ProjectFabricCoreError("STALE_REVISION", "workstream revision changed");
        }
        if (text(workstream, "state") !== "active") {
          throw new ProjectFabricCoreError("CONFLICT", "only an active workstream may settle");
        }
        const rootTaskId = text(workstream, "root_task_id");
        const task = row(this.#database.prepare(`
          SELECT state, revision FROM tasks WHERE run_id=? AND task_id=?
        `).get(context.coordinationRunId, rootTaskId), "workstream root task");
        if (integer(task, "revision") !== request.expectedRootTaskRevision) {
          throw new ProjectFabricCoreError("STALE_REVISION", "workstream root task revision changed");
        }
        const taskState = text(task, "state");
        if (!["complete", "cancelled", "degraded"].includes(taskState)) {
          throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "workstream root task is not terminal");
        }
        const teamId = text(workstream, "team_id");
        const team = row(this.#database.prepare(`
          SELECT state, generation FROM teams WHERE run_id=? AND team_id=?
        `).get(context.coordinationRunId, teamId), "workstream team");
        if (integer(team, "generation") !== request.expectedTeamGeneration) {
          throw new ProjectFabricCoreError("STALE_TEAM_GENERATION", "workstream team generation changed");
        }
        if (text(team, "state") !== "barrier-closed" || !isRow(this.#database.prepare(`
          SELECT 1 FROM subtree_barriers WHERE run_id=? AND team_id=? AND generation=?
        `).get(context.coordinationRunId, teamId, request.expectedTeamGeneration))) {
          throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "workstream team barrier is not closed");
        }
        this.#assertNoLiveObligations(context.coordinationRunId, teamId, text(workstream, "team_scope_id"));
        const target = taskState as WorkstreamProjection["state"];
        const now = this.#clock();
        const updated = this.#database.prepare(`
          UPDATE workstreams SET state=?, revision=revision+1, updated_at=?
           WHERE workstream_id=? AND state='active' AND revision=?
        `).run(target, now, request.workstreamId, request.expectedWorkstreamRevision);
        if (updated.changes !== 1) throw new ProjectFabricCoreError("STALE_REVISION", "workstream changed during settlement");
        const terminalMembership = target === "complete" ? "reconciled" : "abandoned";
        const reason = target === "complete" ? null : `workstream source state ${target}`;
        const taskMemberships = this.#database.prepare(`
          UPDATE project_session_memberships
             SET state=CASE (
                   SELECT task.state FROM tasks task
                    WHERE task.run_id=project_session_memberships.coordination_run_id
                      AND task.task_id=project_session_memberships.member_id
                 ) WHEN 'complete' THEN 'reconciled' ELSE 'abandoned' END,
                 abandoned_reason=CASE (
                   SELECT task.state FROM tasks task
                    WHERE task.run_id=project_session_memberships.coordination_run_id
                      AND task.task_id=project_session_memberships.member_id
                 ) WHEN 'complete' THEN NULL ELSE 'task source state '||(
                   SELECT task.state FROM tasks task
                    WHERE task.run_id=project_session_memberships.coordination_run_id
                      AND task.task_id=project_session_memberships.member_id
                 ) END,
                 revision=revision+1, updated_at=?
           WHERE project_session_id=? AND coordination_run_id=? AND required=1 AND state='active'
             AND member_kind='task' AND member_id IN (
               WITH RECURSIVE team_subtree(team_id) AS (
                 SELECT team_id FROM teams WHERE run_id=? AND team_id=?
                 UNION ALL
                 SELECT child.team_id FROM teams child JOIN team_subtree parent
                   ON child.parent_team_id=parent.team_id WHERE child.run_id=?
               )
               SELECT owned.task_id FROM team_owned_tasks owned
                WHERE owned.run_id=? AND owned.team_id IN (SELECT team_id FROM team_subtree)
             )
        `).run(
          now,
          context.projectSessionId,
          context.coordinationRunId,
          context.coordinationRunId,
          teamId,
          context.coordinationRunId,
          context.coordinationRunId,
        );
        const workstreamMembership = this.#database.prepare(`
          UPDATE project_session_memberships
             SET state=?, abandoned_reason=?, revision=revision+1, updated_at=?
           WHERE project_session_id=? AND coordination_run_id=? AND required=1 AND state='active'
             AND member_kind='workstream' AND member_id=?
        `).run(
          terminalMembership,
          reason,
          now,
          context.projectSessionId,
          context.coordinationRunId,
          request.workstreamId,
        );
        if (workstreamMembership.changes !== 1) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "workstream terminal membership changed unexpectedly");
        }
        this.#database.prepare(`
          WITH RECURSIVE team_subtree(team_id) AS (
            SELECT team_id FROM teams WHERE run_id=? AND team_id=?
            UNION ALL
            SELECT child.team_id FROM teams child JOIN team_subtree parent
              ON child.parent_team_id=parent.team_id WHERE child.run_id=?
          )
          UPDATE budgets SET state='released'
           WHERE run_id=? AND state='active'
             AND team_id IN (SELECT team_id FROM team_subtree)
        `).run(
          context.coordinationRunId,
          teamId,
          context.coordinationRunId,
          context.coordinationRunId,
        );
        this.#database.prepare(`
          WITH RECURSIVE scope_subtree(scope_id) AS (
            SELECT scope_id FROM resource_scopes WHERE coordination_run_id=? AND scope_id=?
            UNION ALL
            SELECT child.scope_id FROM resource_scopes child JOIN scope_subtree parent
              ON child.parent_scope_id=parent.scope_id
             WHERE child.coordination_run_id=?
          )
          UPDATE resource_scopes SET state='released', revision=revision+1
           WHERE state='active' AND scope_id IN (SELECT scope_id FROM scope_subtree)
        `).run(context.coordinationRunId, text(workstream, "team_scope_id"), context.coordinationRunId);
        touchProjectSessionMembershipRevision(
          this.#database,
          context.projectSessionId,
          now,
          taskMemberships.changes + workstreamMembership.changes,
        );
        this.#fault("workstream:settle:after-sources");
        const projected = this.project(context.projectSessionId, request.workstreamId);
        if (projected.membershipRevision !== request.expectedMembershipRevision + 1) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "workstream settlement did not advance membership exactly once");
        }
        return projected;
      },
    );
  }

  project(projectSessionId: string, workstreamId: string): WorkstreamProjection {
    const value = this.#workstream(workstreamId);
    if (text(value, "project_session_id") !== projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "workstream belongs to another project session");
    }
    const state = text(value, "state");
    if (!["active", "complete", "cancelled", "degraded"].includes(state)) {
      throw new Error("stored workstream cannot be publicly projected");
    }
    return {
      workstreamId,
      projectSessionId: parseIdentifier<"ProjectSessionId">(projectSessionId, "workstream.projectSessionId"),
      coordinationRunId: parseIdentifier<"CoordinationRunId">(
        text(value, "coordination_run_id"),
        "workstream.coordinationRunId",
      ),
      deliveryRunId: text(value, "delivery_run_id"),
      teamId: text(value, "team_id"),
      rootTaskId: parseIdentifier<"TaskId">(text(value, "root_task_id"), "workstream.rootTaskId"),
      leadAgentId: parseIdentifier<"AgentId">(text(value, "lead_agent_id"), "workstream.leadAgentId"),
      authorityId: text(value, "authority_id"),
      budgetId: text(value, "budget_id"),
      teamScopeId: text(value, "team_scope_id"),
      state: state as WorkstreamProjection["state"],
      revision: integer(value, "revision"),
      membershipRevision: integer(value, "membership_revision"),
    };
  }

  #workstream(workstreamId: string): Row {
    return row(this.#database.prepare(`
      SELECT stream.*, custody.team_id, custody.root_task_id, custody.authority_id,
             custody.budget_id, custody.team_scope_id, session.membership_revision
        FROM workstreams stream
        JOIN workstream_custody custody USING(workstream_id)
        JOIN project_sessions session USING(project_session_id)
       WHERE stream.workstream_id=?
    `).get(workstreamId), "coordinated workstream");
  }

  #assertChair(
    context: AuthenticatedAgentContext,
    command: WorkstreamCreateRequest["command"],
    expected: Readonly<{
      expectedSessionGeneration: number;
      expectedMembershipRevision: number;
      requireCoordinated: boolean;
    }>,
  ): { membershipRevision: number } {
    if (
      context.projectSessionId !== command.projectSessionId ||
      context.coordinationRunId !== command.coordinationRunId ||
      context.agentId !== command.agentId
    ) throw new ProjectFabricCoreError("WRONG_PROJECT", "workstream command is outside the authenticated principal");
    const current = row(this.#database.prepare(`
      SELECT run.chair_agent_id, run.chair_generation, run.chair_lease_id, run.revision AS run_revision,
             run.lifecycle_state, session.mode, session.state, session.revision AS session_revision,
             session.generation AS session_generation, session.membership_revision
        FROM runs run JOIN project_sessions session USING(project_session_id)
       WHERE run.project_session_id=? AND run.run_id=?
    `).get(context.projectSessionId, context.coordinationRunId), "workstream chair binding");
    if (
      (expected.requireCoordinated && text(current, "mode") !== "coordinated") ||
      !["active", "visibility_degraded"].includes(text(current, "state")) ||
      !["active", "visibility_degraded"].includes(text(current, "lifecycle_state"))
    ) throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "project session cannot admit a coordinated workstream");
    if (
      integer(current, "session_revision") !== command.expectedRevision ||
      integer(current, "session_generation") !== expected.expectedSessionGeneration ||
      integer(current, "membership_revision") !== expected.expectedMembershipRevision
    ) throw new ProjectFabricCoreError("STALE_REVISION", "workstream session binding changed");
    const lease = row(this.#database.prepare(`
      SELECT lease_id, holder_agent_id, generation, status FROM run_chair_leases
       WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
    `).get(
      context.projectSessionId,
      context.coordinationRunId,
      text(current, "chair_lease_id"),
      integer(current, "chair_generation"),
    ), "workstream chair lease");
    if (
      text(current, "chair_agent_id") !== context.agentId ||
      text(lease, "holder_agent_id") !== context.agentId ||
      text(lease, "status") !== "active"
    ) throw new ProjectFabricCoreError("TASK_NOT_OWNER", "workstream mutation is current-chair only");
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM capabilities WHERE run_id=? AND agent_id=? AND principal_generation=?
       AND revoked_at IS NULL AND expires_at>?
    `).get(context.coordinationRunId, context.agentId, context.principalGeneration, this.#clock()))) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "workstream chair capability is not live");
    }
    try {
      assertChairMutationAuthority(command, {
        agentId: context.agentId,
        projectSessionId: context.projectSessionId,
        coordinationRunId: context.coordinationRunId,
        principalGeneration: context.principalGeneration,
        chairLeaseId: parseIdentifier<"LeaseId">(text(lease, "lease_id"), "workstream.chairLeaseId"),
        chairLeaseGeneration: integer(lease, "generation"),
        runRevision: integer(current, "run_revision"),
      });
    } catch (error: unknown) {
      throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", error instanceof Error ? error.message : String(error));
    }
    return { membershipRevision: integer(current, "membership_revision") };
  }

  #assertNoLiveObligations(runId: string, teamId: string, teamScopeId: string): void {
    const blocker = this.#database.prepare(`
      WITH RECURSIVE
      team_subtree(team_id) AS (
        SELECT team_id FROM teams WHERE run_id=? AND team_id=?
        UNION ALL
        SELECT child.team_id FROM teams child JOIN team_subtree parent
          ON child.parent_team_id=parent.team_id WHERE child.run_id=?
      ),
      subtree_tasks(task_id) AS (
        SELECT owned.task_id FROM team_owned_tasks owned
         WHERE owned.run_id=? AND owned.team_id IN (SELECT team_id FROM team_subtree)
      ),
      subtree_members(agent_id) AS (
        SELECT DISTINCT member.agent_id FROM team_members member
         WHERE member.run_id=? AND member.team_id IN (SELECT team_id FROM team_subtree)
      ),
      scope_subtree(scope_id) AS (
        SELECT scope_id FROM resource_scopes WHERE coordination_run_id=? AND scope_id=?
        UNION ALL
        SELECT child.scope_id FROM resource_scopes child JOIN scope_subtree parent
          ON child.parent_scope_id=parent.scope_id WHERE child.coordination_run_id=?
      ),
      affected_gates(gate_id) AS (
        SELECT DISTINCT gate.gate_id FROM scoped_gates gate
        LEFT JOIN scoped_gate_tasks binding ON binding.gate_id=gate.gate_id
         WHERE gate.coordination_run_id=? AND gate.status IN ('pending','deferred')
           AND (gate.scope_task_id IN (SELECT task_id FROM subtree_tasks)
             OR binding.task_id IN (SELECT task_id FROM subtree_tasks))
      )
      SELECT blocker FROM (
        SELECT 'team-barrier' AS blocker FROM teams team
         WHERE team.run_id=? AND team.team_id IN (SELECT team_id FROM team_subtree)
           AND (team.state<>'barrier-closed' OR NOT EXISTS (
             SELECT 1 FROM subtree_barriers barrier WHERE barrier.run_id=team.run_id
               AND barrier.team_id=team.team_id AND barrier.generation=team.generation
           ))
        UNION ALL
        SELECT 'write-lease' FROM leases lease WHERE lease.run_id=? AND lease.kind='write'
          AND lease.holder_agent_id IN (SELECT agent_id FROM subtree_members)
          AND lease.status IN ('active','quarantined')
        UNION ALL
        SELECT 'task-owner-lease' FROM task_owner_leases owner WHERE owner.run_id=?
          AND owner.task_id IN (SELECT task_id FROM subtree_tasks) AND owner.status IN ('active','frozen')
        UNION ALL
        SELECT 'provider-action' FROM provider_actions action WHERE action.run_id=?
          AND action.target_agent_id IN (SELECT agent_id FROM subtree_members)
          AND action.status IN ('prepared','dispatched','accepted','ambiguous','quarantined')
        UNION ALL
        SELECT 'result-delivery' FROM result_deliveries delivery WHERE delivery.run_id=? AND delivery.required=1
          AND delivery.task_id IN (SELECT task_id FROM subtree_tasks)
          AND delivery.state NOT IN ('consumed','abandoned')
        UNION ALL
        SELECT 'task-request' FROM task_requests request WHERE request.run_id=?
          AND request.task_id IN (SELECT task_id FROM subtree_tasks)
          AND request.state NOT IN ('answered','abandoned')
        UNION ALL
        SELECT 'required-message-delivery' FROM deliveries delivery
          JOIN messages message ON message.message_id=delivery.message_id
         WHERE delivery.run_id=? AND (
             delivery.recipient_id IN (SELECT agent_id FROM subtree_members)
             OR message.sender_id IN (SELECT agent_id FROM subtree_members)
           )
           AND message.requires_ack=1
           AND delivery.state NOT IN ('acknowledged','abandoned','expired')
        UNION ALL
        SELECT 'task-obligation-binding' FROM task_obligation_bindings binding
         WHERE binding.coordination_run_id=? AND binding.task_id IN (SELECT task_id FROM subtree_tasks)
           AND binding.state='active'
        UNION ALL
        SELECT 'expected-artifact' FROM task_expected_artifacts expected
         WHERE expected.run_id=? AND expected.task_id IN (SELECT task_id FROM subtree_tasks)
           AND NOT EXISTS (
             SELECT 1 FROM artifacts artifact WHERE artifact.run_id=expected.run_id
               AND artifact.task_id=expected.task_id AND artifact.relative_path=expected.relative_path
               AND artifact.registry_state='active'
           )
        UNION ALL
        SELECT 'artifact-obligation' FROM project_session_memberships membership
          JOIN artifacts artifact ON artifact.artifact_id=membership.member_id
           AND artifact.run_id=membership.coordination_run_id
         WHERE membership.coordination_run_id=? AND membership.member_kind='artifact-obligation'
           AND membership.required=1 AND membership.state='active'
           AND artifact.task_id IN (SELECT task_id FROM subtree_tasks)
        UNION ALL
        SELECT 'resource-reservation' FROM resource_reservations reservation
         WHERE reservation.coordination_run_id=?
           AND reservation.state IN ('reserved','partially-consumed','ambiguous')
           AND (reservation.leaf_scope_id IN (SELECT scope_id FROM scope_subtree)
             OR reservation.operation_id IN (SELECT task_id FROM subtree_tasks))
        UNION ALL
        SELECT 'scoped-gate' FROM affected_gates
        UNION ALL
        SELECT 'scoped-barrier' FROM scoped_gate_barriers binding
          JOIN barriers barrier
            ON binding.barrier_id=barrier.run_id||':'||barrier.scope||':'||barrier.stage_id
         WHERE binding.gate_id IN (SELECT gate_id FROM affected_gates) AND barrier.state<>'closed'
        UNION ALL
        SELECT 'request-barrier' FROM task_request_barriers barrier
          JOIN task_requests request ON request.request_id=barrier.request_id
         WHERE request.run_id=? AND request.task_id IN (SELECT task_id FROM subtree_tasks)
           AND barrier.state='blocked'
      ) LIMIT 1
    `).get(
      runId, teamId, runId,
      runId,
      runId,
      runId, teamScopeId, runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
      runId,
    );
    if (isRow(blocker)) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "workstream retains a live team obligation");
    }
  }

  #assertExactAgentScopes(request: WorkstreamCreateRequest): void {
    const teamAgentIds = [request.team.leader.agentId, ...request.team.initialMembers.map((member) => member.agentId)];
    const scopedAgentIds = request.resources.agentScopes.map((scope) => scope.agentId);
    const scopeIds = request.resources.agentScopes.map((scope) => scope.scopeId);
    if (
      new Set(teamAgentIds).size !== teamAgentIds.length ||
      scopedAgentIds.length !== teamAgentIds.length ||
      new Set(scopedAgentIds).size !== scopedAgentIds.length ||
      new Set(scopeIds).size !== scopeIds.length ||
      [...scopedAgentIds].sort().join("\0") !== [...teamAgentIds].sort().join("\0")
    ) {
      throw new ProjectFabricCoreError(
        "PROTOCOL_INVALID",
        "workstream resource agent scopes must map one-to-one to exactly the team agents",
      );
    }
  }

  #mutableAuthority(authority: WorkstreamCreateRequest["team"]["leader"]["authority"]): AuthorityInput {
    if (!("level" in authority.disclosure)) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "workstream authority disclosure must use the current policy object");
    }
    const disclosure: AuthorityInput["disclosure"] = authority.disclosure.level === "scoped"
      ? { level: "scoped", scopes: [...authority.disclosure.scopes] }
      : { ...authority.disclosure };
    return {
      schemaVersion: 2,
      approval: { ...authority.approval },
      workspaceRoots: [...authority.workspaceRoots],
      sourcePaths: [...authority.sourcePaths],
      artifactPaths: [...authority.artifactPaths],
      actions: [...authority.actions],
      deniedPaths: [...authority.deniedPaths],
      deniedActions: [...authority.deniedActions],
      prohibitedActions: [...authority.prohibitedActions],
      disclosure,
      secrets: authority.secrets.access === "none"
        ? { access: "none" }
        : { access: "use-without-disclosure", references: [...authority.secrets.references] },
      deployment: authority.deployment.allowed
        ? { allowed: true, targets: [...authority.deployment.targets] }
        : { allowed: false },
      irreversibleActions: authority.irreversibleActions.allowed
        ? { allowed: true, actionIds: [...authority.irreversibleActions.actionIds] }
        : { allowed: false },
      network: authority.network.toolEgress === "none"
        ? { toolEgress: "none" }
        : { toolEgress: "allowlist", allowedHosts: [...authority.network.allowedHosts] },
      expiresAt: authority.expiresAt,
      budget: { ...authority.budget },
    };
  }
}
