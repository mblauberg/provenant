import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "./contracts.js";
import { canonicalJson, row, text, type Row } from "../persistence/row-codec.js";

export function supersedeFinalAcceptanceGates(input: Readonly<{
  database: Database.Database;
  projectSessionId: string;
  cause: Readonly<{
    kind: "operator-command" | "chair-bridge-loss" | "system-recovery";
    ref: string;
  }>;
  reason: string;
  now: number;
}>): { gateChanges: number; membershipChanges: number } {
  const systemSupersession = canonicalJson({
    kind: "system-supersession",
    cause: input.cause,
    reason: input.reason,
    decidedAt: new Date(input.now).toISOString(),
  });
  const candidates = input.database.prepare(`
    SELECT gate_id, status FROM scoped_gates
     WHERE project_session_id=? AND status IN ('pending','deferred','approved')
       AND EXISTS (
         SELECT 1 FROM scoped_gate_operations operation
          WHERE operation.gate_id=scoped_gates.gate_id
            AND operation.operation_id='fabric.v1.project-session.close'
       )
     ORDER BY gate_id
  `).all(input.projectSessionId) as Row[];
  const gates = input.database.prepare(`
    UPDATE scoped_gates
       SET status='superseded',
           resolution_json=CASE
             WHEN status IN ('pending','deferred') THEN ?
             ELSE resolution_json
           END,
           resolved_by_operator_id=CASE
             WHEN status IN ('pending','deferred') THEN NULL
             ELSE resolved_by_operator_id
           END,
           revision=revision+1,
           updated_at=?
     WHERE project_session_id=? AND status IN ('pending','deferred','approved')
       AND EXISTS (
         SELECT 1 FROM scoped_gate_operations operation
          WHERE operation.gate_id=scoped_gates.gate_id
            AND operation.operation_id='fabric.v1.project-session.close'
       )
  `).run(systemSupersession, input.now, input.projectSessionId);
  if (gates.changes !== candidates.length) {
    throw new ProjectFabricCoreError("STALE_REVISION", "final-acceptance gate set changed during supersession");
  }
  let membershipChanges = 0;
  for (const candidateValue of candidates) {
    const candidate = row(candidateValue, "final-acceptance supersession candidate");
    const abandoned = text(candidate, "status") !== "approved";
    const membership = input.database.prepare(`
      UPDATE project_session_memberships
         SET state=?, abandoned_reason=?, revision=revision+1, updated_at=?
       WHERE project_session_id=? AND member_kind='gate' AND member_id=?
         AND state IN ('active','reconciled')
         AND EXISTS (
           SELECT 1 FROM scoped_gates gate
            WHERE gate.gate_id=project_session_memberships.member_id
              AND gate.project_session_id=project_session_memberships.project_session_id
              AND gate.coordination_run_id=project_session_memberships.coordination_run_id
         )
    `).run(
      abandoned ? "abandoned" : "reconciled",
      abandoned ? "gate source status superseded by acceptance-cycle exit" : null,
      input.now,
      input.projectSessionId,
      text(candidate, "gate_id"),
    );
    if (membership.changes !== 1) {
      throw new ProjectFabricCoreError(
        "RECOVERY_REQUIRED",
        "final-acceptance gate membership is missing or already terminally inconsistent",
      );
    }
    membershipChanges += 1;
  }
  return { gateChanges: gates.changes, membershipChanges };
}
