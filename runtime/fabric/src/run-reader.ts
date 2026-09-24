import { existsSync, lstatSync, realpathSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { basename, dirname, isAbsolute, relative, resolve, sep } from "node:path";

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

export interface RunReadResponse {
  schema: "fabric.runs.v1";
  status: "ok" | "unknown";
  error?: string;
  runs: RunRead[];
  claims?: unknown;
}

function rootPath(root: string, runDir: string, value: unknown): string | null {
  if (typeof value !== "string" || !value) return null;
  const path = resolve(runDir, value);
  let ancestor = path;
  const missing: string[] = [];
  try {
    if (lstatSync(path).isSymbolicLink()) return null;
  } catch { /* A result may not exist yet. */ }
  for (;;) {
    try { lstatSync(ancestor); break; }
    catch {
      const parent = dirname(ancestor);
      if (parent === ancestor) return null;
      missing.unshift(basename(ancestor));
      ancestor = parent;
    }
  }
  let candidate: string;
  let canonicalRunDir: string;
  try {
    if (missing.length && !statSync(ancestor).isDirectory()) return null;
    candidate = resolve(realpathSync(ancestor), ...missing);
    canonicalRunDir = realpathSync(runDir);
  } catch { return null; }
  const local = relative(canonicalRunDir, candidate);
  const rel = relative(root, candidate);
  return rel && !isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`) &&
    !isAbsolute(local) && local !== ".." && !local.startsWith(`..${sep}`) ? rel : null;
}

/** The public read contract. Receipt layout and absolute paths end here. */
export async function readRuns(workspace: string, ids?: string[], waitSeconds = 0, signal?: AbortSignal): Promise<RunReadResponse> {
  try {
    const source = await statusRows(workspace, ids, waitSeconds, "all", signal, "brief", false);
    if (!source.runs) return { schema: "fabric.runs.v1", status: "unknown" as const,
      error: String(source.error ?? "run_read_failed"), runs: [] as RunRead[] };
    const root = existsSync(runRoot(workspace)) ? realpathSync(runRoot(workspace)) : resolve(runRoot(workspace));
    const runs: RunRead[] = source.runs.map((row) => {
      const runDir = String(row.run_dir);
      const latest = Array.isArray(row.attempts) ? row.attempts.at(-1) : undefined;
      const resultForReceipt = latest?.paths?.result ?? row.paths?.result;
      const taskId = typeof row.task_id === "string" ? row.task_id : "";
      const attempt = Number(latest?.attempt ?? row.attempt);
      const layoutReceipt = /^[A-Za-z0-9][A-Za-z0-9._-]*$/u.test(taskId) && Number.isSafeInteger(attempt) && attempt > 0
        ? ["tasks", "dispatch/tasks"].map((tree) => `${tree}/${taskId}/attempt-${String(attempt).padStart(3, "0")}/attempt.json`)
          .find((path) => existsSync(resolve(runDir, path)))
        : undefined;
      const receipt = row.paths?.receipt ?? latest?.paths?.receipt ??
        (typeof resultForReceipt === "string" ? `${dirname(resultForReceipt)}/attempt.json` : layoutReceipt);
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
  const previous = cursor
    ? JSON.parse(Buffer.from(cursor, "base64url").toString("utf8")) as { runs: Record<string, [number, string]>; messages: string[] }
    : { runs: {}, messages: [] };
  const source = await statusRows(workspace, undefined, 0, "all", undefined, "brief", false);
  if (!source.runs) return { schema: "fabric.events.v1", status: "unknown", error: source.error, events: [], cursor: cursor ?? "" };
  const root = existsSync(runRoot(workspace)) ? realpathSync(runRoot(workspace)) : resolve(runRoot(workspace));
  const key = (id: string) => createHash("sha256").update(id).digest("hex").slice(0, 16);
  const runMarks: Record<string, [number, string]> = {};
  const events: Record<string, unknown>[] = [];
  for (const run of source.runs) {
    const runKey = key(`${run.run_id}:${run.task_id}`);
    const attempts = Array.isArray(run.attempts) && run.attempts.length ? run.attempts : [run];
    for (const attempt of attempts) {
      if (attempt.state !== "terminal" && attempt.state !== "input_required" && attempt.status !== "input_required") continue;
      const number = Number(attempt.attempt ?? run.attempt);
      if (!Number.isSafeInteger(number)) continue;
      const state = key(`${attempt.state}:${attempt.status}`);
      const mark = runMarks[runKey];
      if (!mark || number >= mark[0]) runMarks[runKey] = [number, state];
      const prior = previous.runs[runKey];
      if (prior && (number < prior[0] || (number === prior[0] && state === prior[1]))) continue;
      events.push({ schema: "fabric.event.v1", type: "task_state",
        id: `${run.run_id}:${run.task_id}:${number}:${attempt.status}`,
        run_id: String(run.run_id), task_id: run.task_id ?? null,
        state: attempt.status === "input_required" ? "input_required" : attempt.state,
        status: attempt.status ?? null,
        result_path: rootPath(root, String(run.run_dir), attempt.paths?.result ?? attempt.result_path) });
    }
  }
  const messageKeys = messages.slice(0, 100).map((row) => key(row.messageId));
  const seenMessages = new Set(previous.messages);
  for (const [index, row] of messages.slice(0, 100).entries()) {
    if (seenMessages.has(messageKeys[index]!)) continue;
    events.push({ schema: "fabric.event.v1", type: "inbox_message", id: row.messageId,
      task_id: row.taskId ?? null, from: row.from, kind: row.kind, preview: row.body.replace(/\s+/gu, " ").slice(0, 80) });
  }
  const next = Buffer.from(JSON.stringify({ runs: runMarks, messages: messageKeys })).toString("base64url");
  return { schema: "fabric.events.v1", status: "ok", events, cursor: next };
  } catch (error) {
    return { schema: "fabric.events.v1", status: "unknown", error: error instanceof Error ? error.message : String(error),
      events: [], cursor: cursor ?? "" };
  }
}
