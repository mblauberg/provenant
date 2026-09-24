import { existsSync, lstatSync, realpathSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";

import { runRoot } from "./identity.js";
import { processMatches, processStartedAt, readOwnerRecord, readProviderRecord, statusRows } from "./run-registry.js";
import type { Message } from "./store.js";

export interface RunRead {
  id: string;
  run_id: string;
  task_id: string | null;
  run_path: string;
  state: string;
  status: string | null;
  route: string | null;
  model: string | null;
  started_at: string | null;
  last_progress_at: string | null;
  pgid: number | null;
  pgid_alive: boolean | null;
  result_path: string | null;
  receipt_path: string | null;
  writer: boolean;
  worktree: string | null;
  attempt: number;
}

function rootPath(root: string, runDir: string, value: unknown): string | null {
  if (typeof value !== "string" || !value) return null;
  const path = resolve(runDir, value);
  try { if (lstatSync(path).isSymbolicLink()) return null; } catch { /* A result may not exist yet. */ }
  const candidate = existsSync(path) ? realpathSync(path) : path;
  const local = relative(runDir, candidate);
  const rel = relative(root, candidate);
  return rel && !isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`) &&
    !isAbsolute(local) && local !== ".." && !local.startsWith(`..${sep}`) ? rel : null;
}

/** The public read contract. Receipt layout and absolute paths end here. */
export async function readRuns(workspace: string, ids?: string[], waitSeconds = 0, signal?: AbortSignal) {
  try {
    const source = await statusRows(workspace, ids, waitSeconds, "all", signal, "brief", false);
    if (!source.runs) return { schema: "fabric.runs.v1", status: "unknown" as const,
      error: source.error ?? "run_read_failed", runs: [] as RunRead[] };
    const root = existsSync(runRoot(workspace)) ? realpathSync(runRoot(workspace)) : resolve(runRoot(workspace));
    const runs: RunRead[] = source.runs.map((row) => {
      const runDir = String(row.run_dir);
      const latest = Array.isArray(row.attempts) ? row.attempts.at(-1) : undefined;
      const receipt = row.paths?.receipt ?? latest?.paths?.receipt ??
        (typeof row.paths?.result === "string" ? `${dirname(row.paths.result)}/attempt.json` : undefined);
      const owner = readOwnerRecord(runDir);
      const provider = owner && readProviderRecord(runDir, owner.run_token);
      const liveRecord = provider?.provider_started_at ? provider : owner;
      const pid = liveRecord && ("provider_pid" in liveRecord ? liveRecord.provider_pid : liveRecord.owner_pid);
      const started = liveRecord && ("provider_started_at" in liveRecord ? liveRecord.provider_started_at : liveRecord.owner_started_at);
      const requested = row.provenance?.requested;
      const adapter = requested?.adapter ?? row.route?.adapter ?? row.adapter;
      const model = row.provenance?.resolved_model ?? row.route?.resolved_model ?? row.model;
      const effort = row.provenance?.effort_applied ?? requested?.effort;
      return {
        id: String(row.task_id ?? row.id ?? row.run_id),
        run_id: String(row.run_id),
        task_id: typeof row.task_id === "string" ? row.task_id : null,
        run_path: rootPath(root, root, runDir) ?? "",
        state: String(row.state ?? "unknown"),
        status: typeof row.status === "string" ? row.status : null,
        route: typeof adapter === "string" && typeof model === "string"
          ? `${adapter}/${model}${typeof effort === "string" ? `@${effort}` : ""}` : null,
        model: typeof model === "string" ? model : null,
        started_at: typeof row.started_at === "string" ? row.started_at : null,
        last_progress_at: typeof row.last_progress_at === "string" ? row.last_progress_at : null,
        pgid: typeof row.pgid === "number" ? row.pgid : provider?.provider_pgid ?? owner?.owner_pgid ?? null,
        pgid_alive: pid === undefined || started == null ? null
          : processStartedAt(pid) === null ? null : processMatches(pid, started),
        result_path: rootPath(root, runDir, row.paths?.result ?? row.result_path),
        receipt_path: rootPath(root, runDir, receipt),
        writer: row.mode === "worktree_write",
        worktree: typeof row.worktree === "string" ? row.worktree : null,
        attempt: Number(row.attempt ?? 0),
      };
    });
    return { schema: "fabric.runs.v1", status: "ok" as const, runs };
  } catch (error) {
    return { schema: "fabric.runs.v1", status: "unknown" as const,
      error: error instanceof Error ? error.message : String(error), runs: [] as RunRead[] };
  }
}

export async function readEvents(workspace: string, cursor?: string, messages: Message[] = []) {
  try {
  const previous = new Set<string>(cursor ? JSON.parse(Buffer.from(cursor, "base64url").toString("utf8")) : []);
  const source = await statusRows(workspace, undefined, 0, "all", undefined, "brief", false);
  if (!source.runs) return { schema: "fabric.events.v1", status: "unknown", error: source.error, events: [], cursor: cursor ?? "" };
  const root = existsSync(runRoot(workspace)) ? realpathSync(runRoot(workspace)) : resolve(runRoot(workspace));
  const candidates = [
    ...source.runs.flatMap((run) => (Array.isArray(run.attempts) && run.attempts.length ? run.attempts : [run])
      .filter((attempt: Record<string, any>) => attempt.state === "terminal" || attempt.state === "input_required" || attempt.status === "input_required")
      .map((attempt: Record<string, any>) => ({ schema: "fabric.event.v1", type: "task_state",
        id: `${run.run_id}:${run.task_id}:${attempt.attempt ?? run.attempt}:${attempt.status}`,
        run_id: String(run.run_id), task_id: run.task_id ?? null,
        state: attempt.status === "input_required" ? "input_required" : attempt.state,
        status: attempt.status ?? null,
        result_path: rootPath(root, String(run.run_dir), attempt.paths?.result ?? attempt.result_path) }))),
    ...messages.map((row) => ({ schema: "fabric.event.v1", type: "inbox_message", id: row.messageId,
      task_id: row.taskId ?? null, from: row.from, kind: row.kind, preview: row.body.replace(/\s+/gu, " ").slice(0, 80) })),
  ];
  const key = (id: string) => createHash("sha256").update(id).digest("hex").slice(0, 16);
  const events = candidates.filter((row) => !previous.has(key(row.id)));
  const next = Buffer.from(JSON.stringify(candidates.map((row) => key(row.id)))).toString("base64url");
  return { schema: "fabric.events.v1", status: "ok", events, cursor: next };
  } catch (error) {
    return { schema: "fabric.events.v1", status: "unknown", error: error instanceof Error ? error.message : String(error),
      events: [], cursor: cursor ?? "" };
  }
}
