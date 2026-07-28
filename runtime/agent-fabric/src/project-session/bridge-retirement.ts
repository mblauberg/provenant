import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "./contracts.js";
import { nullableText, row, text, type Row } from "../persistence/row-codec.js";

export type SessionBridgeRetirement = Readonly<{
  projectSessionId: string;
  sourceKind: "project-session-close" | "project-session-stop" | "chair-recovery-abandon";
  terminalKind: "accepted" | "cancelled" | "failed";
  terminalRef: string;
  ownerOperatorId: string;
  ownerRef: string;
  now: number;
}>;

export function retireProjectSessionBridges(
  database: Database.Database,
  input: SessionBridgeRetirement,
): { chairBridges: number; childBridges: number } {
  const expectedSessionState = input.sourceKind === "project-session-close" ? "closed" : "cancelled";
  const session = row(database.prepare(`
    SELECT state,terminal_path_json FROM project_sessions WHERE project_session_id=?
  `).get(input.projectSessionId), "terminal bridge project session");
  if (text(session, "state") !== expectedSessionState) {
    throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "bridge retirement requires a terminal project session");
  }
  if (nullableText(session, "terminal_path_json") !== input.terminalRef) {
    throw new ProjectFabricCoreError("STALE_REVISION", "bridge retirement terminal reference changed");
  }
  if (database.prepare(`
    SELECT 1 FROM launched_chair_bridge_state
     WHERE project_session_id=? AND state='lost' LIMIT 1
  `).get(input.projectSessionId) !== undefined) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "lost launched-chair bridge requires its recovery owner");
  }
  if (database.prepare(`
    SELECT 1 FROM launched_chair_bridge_retirements retirement
    JOIN runs run ON run.project_session_id=retirement.project_session_id
                 AND run.run_id=retirement.coordination_run_id
     WHERE retirement.project_session_id=?
       AND run.lifecycle_state NOT IN ('closed','cancelled','launch_failed')
     LIMIT 1
  `).get(input.projectSessionId) !== undefined) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "bridge retirement belongs to a nonterminal run");
  }
  if (database.prepare(`
    SELECT 1 FROM agent_bridge_state bridge
    JOIN runs run ON run.run_id=bridge.run_id
     WHERE run.project_session_id=? AND bridge.bridge_state IN ('pending','lost') LIMIT 1
  `).get(input.projectSessionId) !== undefined) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "nonterminal child bridge requires explicit recovery");
  }

  const chairBridges = database.prepare(`
    SELECT bridge.project_session_id,bridge.coordination_run_id,bridge.chair_agent_id,
           bridge.capability_hash,run.chair_agent_id AS current_chair_agent_id,
           run.lifecycle_state,lease.status AS lease_status,
           capability.revoked_at,agent.lifecycle AS agent_lifecycle
      FROM launched_chair_bridge_state bridge
      JOIN runs run ON run.project_session_id=bridge.project_session_id
                   AND run.run_id=bridge.coordination_run_id
      LEFT JOIN run_chair_leases lease
        ON lease.project_session_id=run.project_session_id AND lease.run_id=run.run_id
       AND lease.lease_id=run.chair_lease_id AND lease.generation=run.chair_generation
      LEFT JOIN capabilities capability ON capability.token_hash=bridge.capability_hash
      LEFT JOIN agents agent ON agent.run_id=bridge.coordination_run_id
                            AND agent.agent_id=bridge.chair_agent_id
     WHERE bridge.project_session_id=? AND bridge.state IN ('active','abandoned')
       AND NOT EXISTS (
         SELECT 1 FROM launched_chair_bridge_retirements retirement
          WHERE retirement.project_session_id=bridge.project_session_id
            AND retirement.coordination_run_id=bridge.coordination_run_id
       )
     ORDER BY bridge.coordination_run_id
  `).all(input.projectSessionId) as Row[];
  for (const bridgeValue of chairBridges) {
    const bridge = row(bridgeValue, "terminal launched-chair bridge");
    if (
      !["closed", "cancelled", "launch_failed"].includes(text(bridge, "lifecycle_state")) ||
      text(bridge, "chair_agent_id") !== text(bridge, "current_chair_agent_id") ||
      text(bridge, "lease_status") !== "revoked" ||
      bridge.revoked_at === null || bridge.revoked_at === undefined ||
      text(bridge, "agent_lifecycle") !== "archived"
    ) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "launched-chair bridge lacks clean terminal fencing proof");
    }
    const inserted = database.prepare(`
      INSERT INTO launched_chair_bridge_retirements(
        project_session_id,coordination_run_id,source_kind,terminal_kind,
        terminal_ref,owner_operator_id,owner_ref,created_at
      ) VALUES (?,?,?,?,?,?,?,?)
      ON CONFLICT(project_session_id,coordination_run_id) DO NOTHING
    `).run(
      input.projectSessionId,
      text(bridge, "coordination_run_id"),
      input.sourceKind,
      input.terminalKind,
      input.terminalRef,
      input.ownerOperatorId,
      input.ownerRef,
      input.now,
    );
    if (inserted.changes !== 1) {
      const existing = row(database.prepare(`
        SELECT source_kind,terminal_kind,terminal_ref,owner_operator_id,owner_ref
          FROM launched_chair_bridge_retirements
         WHERE project_session_id=? AND coordination_run_id=?
      `).get(input.projectSessionId, text(bridge, "coordination_run_id")), "launched-chair bridge retirement");
      if (
        text(existing, "source_kind") !== input.sourceKind ||
        text(existing, "terminal_kind") !== input.terminalKind ||
        text(existing, "terminal_ref") !== input.terminalRef ||
        text(existing, "owner_operator_id") !== input.ownerOperatorId ||
        text(existing, "owner_ref") !== input.ownerRef
      ) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launched-chair bridge retirement replay changed");
      }
    }
  }

  const children = database.prepare(`
    UPDATE agent_bridge_state
       SET bridge_state='none',provider_session_ref=NULL,provider_session_generation=NULL,
           capability_hash=NULL,activation_evidence_digest=NULL,
           revision=revision+1,updated_at=?
     WHERE bridge_state='active'
       AND run_id IN (SELECT run_id FROM runs WHERE project_session_id=?)
       AND EXISTS (
         SELECT 1 FROM runs run JOIN agents agent
           ON agent.run_id=run.run_id AND agent.agent_id=agent_bridge_state.agent_id
          JOIN capabilities capability ON capability.token_hash=agent_bridge_state.capability_hash
          WHERE run.run_id=agent_bridge_state.run_id
            AND run.lifecycle_state IN ('closed','cancelled','launch_failed')
            AND agent.lifecycle='archived' AND capability.revoked_at IS NOT NULL
       )
  `).run(input.now, input.projectSessionId);
  const remaining = database.prepare(`
    SELECT 1 FROM agent_bridge_state bridge JOIN runs run ON run.run_id=bridge.run_id
     WHERE run.project_session_id=? AND bridge.bridge_state<>'none' LIMIT 1
  `).get(input.projectSessionId);
  if (remaining !== undefined) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "child bridge lacks clean terminal fencing proof");
  }
  return { chairBridges: chairBridges.length, childBridges: children.changes };
}
