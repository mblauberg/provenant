import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, integer, row, text } from "../persistence/row-codec.js";
import type { OperatorEffectOutcome } from "./action-store.js";

export type EffectFreeSessionCancellationInput = Readonly<{
  projectSessionId: string;
  expectedRevision: number;
  expectedGeneration: number;
  reason: string;
  commandId: string;
}>;

export function cancelEffectFreeProjectSession(options: Readonly<{
  database: Database.Database;
  clock: () => number;
  input: EffectFreeSessionCancellationInput;
  storeCustodyOutcome(outcome: OperatorEffectOutcome): void;
  retireVolatileProjectSession?: (projectSessionId: string) => void;
}>): OperatorEffectOutcome {
  const { database, input } = options;
  const outcome: OperatorEffectOutcome = {
    status: "committed",
    afterState: { lifecycleState: "cancelled", cancelledTasks: 0 },
  };
  const terminalPath = canonicalJson({ kind: "cancelled", reason: input.reason });
  database.transaction(() => {
    const session = row(database.prepare(`
      SELECT state, revision, generation FROM project_sessions WHERE project_session_id=?
    `).get(input.projectSessionId), "effect-free project session");
    if (
      !["draft", "awaiting_launch"].includes(text(session, "state")) ||
      integer(session, "revision") !== input.expectedRevision ||
      integer(session, "generation") !== input.expectedGeneration
    ) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "effect-free cancellation requires the exact draft or awaiting-launch session",
      );
    }
    const blockers: ReadonlyArray<readonly [string, string, (readonly unknown[])?]> = [
      ["coordination run", "SELECT 1 FROM runs WHERE project_session_id=? LIMIT 1"],
      ["session membership", "SELECT 1 FROM project_session_memberships WHERE project_session_id=? LIMIT 1"],
      ["launch custody", "SELECT 1 FROM project_session_launch_custody WHERE project_session_id=? LIMIT 1"],
      ["resource reservation", "SELECT 1 FROM resource_reservations WHERE project_session_id=? LIMIT 1"],
      ["gate", "SELECT 1 FROM scoped_gates WHERE project_session_id=? LIMIT 1"],
      [
        "operator effect",
        `SELECT 1 FROM operator_effect_custody
          WHERE project_session_id=? AND command_id<>? LIMIT 1`,
        [input.commandId],
      ],
    ];
    for (const [label, sql, tail = []] of blockers) {
      if (database.prepare(sql).get(input.projectSessionId, ...tail) !== undefined) {
        throw new ProjectFabricCoreError(
          "LIFECYCLE_PRECONDITION_FAILED",
          `effect-free cancellation found an unresolved ${label}`,
        );
      }
    }
    const changed = database.prepare(`
      UPDATE project_sessions
         SET state='cancelled', terminal_path_json=?, revision=revision+1, updated_at=?
       WHERE project_session_id=? AND revision=? AND generation=?
         AND state IN ('draft','awaiting_launch')
    `).run(
      terminalPath,
      options.clock(),
      input.projectSessionId,
      input.expectedRevision,
      input.expectedGeneration,
    );
    if (changed.changes !== 1) {
      throw new ProjectFabricCoreError("STALE_REVISION", "effect-free cancellation raced another transition");
    }
    options.storeCustodyOutcome(outcome);
  })();
  try { options.retireVolatileProjectSession?.(input.projectSessionId); } catch { /* durable cancellation committed */ }
  return outcome;
}
