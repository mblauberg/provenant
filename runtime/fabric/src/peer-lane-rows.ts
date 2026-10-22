import { resolve } from "node:path";
import { runRoot } from "./identity.js";
import { runProcessAlive } from "./run-registry.js";

export function annotatePeerLaneRows(rows: Array<Record<string, any>>, project: string): void {
  for (const row of rows) {
    // Imported lanes carry liveness from their actual owner record.
    if (typeof row.source_project === "string") continue;
    const runDir = typeof row.run_dir === "string" ? row.run_dir
      : typeof row.run_path === "string" && row.run_path !== "" ? resolve(runRoot(project), row.run_path) : undefined;
    if (typeof row.run_dir !== "string" && runDir !== undefined) row.run_dir = runDir;
    if (row.state === "terminal") {
      row.pgid_alive = runDir === undefined || typeof row.task_id !== "string"
        ? true : runProcessAlive(runDir, row.task_id, Number(row.attempt));
    }
  }
}
