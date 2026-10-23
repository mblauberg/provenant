/** Read retained run status and bounded output through the Fabric front doors. */
import { execFile } from "node:child_process";
import {
  closeSync, constants, fstatSync, lstatSync, openSync,
  readdirSync, readSync, realpathSync, statSync, unlinkSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";
import { projectRoot, runRoot, withoutGitRedirects } from "./identity.js";
import { federatedLane } from "./hosts.js";
import { canonicalSuccessStatus } from "./success-status.js";
import {
  newestMtimeMs, positiveInteger, readJson, readOwnerRecord,
  runDirectoryNames, shortRunId,
} from "./run-registry.js";
import { liveness, observedAlive, reapAttemptRuns, runOwners, type ProcessIdentity } from "./process-identity.js";

const execFileAsync = promisify(execFile);

function readWatchdog(runDir: string, value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "string" || isAbsolute(value)) return undefined;
  let path: string;
  try {
    const root = realpathSync.native(runDir);
    path = realpathSync.native(resolve(root, value));
    const rel = relative(root, path);
    if (isAbsolute(rel) || rel === ".." || rel.startsWith(`..${sep}`)) return undefined;
    if (lstatSync(path).size > 64 * 1024) return undefined;
  } catch { return undefined; }
  const record = readJson(path);
  return record?.schema === "fabric.watchdog.v1" && typeof record.process_alive === "boolean" &&
    typeof record.silent_seconds === "number" && Number.isFinite(record.silent_seconds) && record.silent_seconds >= 0 &&
    typeof record.checked_at === "string" && Number.isFinite(Date.parse(record.checked_at))
    ? record : undefined;
}

/**
 * A launcher that announced an attempt and died before starting any owner: nothing will run it.
 * The launcher stamps its owner into status before writing an owner record, so with no stamp
 * any owner record is an earlier launch's, and it counts only while that owner still lives.
 */
function launcherAbandoned(status: Record<string, unknown> | undefined, owners: ProcessIdentity[]): boolean {
  return status?.owner_pid === undefined && !owners.some((owner) => observedAlive(owner.pid, owner.startedAt)) &&
    status?.status === "running" && status.finished_at === undefined &&
    positiveInteger(status.host_pid) &&
    !observedAlive(status.host_pid, typeof status.host_started_at === "string" ? status.host_started_at : null);
}

