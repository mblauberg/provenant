import {
  parseIdentifier,
  type AgentTopology,
  type AgentTeamTopologyMembership,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { integer, nullableText, row, text, type Row } from "../persistence/row-codec.js";

const MAX_AGENT_TEAM_MEMBERSHIPS = 4;

export type AgentTopologyProjection = "include" | "omit";

export function agentTopologyProjectionField(
  database: Database.Database,
  agent: Row,
  topologyRevision: number,
  projection: AgentTopologyProjection,
): { topology?: AgentTopology } {
  return projection === "include" ? { topology: projectAgentTopology(database, agent, topologyRevision) } : {};
}

/** Project one agent's topology from Fabric-owned rows inside the caller's read transaction. */
export function projectAgentTopology(
  database: Database.Database,
  agent: Row,
  topologyRevision: number,
): AgentTopology {
  const runId = text(agent, "run_id");
  const agentId = text(agent, "agent_id");
  const storedMemberships = database.prepare(`
    SELECT team.team_id, team.generation, team.leader_agent_id
      FROM team_members membership
      JOIN teams team ON team.run_id=membership.run_id AND team.team_id=membership.team_id
     WHERE membership.run_id=? AND membership.agent_id=?
     ORDER BY team.team_id
     LIMIT ${String(MAX_AGENT_TEAM_MEMBERSHIPS + 1)}
  `).all(runId, agentId);
  if (storedMemberships.length > MAX_AGENT_TEAM_MEMBERSHIPS) {
    throw new ProjectFabricCoreError(
      "RESOURCE_EXHAUSTED",
      `agent topology exceeds ${String(MAX_AGENT_TEAM_MEMBERSHIPS)} team memberships`,
    );
  }
  const memberships = storedMemberships.map((value): AgentTeamTopologyMembership => {
    const membership = row(value, "agent team topology membership");
    const leadAgentId = parseIdentifier<"AgentId">(
      text(membership, "leader_agent_id"),
      "agentTopology.leadAgentId",
    );
    return {
      teamId: parseIdentifier<"TeamId">(
        text(membership, "team_id"),
        "agentTopology.teamId",
      ),
      teamGeneration: positive(integer(membership, "generation"), "agentTopology.teamGeneration"),
      relationship: leadAgentId === agentId ? "Lead" : "Member",
      leadAgentId,
    };
  });
  const parentAgentId = nullableText(agent, "parent_agent_id");
  const currentTasks = database.prepare(`
    SELECT task_id, revision, owner_lease_generation
      FROM tasks
     WHERE run_id=? AND owner_agent_id=? AND state='active'
     ORDER BY task_id
     LIMIT 2
  `).all(runId, agentId);
  const currentTask: AgentTopology["currentTask"] = currentTasks.length === 0
    ? { observation: "Unobserved" }
    : currentTasks.length > 1
      ? { observation: "Unknown", reason: "MultipleActiveClaims" }
      : projectCurrentTask(row(currentTasks[0], "agent current task"));
  return {
    topologyRevision: positive(topologyRevision, "agentTopology.topologyRevision"),
    teams: { observation: "Observed", memberships },
    supervisor: parentAgentId === null
      ? { observation: "Unobserved" }
      : {
          observation: "Observed",
          agentId: parseIdentifier<"AgentId">(parentAgentId, "agentTopology.supervisor.agentId"),
        },
    currentTask,
    nativeChildren: { observation: "Unobserved" },
  };
}

function projectCurrentTask(task: Row): Extract<AgentTopology["currentTask"], { observation: "Observed" }> {
  return {
    observation: "Observed",
    taskId: parseIdentifier<"TaskId">(text(task, "task_id"), "agentTopology.currentTask.taskId"),
    taskRevision: positive(integer(task, "revision"), "agentTopology.currentTask.taskRevision"),
    ownerLeaseGeneration: positive(
      integer(task, "owner_lease_generation"),
      "agentTopology.currentTask.ownerLeaseGeneration",
    ),
  };
}

function positive(value: number, path: string): number {
  if (value < 1) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `${path} must be positive`);
  }
  return value;
}
