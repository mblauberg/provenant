/** Fixed peer boundary. Provider mechanics remain with the local owners. */
import { closeSync, readFileSync, writeFileSync } from "node:fs";
import { identify, databasePath } from "./identity.js";
import { dispatchConfiguredProvider, hostOwnersInThemselves, releaseActiveExecutions } from "./execution.js";
import { dispatchConfiguredBatch, cancelConfiguredRun } from "./batch-execution.js";
import { statusRows, fabricOutput } from "./run-status.js";
import { readRuns } from "./run-reader.js";
import { resumeConfiguredProvider, handoffDispatch } from "./resume.js";
import { sessionDispatch } from "./sessions.js";
import { Store } from "./store.js";
import { annotatePeerLaneRows } from "./peer-lane-rows.js";

const request = JSON.parse(readFileSync(process.argv[2]!, "utf8"));
const identity = identify();
const signal = new AbortController().signal;
hostOwnersInThemselves();
// The caller's operation lock, shared until the launch is recorded. Releasing
// it before any owner starts keeps detached owners from inheriting it.
let lockFd: number | null = Number(process.env.PROVENANT_OPERATION_LOCK_FD ?? "");
delete process.env.PROVENANT_OPERATION_LOCK_FD;
const recorded = () => {
  if (lockFd !== null && Number.isInteger(lockFd) && lockFd > 2) try { closeSync(lockFd); } catch { /* already closed */ }
  lockFd = null;
};
const launch = { ...(typeof request.reader_cwd === "string" ? { readOnlyCwd: request.reader_cwd } : {}), onLaunch: (value: unknown) => {
  if (request.launch_path) writeFileSync(request.launch_path, JSON.stringify(value), { mode: 0o600 });
  recorded();
} };
let result: unknown;
try {
  const input = request.input;
  switch (request.verb) {
    case "dispatch": {
      if (input.session) {
        const store = new Store(databasePath());
        try { store.announce(identity); result = await sessionDispatch(input, identity, store, signal, launch); }
        finally { store.close(); }
      } else result = input.tasks ? await dispatchConfiguredBatch(input, identity, signal, process.env, launch)
        : await dispatchConfiguredProvider(input, identity, signal, process.env, launch);
      break;
    }
    case "resume": {
      const { resume_attempt, require_session, ...rest } = input;
      result = await resumeConfiguredProvider(rest, identity, signal, process.env,
        { ...launch, ...(resume_attempt === undefined ? {} : { attempt: resume_attempt }),
          ...(require_session === undefined ? {} : { session: require_session }) });
      break;
    }
    case "handoff": result = await handoffDispatch(input, identity, signal, process.env, launch); break;
    case "cancel": {
      const current = await statusRows(identity.project, [input.id], 0, "all", signal, "full", false, null);
      if (request.launch_path && current.runs?.length) writeFileSync(request.launch_path, JSON.stringify({
        targets: current.runs.map((row) => ({ runId: row.run_id, taskId: row.task_id, attempt: row.attempt })),
      }), { mode: 0o600 });
      recorded();
      result = await cancelConfiguredRun(input.id, identity, input.reason);
      break;
    }
    case "output": result = await fabricOutput(identity.project, { ...input, max_bytes: Math.min(input.max_bytes ?? 20000, 20000) }); break;
    case "lanes": result = await readRuns(identity.project, input.ids ?? undefined, 0, signal, input.limit ?? null, input.state, input.include_history === true); break;
    case "status": {
      const value = await statusRows(identity.project, input.ids ?? undefined, 0, "all", signal, input.detail, false, input.limit ?? null, input.include_history === true, input.include_pointers === true);
      result = value.runs ? { ...value, status: "ok" } : value;
      break;
    }
    default: throw new Error("Unsupported peer lane verb");
  }
  const rows = (result as { runs?: Array<Record<string, any>> } | undefined)?.runs;
  // A terminal attempt can still have a live sequence owner between fallbacks.
  // Expose its liveness before the Python host considers cleanup.
  annotatePeerLaneRows(rows ?? [], identity.project);
  console.log(JSON.stringify(result));
} finally {
  releaseActiveExecutions();
}
// Detached local owners have durable records and must survive this connection.
process.exit(0);