/** Status observes retained files and process identities; it never repairs or reaps runs. */
async function legacyStatus(
  workspace: string,
  id?: string,
  waitSeconds = 0,
  signal?: AbortSignal,
  onlyRunDir?: string,
): Promise<Record<string, unknown>> {
  if (!Number.isInteger(waitSeconds) || waitSeconds < 0 || waitSeconds > 55) {
    return { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." };
  }
  try {
    workspace = realpathSync.native(workspace);
  } catch {
    workspace = resolve(workspace);
  }
  const deadline = Date.now() + waitSeconds * 1000;
  while (true) {
    signal?.throwIfAborted();
    const candidates = (onlyRunDir ? [onlyRunDir] : (() => { const base = runRoot(workspace); return runDirectoryNames(workspace).map((name) => join(base, name)); })())
      .flatMap((runDir) => {
        try {
          const metadata = lstatSync(runDir);
          if (!metadata.isDirectory() || metadata.isSymbolicLink()) return [];
          const status = readJson(join(runDir, "dispatch-status.json"));
          const owner = readOwnerRecord(runDir);
          const started = Date.parse(String(status?.started_at ?? owner?.started_at ?? "")) || metadata.birthtimeMs;
          return [{ runDir, status, owner, started, created: metadata.birthtimeMs }];
        } catch {
          return [];
        }
      })
      .sort((a, b) => b.started - a.started || b.created - a.created);
    const exact = candidates.find(
      (run) => run.runDir === id || basename(run.runDir) === id || shortRunId(run.runDir) === id,
    );
    const matches =
      id === undefined
        ? candidates
            .filter((run) => run.status?.status === "running" || run.started >= Date.now() - 86_400_000)
            .slice(0, 20)
        : exact !== undefined
          ? [exact]
          : candidates.filter(
              (run) =>
                [
                  run.status?.id,
                  run.status?.task_id,
                  run.status?.task_alias,
                  run.status?.batch_id,
                  run.owner?.task_id,
                  run.owner?.batch_id,
                ].includes(id) ||
                (Array.isArray(run.status?.task_ids) && run.status.task_ids.includes(id)) ||
                (/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u.test(id) &&
                  readJson(join(run.runDir, "dispatch", "tasks", id, "attempt-001", "attempt.json"))?.task_id === id),
            );
    if (id !== undefined && matches.length === 0) {
      return { status: "rejected", error: "run_not_found", fix: "Pass the id from the dispatch response." };
    }
    const note =
      id !== undefined && matches.length > 1
        ? `Multiple runs match; showing the newest (${basename(matches[0]!.runDir)}).`
        : undefined;
    const rows = (id === undefined ? matches : matches.slice(0, 1)).map(
      ({ runDir, status: initialStatus, owner, started }) => {
        const live = liveness(runDir, runOwners(owner, initialStatus));
        const alive = live.owner || live.provider;
        // Once the owner is dead its attempt files are final. Read them after the probe.
        const status = readJson(join(runDir, "dispatch-status.json")) ?? initialStatus;
        const safePath = (value: unknown): string | undefined => {
          if (typeof value !== "string") return undefined;
          const path = resolve(runDir, value);
          const rel = relative(runDir, path);
          return !isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`) ? path : undefined;
        };
        const attempts: Record<string, unknown>[] = [];
        const outputPaths: string[] = [];
        try {
          const tasksRoot = join(runDir, "dispatch", "tasks");
          if (lstatSync(tasksRoot).isSymbolicLink()) throw new Error("linked tasks");
          for (const task of readdirSync(tasksRoot).slice(0, 64)) {
            const taskDir = join(tasksRoot, task);
            if (lstatSync(taskDir).isSymbolicLink()) continue;
            for (const attempt of readdirSync(taskDir)
              .filter((name) => /^attempt-\d+$/u.test(name))
              .sort()
              .slice(-1)) {
              const attemptDir = join(taskDir, attempt);
              if (lstatSync(attemptDir).isSymbolicLink()) continue;
              const record = readJson(join(attemptDir, "attempt.json"));
              if (record !== undefined) attempts.push(record);
              for (const name of ["result.md", "stderr.log", "adapter-receipt.json"])
                outputPaths.push(join(attemptDir, name));
              const scratch = readJson(join(attemptDir, "provider-output.json"))?.directory;
              if (
                typeof scratch === "string" &&
                isAbsolute(scratch) &&
                basename(scratch).startsWith("fabric-provider-")
              ) {
                try {
                  if (lstatSync(scratch).isSymbolicLink()) continue;
                  for (const directory of readdirSync(scratch)
                    .filter((name) => name.startsWith("cf-dispatch-run."))
                    .slice(0, 32)) {
                    const path = join(scratch, directory);
                    if (lstatSync(path).isSymbolicLink()) continue;
                    for (const name of ["raw", "diag", "combined", "clean"]) outputPaths.push(join(path, name));
                  }
                } catch {
                  /* Owners remove temporary output after retaining the result. */
                }
              }
            }
          }
        } catch {
          /* A new run may not have its first attempt yet. */
        }
        const selectedIndex = id === undefined || !Array.isArray(status?.task_ids) ? -1 : status.task_ids.indexOf(id);
        const selectedTask =
          id !== undefined &&
          id !== status?.id &&
          id !== status?.task_id &&
          id !== status?.batch_id &&
          id !== owner?.task_id &&
          id !== owner?.batch_id
            ? attempts.find((attempt) => attempt.task_id === id)
            : undefined;
        const batchId = owner?.batch_id ?? status?.batch_id ?? (status?.kind === "batch" ? status.id : undefined);
        const summaryPath =
          typeof batchId === "string" && /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u.test(batchId)
            ? join(runDir, "dispatch", "batches", batchId, "summary.json")
            : undefined;
        const summary = summaryPath === undefined ? undefined : readJson(summaryPath);
        const route = (selectedTask?.route ??
          (selectedIndex >= 0 ? (status?.routes as unknown[] | undefined)?.[selectedIndex] : undefined) ??
          status?.route ??
          attempts[0]?.route ??
          (status?.routes as unknown[] | undefined)?.[0] ??
          {}) as Record<string, unknown>;
        const terminal =
          attempts.length > 0 &&
          attempts.every((attempt) =>
            ["ok", "quarantined", "failed", "blocked", "timed_out", "cancelled", "lost"].includes(String(canonicalSuccessStatus(attempt.status))),
          ) &&
          (batchId === undefined ||
            (Array.isArray(status?.task_ids) &&
              status.task_ids.every((taskId) => attempts.some((attempt) => attempt.task_id === taskId))));
        let state = String(status?.status ?? "running");
        if (state === "running" && !alive) {
          state = ["completed", "failed", "cancelled"].includes(String(summary?.status))
            ? String(summary!.status)
            : terminal
              ? batchId === undefined
                ? String(attempts[0]!.status)
                : "completed"
              : "interrupted";
        }
        if (alive) state = "running";
        if (selectedTask !== undefined) state = String(selectedTask.status);
        state = String(canonicalSuccessStatus(state));
        // Owner logs are siblings created by the front door, never arbitrary receipt paths.
        outputPaths.push(`${runDir}-owner.stdout.jsonl`, `${runDir}-owner.stderr.log`);
        const newest = Math.max(started, newestMtimeMs(outputPaths));
        const silence = Math.max(0, (Date.now() - newest) / 1000);
        const paths = status?.paths as Record<string, unknown> | undefined;
        const profile = (selectedTask ?? attempts[0] ?? status)?.profile as { knobs?: Record<string, unknown> } | undefined;
        const savedSilence = profile?.knobs?.["lanes.silence_seconds"];
        const silenceSeconds = typeof savedSilence === "number" && savedSilence >= 1 && savedSilence <= 3600
          ? savedSilence : 300;
        const result = (selectedTask ?? attempts[0])?.result as Record<string, unknown> | undefined;
        const finished = Date.parse(
          String(
            selectedTask?.finished_at ??
              status?.finished_at ??
              summary?.finished_at ??
              attempts.at(-1)?.finished_at ??
              "",
          ),
        );
        return {
          id:
            selectedTask?.task_id ??
            (selectedIndex >= 0 ? id : undefined) ??
            status?.id ??
            owner?.task_id ??
            owner?.batch_id ??
            basename(runDir),
          run_dir: runDir,
          adapter: route.adapter ?? null,
          model: route.resolved_model ?? route.model ?? null,
          status: state,
          elapsed_seconds: Math.round(
            Math.max(0, ((state === "running" || !finished ? Date.now() : finished) - started) / 1000),
          ),
          output_age_seconds: Math.round(silence),
          stalled:
            state === "running" && alive && silence >= silenceSeconds,
          result_path:
            safePath(
              selectedTask === undefined
                ? (paths?.result ?? paths?.summary ?? (summary === undefined ? result?.path : summaryPath))
                : result?.path,
            ) ?? null,
        };
      },
    );
    if (id === undefined) return { runs: rows };
    const row = { ...rows[0]!, ...(note === undefined ? {} : { note }) };
    if (row.status !== "running" || Date.now() >= deadline) return row;
    await new Promise((done) => setTimeout(done, Math.min(250, deadline - Date.now())));
  }
}
export interface StatusResult extends Record<string, unknown> {
  runs?: Record<string, any>[];
}
async function ledger(worktree: unknown): Promise<Record<string, unknown>> {
  const empty = { worktree: worktree ?? null, branch_tip: null, dirty: null, ahead: null };
  if (typeof worktree !== "string") return empty;
  try {
    const git = async (...args: string[]) =>
      (await execFileAsync("git", ["-C", worktree, ...args], {
        env: withoutGitRedirects(process.env),
        encoding: "utf8",
        timeout: 1000,
      })).stdout.trim();
    const branch_tip = await git("rev-parse", "HEAD");
    const status = await git("status", "--porcelain=v2", "--branch", "--untracked-files=normal");
    const ahead = status.match(/^# branch\.ab \+(\d+)/mu)?.[1];
    return {
      worktree,
      branch_tip,
      dirty: status.split("\n").some((line) => line && !line.startsWith("#")),
      ahead: ahead === undefined ? null : Number(ahead),
    };
  } catch {
    return empty;
  }
}
function v1Rows(runDir: string): Record<string, any>[] {
  const grouped = new Map<string, Record<string, any>[]>();
  for (const tree of ["tasks", "dispatch/tasks"]) {
    const tasks = join(runDir, tree);
    try {
      if (lstatSync(tasks).isSymbolicLink()) continue;
      for (const task of readdirSync(tasks, { withFileTypes: true })
        .filter((d) => d.isDirectory())
        .slice(0, 64)) {
        const dir = join(tasks, task.name);
        for (const attempt of readdirSync(dir, { withFileTypes: true }).filter(
          (d) => d.isDirectory() && /^attempt-\d+$/u.test(d.name),
        )) {
          const row = readJson(join(dir, attempt.name, "attempt.json"));
          if (row?.schema !== "fabric.attempt.v1") continue;
          const watchdog = readWatchdog(runDir, (row.paths as Record<string, unknown> | undefined)?.watchdog ??
            relative(runDir, join(dir, attempt.name, "watchdog.json")));
          if (watchdog) row.watchdog = watchdog;
          const rows = grouped.get(task.name) ?? [];
          rows.push(row);
          grouped.set(task.name, rows);
        }
      }
    } catch {
      /* An owner may still be creating its task directory. */
    }
  }
  if (grouped.size === 0) {
    const receipt = readJson(join(runDir, "RUN_RECEIPT.json"));
    const rows = Array.isArray(receipt?.attempts) ? receipt.attempts : [];
    for (const row of rows)
      if (row?.schema === "fabric.attempt.v1") {
        const values = grouped.get(String(row.task_id)) ?? [];
        values.push(row);
        grouped.set(String(row.task_id), values);
      }
  }
  const metadata = readJson(join(runDir, "dispatch-status.json"));
  const summary =
    typeof metadata?.batch_id === "string" && /^[A-Za-z0-9._-]+$/u.test(metadata.batch_id)
      ? readJson(join(runDir, "dispatch/batches", metadata.batch_id, "summary.json"))
      : undefined;
  // A batch rejected at preflight has a summary but no attempts; anything else empty has no rows.
  if (grouped.size === 0 && !Array.isArray(summary?.tasks)) return [];
  const receipt = readJson(join(runDir, "RUN_RECEIPT.json"));
  const owners = runOwners(readOwnerRecord(runDir), metadata);
  const live = liveness(runDir, owners);
  const ownerAlive = live.owner;
  const dead = owners.length > 0 && !live.owner && !live.provider;
  const closed = metadata?.finished_at !== undefined && metadata?.status !== "running";
  const interrupted = !ownerAlive && (receipt?.status === "interrupted" || dead || closed);
  // Only the attempt the dead launcher announced is closed; any attempt an owner wrote stands.
  const abandoned = launcherAbandoned(metadata, owners);
  const closureStatus = receipt?.status === "cancelled" || metadata?.status === "cancelled" ? "cancelled" : "interrupted";
  // A closure the owner never wrote takes the run's recorded finish; unknown stays null.
  const time = (value: unknown) => (typeof value === "string" ? value : null);
  const closedAt = closed ? time(metadata?.finished_at) : null;
  const rows: Record<string, any>[] = [...grouped.values()].map((attempts) => {
    const row = attempts.sort((a, b) => Number(a.attempt) - Number(b.attempt)).at(-1)!;
    const routeIndex = Array.isArray(metadata?.task_ids) ? metadata.task_ids.indexOf(row.task_id) : 0;
    const route = Array.isArray(metadata?.routes) ? metadata.routes[routeIndex] : undefined;
    const notes = Array.isArray(route?.notes) ? route.notes.filter((note: unknown) => typeof note === "string") : [];
    const pending = metadata?.task_id === row.task_id && Number(metadata?.next_attempt ?? 0) > Number(row.attempt);
    const effective =
      interrupted && row.state !== "terminal"
        ? {
            ...row,
            state: "terminal",
            status: closureStatus,
            ended_at: row.ended_at ?? closedAt,
            digest: `${closureStatus} ${row.run_id}${closureStatus === "interrupted" ? " · fix: dispatch a new run" : ""}`,
          }
        : row;
    if (pending) {
      const ended = interrupted || abandoned;
      const status = closed && metadata?.status === "rejected" ? "rejected" : ended ? closureStatus : null;
      const fix = metadata?.fix ?? metadata?.message ??
        (abandoned ? "The launcher exited before starting the run owner; retry." : "Dispatch a new run; the owner exited.");
      return {
        schema: "fabric.status.v1",
        id: row.run_id,
        run_id: row.run_id,
        run_dir: runDir,
        task_id: row.task_id,
        attempt: metadata!.next_attempt,
        attempts: attempts.map((attempt) => ({ ...attempt, status: canonicalSuccessStatus(attempt.status) })),
        attempt_count: attempts.length,
        state: ended ? "terminal" : "queued",
        status,
        ended_at: ended ? closedAt : null,
        ...(ended ? { fix, message: metadata?.message, ...(status === "rejected" && typeof metadata?.error === "string" ? { error: metadata.error } : {}) } : {}),
        started_at: metadata!.started_at,
        digest: ended ? `${status} ${row.run_id} · fix: ${fix}` : `running ${row.run_id} attempt ${metadata!.next_attempt} · fabric_status{ids:["${row.run_id}"],wait_seconds:55}`,
        paths: {},
      };
    }
    return {
      ...effective,
      status: canonicalSuccessStatus(effective.status),
      ...(notes.length ? { notes } : {}),
      schema: "fabric.status.v1",
      id: row.run_id,
      run_dir: runDir,
      attempts: attempts.map((attempt) => ({ ...attempt, status: canonicalSuccessStatus(attempt.status) })),
      attempt_count: attempts.length,
      ...(metadata?.batch_id ? { batch_id: metadata.batch_id } : {}),
      result_path: typeof row.paths?.result === "string" ? resolve(runDir, row.paths.result) : null,
    };
  });
  // A batch cannot finish before every manifest task has a row.
  if (Array.isArray(metadata?.task_ids))
    for (const id of metadata.task_ids) {
      if (rows.some((row) => row.task_id === id)) continue;
      const terminalTask = Array.isArray(summary?.tasks)
        ? summary.tasks.find((task) => task.task_id === id)
        : undefined;
      const terminal = interrupted || (summary?.status !== undefined && summary.status !== "running");
      rows.push({
        schema: "fabric.status.v1",
        id: metadata.id,
        run_id: metadata.id ?? shortRunId(runDir),
        run_dir: runDir,
        task_id: id,
        batch_id: metadata.batch_id,
        attempt: 0,
        attempts: [],
        attempt_count: 0,
        ...(terminalTask?.outcome === undefined ? {} : { outcome: terminalTask.outcome }),
        ...(typeof terminalTask?.error === "string" ? { error: terminalTask.error } : {}),
        ...(typeof terminalTask?.fix === "string" ? { fix: terminalTask.fix } : {}),
        state: terminal ? "terminal" : "queued",
        status: terminal ? canonicalSuccessStatus(terminalTask?.status ?? "interrupted") : null,
        started_at: metadata.started_at,
        ended_at: terminal ? time(terminalTask?.finished_at) ?? time(summary?.finished_at) ?? closedAt : null,
        paths: {},
        digest: `${terminal ? canonicalSuccessStatus(terminalTask?.status ?? "interrupted") : "queued"} ${id}${typeof terminalTask?.error === "string" ? `: ${terminalTask.error}` : ""}`,
      });
    }
  const order = Array.isArray(metadata?.task_ids) ? metadata.task_ids.map(String) : []; // manifest order, else task-2 before task-10
  const rank = (row: Record<string, any>) => { const at = order.indexOf(String(row.task_id)); return at < 0 ? order.length : at; };
  return rows.sort((a, b) => rank(a) - rank(b) || String(a.task_id).localeCompare(String(b.task_id), "en", { numeric: true }));
}

function wrapLegacyRow(row: Record<string, any>): Record<string, any> {
  const metadata = readJson(join(row.run_dir, "dispatch-status.json"));
  const owner = readOwnerRecord(row.run_dir);
  return {
    ...row,
    schema: "fabric.status.v1",
    run_id: shortRunId(row.run_dir),
    task_id: metadata?.task_id ?? owner?.task_id,
    batch_id: metadata?.batch_id ?? owner?.batch_id,
    state: row.status === "running" ? "running" : "terminal",
    attempt: 1,
    attempts: [row],
    attempt_count: 1,
    started_at: metadata?.started_at ?? owner?.started_at ?? new Date(statSync(row.run_dir).mtimeMs).toISOString(),
    paths: metadata?.paths ?? { result: row.result_path },
    worktree: metadata?.worktree ?? null,
  };
}

/** v1 is authoritative; the old reader remains isolated for wave-1 receipts. */
export async function statusRows(
  workspace: string,
  ids?: string[],
  waitSeconds = 0,
  until: "any" | "all" = "all",
  signal?: AbortSignal,
  detail: "brief" | "full" = "brief",
  includeLedger = true,
  limit: number | null = 20,
  includeHistory = false,
  includePointers = false,
): Promise<StatusResult> {
  if (!Number.isInteger(waitSeconds) || waitSeconds < 0 || waitSeconds > 55)
    return { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." };
  if (limit !== null && (!Number.isSafeInteger(limit) || limit < 0))
    return { status: "rejected", error: "limit_invalid", fix: "Pass a non-negative row limit or null." };
  const deadline = Date.now() + waitSeconds * 1000;
  for (;;) {
    signal?.throwIfAborted();
    const root = runRoot(workspace);
    const candidates = runDirectoryNames(workspace).map((name) => join(root, name));
    const project = projectRoot(workspace);
    const imported: Record<string, any>[] = [];
    const discoveryErrors: string[] = [];
    const pointers = join(root, "runs/pointers");
    let entries: string[] = [];
    try {
      if (includePointers && !lstatSync(pointers).isSymbolicLink())
        entries = readdirSync(pointers).filter((name) => name.endsWith(".json"));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") discoveryErrors.push(String(error));
    }
    for (const name of entries) {
      signal?.throwIfAborted();
      try {
        const pointerPath = join(pointers, name);
        const pointer = readJson(pointerPath);
        if (!pointer || typeof pointer.run_dir !== "string" || !isAbsolute(pointer.run_dir) ||
            !Array.isArray(pointer.task_ids) || typeof pointer.source_project !== "string") continue;
        const runDir = pointer.run_dir;
        const modern = /^\d{8}-\d{4}-(dispatch|batch)-/u.test(basename(runDir)) && basename(dirname(runDir)) === "runs";
        const sourceRoot = modern ? dirname(dirname(runDir)) : dirname(runDir);
        if (!modern || basename(sourceRoot) !== ".agent-run") continue;
        let info;
        try { info = lstatSync(runDir); }
        catch (error) {
          if ((error as NodeJS.ErrnoException).code === "ENOENT") {
            // A concurrent reader may have retired the same pointer first.
            try { unlinkSync(pointerPath); }
            catch (unlinkError) { if ((unlinkError as NodeJS.ErrnoException).code !== "ENOENT") throw unlinkError; }
            continue;
          }
          throw error;
        }
        if (!info.isDirectory() || info.isSymbolicLink() || candidates.includes(runDir)) continue;
        const metadata = readJson(join(runDir, "dispatch-status.json"));
        if (!metadata) throw new Error(`run_read_failed: ${runDir}`);
        const projects = metadata.cwd_projects;
        // Dispatch writes canonical projectRoot values. A hand-made or older
        // status may hold a symlinked spelling (/var vs /private/var), so each
        // value is realpath-canonicalised on read; a value that still does not
        // name this project (for example a worktree path rather than its
        // project root) is treated as another project's.
        const named = (task: unknown): boolean => {
          const value = projects && typeof projects === "object" && !Array.isArray(projects) && typeof task === "string"
            ? (projects as Record<string, unknown>)[task] : undefined;
          if (typeof value !== "string" || !isAbsolute(value)) return false;
          if (value === project) return true;
          try { return realpathSync.native(value) === project; } catch { return false; }
        };
        // A resume that moved every pointed task elsewhere leaves this index
        // stale. It is skipped, not unlinked: a later resume may move the task
        // back and atomically replace this pointer after this read, and only
        // the source reaper decides when a run's index may go (ENOENT above).
        if (!pointer.task_ids.some(named)) continue;
        const finished = metadata.status !== "running" && typeof metadata.finished_at === "string"
          ? Date.parse(metadata.finished_at) : Number.NaN;
        // Outside the lanes window: skip before reading any receipt. Age never
        // prunes a pointer; the source reaper's eligibility (resumable runs,
        // failed-run retention, liveness) governs, and its removal of the run
        // directory is what retires the pointer.
        if (!ids?.length && Number.isFinite(finished) && !includeHistory && finished <= Date.now() - 86400000) continue;
        const taskIds = Array.isArray(metadata.task_ids) ? metadata.task_ids : [];
        const routes = Array.isArray(metadata.routes) ? metadata.routes : [];
        const recorded = v1Rows(runDir);
        const hasRoute = (row: Record<string, any> | undefined): boolean => {
          const route = row?.provenance?.requested ?? row?.route ?? row;
          return typeof route?.adapter === "string" &&
            [row?.provenance?.resolved_model, route.resolved_model, route.model, route.alias]
              .some((value) => typeof value === "string" && value.length > 0);
        };
        // The pointer is only an index. The source must independently name
        // this target and task with a route; source_project is display-only.
        const owner = readOwnerRecord(runDir);
        // Closure can remove dispatch-owner.json. Its stamped identity in
        // dispatch-status.json survives, together with the task routes.
        const owned = (!!owner && typeof owner.workspace === "string" && runRoot(owner.workspace) === sourceRoot) ||
          (positiveInteger(metadata.owner_pid) && typeof metadata.run_token === "string" &&
            (metadata.kind === "dispatch" || metadata.kind === "batch"));
        const tasks = pointer.task_ids.filter((task): task is string => {
          if (typeof task !== "string" || !named(task)) return false;
          const index = taskIds.indexOf(task);
          return (owned && index >= 0 && hasRoute(routes[index])) ||
            recorded.some((row) => row.task_id === task && (hasRoute(row) || row.attempts?.some(hasRoute)));
        });
        if (!tasks.length) continue;
        let rows = recorded;
        if (!rows.length) {
          const legacy = await legacyStatus(workspace, runDir, 0, signal, runDir);
          if (legacy.error) throw new Error(String(legacy.error));
          rows = [wrapLegacyRow(legacy)];
        }
        for (const row of rows) {
          // Aggregate filtering/sorting must not coerce corrupt foreign facts.
          if ([row.started_at, row.ended_at].some((value) => value != null && typeof value !== "string"))
            throw new Error(`run_read_failed: invalid timestamp in ${runDir}`);
          if (typeof row.task_id === "string") {
            if (tasks.includes(row.task_id)) imported.push({ ...row, source_project: pointer.source_project });
          } else {
            // A batch has only an owner row before its first task receipt.
            for (const task of tasks) {
              const route = routes[taskIds.indexOf(task)];
              // Corroborated by a receipt alone, a task may be absent from
              // metadata.task_ids; its own receipt row will carry the route.
              if (!route) continue;
              imported.push({ ...row, id: task, task_id: task, source_project: pointer.source_project,
                adapter: route.adapter, model: route.resolved_model ?? route.model ?? route.alias });
            }
          }
        }
      } catch (error) {
        if (signal?.aborted) throw error;
        discoveryErrors.push(`${name}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    const v1 = [...candidates.flatMap(v1Rows), ...imported];
    const v1Dirs = new Set(v1.map((row) => row.run_dir));
    let rows: Record<string, any>[] = [];
    if (ids?.length) {
      for (const id of ids) {
        const exact = v1.filter((row) => [row.run_id, row.run_dir, basename(row.run_dir)].includes(id));
        const named = v1.filter((row) => [row.batch_id, row.task_id, row.id].includes(id));
        const matches = exact.length ? exact : named.length ? named : v1.filter((row) => {
          const metadata = readJson(join(row.run_dir, "dispatch-status.json"));
          return metadata?.task_alias === id && metadata.task_id === row.task_id;
        });
        if (matches.length) {
          const newest = matches.reduce((best, row) => (Date.parse(row.started_at) > Date.parse(best.started_at) ? row : best)); // tasks keep batch order
          rows.push(...matches.filter((row) => row.run_dir === newest.run_dir));
        } else {
          const legacy = await legacyStatus(workspace, id);
          if (legacy.error) return legacy;
          rows.push(wrapLegacyRow(legacy));
        }
      }
      rows = rows.filter(
        (row, index) =>
          rows.findIndex((other) => other.run_dir === row.run_dir && other.task_id === row.task_id) === index,
      );
    } else {
      const legacy = await legacyStatus(workspace);
      rows = [
        ...v1,
        ...((legacy.runs ?? []) as Record<string, any>[]).filter((row) => !v1Dirs.has(row.run_dir)).map(wrapLegacyRow),
      ]
        .filter((row) => includeHistory || row.state !== "terminal" || Date.parse(row.ended_at ?? row.started_at) > Date.now() - 86400000)
        .sort((a, b) => Date.parse(b.started_at) - Date.parse(a.started_at));
    }
    const omitted = ids?.length || limit === null ? 0 : Math.max(0, rows.length - limit);
    if (limit !== null && !ids?.length) rows = rows.slice(0, limit);
    const reaped = await reapAttemptRuns(workspace, rows.filter((row) =>
      row.attempts?.some((attempt: Record<string, unknown>) => attempt.state === "running"))
      .map((row) => row.run_dir));
    if (reaped.length) {
      const refreshed = reaped.flatMap(v1Rows);
      rows = rows.map((row) => ({ ...row, ...refreshed.find((fresh) => fresh.run_dir === row.run_dir && fresh.task_id === row.task_id) }));
    }
    const done =
      until === "any" ? rows.some((row) => row.state === "terminal") : rows.every((row) => row.state === "terminal");
    if (done || !rows.length || Date.now() >= deadline) {
      const cache = new Map<unknown, Promise<Record<string, unknown>>>();
      const enriched = await Promise.all(rows.map(async (row) => {
        if (!includeLedger) return row;
        if (row.state === "terminal" && detail !== "full") return row;
        if (!cache.has(row.worktree)) cache.set(row.worktree, ledger(row.worktree));
        return { ...row, ...await cache.get(row.worktree) };
      }));
      return { schema: "fabric.status.v1", runs: enriched, ...(discoveryErrors.length ? { discovery_error: discoveryErrors.join("; ") } : {}), ...(omitted ? { omitted } : {}) };
    }
    await new Promise((done) => setTimeout(done, Math.min(100, deadline - Date.now())));
  }
}

export async function federatedStatusRows(
  workspace: string, ids?: string[], waitSeconds = 0, until: "any" | "all" = "all",
  signal?: AbortSignal, detail: "brief" | "full" = "brief", includeLedger = true, limit: number | null = 20,
  includeHistory = false,
  includePointers = false,
): Promise<StatusResult> {
  if (!Number.isInteger(waitSeconds) || waitSeconds < 0 || waitSeconds > 55)
    return { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." };
  const deadline = Date.now() + waitSeconds * 1000;
  const request = { ids, wait_seconds: 0, until, detail, limit, include_history: includeHistory, include_pointers: includePointers };
  for (;;) {
    const remote = await federatedLane("status", workspace, request, signal);
    if (!remote) return statusRows(workspace, request.ids, waitSeconds, until, signal, detail, includeLedger, limit, includeHistory, includePointers);
    const done = (row: Record<string, any>) => row.reachability !== "unreachable" && ["terminal", "input_required"].includes(row.state);
    const rows = remote.runs ?? [];
    if (remote.status !== "ok" || !waitSeconds || Date.now() >= deadline || !rows.length || (until === "any" ? rows.some(done) : rows.every(done))) return remote as StatusResult;
    await new Promise((resolveWait) => setTimeout(resolveWait, Math.min(2000, deadline - Date.now())));
    signal?.throwIfAborted();
  }
}

export async function fabricStatus(
  workspace: string,
  id?: string,
  waitSeconds = 0,
  signal?: AbortSignal,
): Promise<Record<string, any>> {
  const result = await federatedStatusRows(workspace, id ? [id] : undefined, waitSeconds, "all", signal, "brief", true, 20, false, true);
  if (id && result.runs?.length === 1) return result.runs[0]!;
  return result;
}

export async function fabricOutput(
  workspace: string,
  input: { id: string; part?: string; offset?: number; max_bytes?: number; tail?: boolean },
) {
  const remote = await federatedLane("output", workspace, input);
  if (remote) return remote;
  const result = await statusRows(workspace, [input.id], 0, "all", undefined, "brief", false, null, false, true);
  if (!result.runs) return result;
  if (result.runs.length !== 1)
    return { status: "rejected", error: "output_ambiguous", fix: "Pass a task id for batch output." };
  const row = result.runs[0]!;
  const part = input.part ?? "result";
  const raw = row.paths?.[part] ?? (part === "result" ? row.result_path : undefined);
  if (typeof raw !== "string")
    return { status: "rejected", error: "output_unavailable", fix: `Wait for the owner to publish ${part}.` };
  let offset = input.offset ?? 0;
  const max = input.max_bytes ?? 4000;
  if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isInteger(max) || max < 1 || max > 20000)
    return { status: "rejected", error: "output_bounds", fix: "Use offset >= 0 and max_bytes 1–20000." };
  let fd: number | undefined;
  try {
    const root = realpathSync.native(row.run_dir),
      path = realpathSync.native(resolve(root, raw));
    const rel = relative(root, path);
    if (isAbsolute(rel) || rel === ".." || rel.startsWith(`..${sep}`)) throw new Error("output escapes run directory");
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    if (!fstatSync(fd).isFile()) throw new Error("output is not a regular file");
    if (input.tail && input.offset !== undefined)
      return { status: "rejected", error: "output_bounds", fix: "Use tail or offset, not both." };
    const size = fstatSync(fd).size;
    if (input.tail) offset = Math.max(0, size - max);
    const bytes = Buffer.alloc(max + 3);
    const available = readSync(fd, bytes, 0, bytes.length, offset);
    const continuation = (byte: number) => (byte & 0xc0) === 0x80;
    let start = 0, count = Math.min(max, available);
    if (input.tail) while (start < count && continuation(bytes[start]!)) start++;
    else if (available && continuation(bytes[0]!))
      return { status: "rejected", error: "output_bounds", fix: "Use a UTF-8 boundary from next_offset." };
    if (count < available) while (count > start && continuation(bytes[count]!)) count--;
    if (count === start && available > start)
      return { status: "rejected", error: "output_bounds", fix: "Use max_bytes of at least 4 for UTF-8 text." };
    const text = bytes.subarray(start, count).toString("utf8");
    return {
      id: input.id,
      part,
      offset: offset + start,
      next_offset: offset + count,
      eof: offset + count >= fstatSync(fd).size,
      digest: text,
    };
  } catch (error) {
    return { status: "rejected", error: "output_unavailable", fix: `Read a retained output file: ${String(error)}` };
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

/**
 * The last `chars` characters of a terminal row's result, so the chair need not
 * open result.md for a short answer. Same containment rules as fabricOutput;
 * an unreadable or empty result yields nothing rather than an error.
 */
export function resultTail(row: Record<string, any>, chars: number): { text: string; total: number } | undefined {
  const raw = row.paths?.result ?? row.result_path;
  if (typeof raw !== "string" || !row.run_dir || chars < 1) return undefined;
  let fd: number | undefined;
  try {
    const root = realpathSync.native(row.run_dir), path = realpathSync.native(resolve(root, raw));
    const rel = relative(root, path);
    if (isAbsolute(rel) || rel === ".." || rel.startsWith(`..${sep}`)) return undefined;
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size === 0) return undefined;
    const bytes = Buffer.alloc(Math.min(stat.size, chars * 4));
    const read = readSync(fd, bytes, 0, bytes.length, stat.size - bytes.length);
    let start = 0;
    while (start < read && (bytes[start]! & 0xc0) === 0x80) start++;
    const whole = bytes.subarray(start, read).toString("utf8");
    return { text: [...whole].slice(-chars).join("").trimStart(), total: stat.size };
  } catch {
    return undefined;
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}
