/**
 * Named provider sessions: a project-scoped alias over the existing run resume
 * path (ADR 0022). A turn is an ordinary dispatch, resume or handoff; this
 * module only chooses which, serialises turns per name and moves the alias
 * after a clean turn. Busy state is reconciled from run state on every read.
 */
import { InputError, rejected, type DispatchInput } from "./execution-input.js";
import { dispatchConfiguredProvider } from "./execution.js";
import type { Identity } from "./identity.js";
import { handoffDispatch, resumeConfiguredProvider } from "./resume.js";
import { statusRows } from "./run-registry.js";
import { canonicalSuccessStatus } from "./success-status.js";
import type { NamedSession, SessionTurn, Store } from "./store.js";

/** The owner relaunches Copilot instead of resuming it (dispatch_run.prepare_resume). */
const NO_NATIVE_CONTINUATION = new Set(["copilot"]);
/** A turn the provider ended cleanly; `input_required` waits for its answer by resume. */
const CLEAN = new Set(["ok", "input_required"]);

function alive(pid: number | null): boolean {
  if (pid === null) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

export function sessionName(value: unknown): string {
  if (typeof value !== "string" || !value.trim() || value.length > 128 || /[\u0000-\u001f]/u.test(value))
    throw new InputError("session_invalid", "Pass session as a 1–128 character name.");
  return value;
}

async function taskRow(cwd: string, runId: string, taskId: string | null) {
  const runs = (await statusRows(cwd, [runId], 0, "all", undefined, "brief", false)).runs ?? [];
  return runs.find((row) => row.task_id === taskId) ?? (taskId === null && runs.length === 1 ? runs[0] : undefined);
}

/** Settle a finished or abandoned turn from run state; return the current row. */
export async function reconcileSession(store: Store, who: Identity, name: string): Promise<NamedSession | undefined> {
  const row = store.session(who.project, name);
  if (!row || row.turnStatus !== null || alive(row.turnPid)) return row;
  let status = "interrupted",
    current: Record<string, any> | undefined;
  if (row.turnRunId !== null) {
    const task = await taskRow(who.cwd, row.turnRunId, row.turnTaskId);
    current = task && (Number(task.attempt) === row.turnAttempt
      ? task
      : task.attempts?.find((attempt: Record<string, any>) => Number(attempt.attempt) === row.turnAttempt));
    // The owner is still creating the attempt it was launched for.
    if (!current && task && task.state !== "terminal") return row;
    if (current && current.state !== "terminal") return row;
    if (current) status = String(canonicalSuccessStatus(current.status) ?? "interrupted");
  }
  const clean = current && CLEAN.has(status) && typeof current.task_id === "string";
  store.settleSessionTurn(who, name, row.turnClaim, status, clean ? {
    adapter: current!.provenance?.transport ?? current!.provenance?.requested?.adapter ?? current!.adapter ?? null,
    providerSessionId: typeof current!.session_id === "string" && current!.session_id ? current!.session_id : null,
    runId: row.turnRunId!,
    taskId: current!.task_id,
    attempt: Number(current!.attempt),
    resultPath: typeof current!.result_path === "string" ? current!.result_path
      : typeof current!.paths?.result === "string" ? current!.paths.result : null,
  } : undefined);
  return store.session(who.project, name);
}

export function sessionView(row: NamedSession): Record<string, unknown> {
  const active = row.turnStatus === null;
  const pointer = row.runId === null ? "no clean turn" :
    `${row.adapter ?? "?"} ${row.providerSessionId ?? "no provider session id"} · ${row.runId}/${row.taskId}#${row.attempt}` +
    (row.resultPath ? ` · result ${row.resultPath}` : "");
  const turn = active
    ? ` · busy ${row.turnKind} ${row.turnRunId ?? "launching"}`
    : row.turnRunId !== null && (row.turnRunId !== row.runId || row.turnAttempt !== row.attempt)
      ? ` · last turn ${row.turnStatus} ${row.turnRunId}#${row.turnAttempt}` : "";
  return {
    name: row.name,
    adapter: row.adapter,
    provider_session_id: row.providerSessionId,
    run_id: row.runId,
    task_id: row.taskId,
    attempt: row.attempt,
    result_path: row.resultPath,
    active_run_id: active ? row.turnRunId : null,
    last_turn: { kind: row.turnKind, run_id: row.turnRunId, task_id: row.turnTaskId, attempt: row.turnAttempt, status: row.turnStatus },
    updated_by: row.updatedBy,
    updated_at: row.updatedAt,
    digest: `session ${row.name} ${pointer}${turn}`,
  };
}

function busy(name: string, row: NamedSession | undefined): Record<string, unknown> {
  const active = row?.turnStatus === null ? row.turnRunId : null;
  return {
    status: "rejected",
    error: "session_busy",
    session: name,
    active_run_id: active,
    fix: active
      ? `Wait for run ${active} with fabric_status, or cancel it, then retry.`
      : "Another turn of this session is starting or has just moved it; retry.",
  };
}

/**
 * One turn of a named session: start it when the name has no clean turn,
 * otherwise resume its provider session, or, only when `fresh` is passed, hand
 * its last result to a new session. The reply is the ordinary run row.
 */
export async function sessionDispatch(
  input: DispatchInput,
  identity: Identity,
  store: Store,
  signal: AbortSignal,
): Promise<Record<string, any>> {
  const { session, fresh, ...rest } = input;
  let name: string;
  try {
    name = sessionName(session);
  } catch (error) {
    return rejected(error);
  }
  const row = await reconcileSession(store, identity, name);
  if (row?.turnStatus === null) return busy(name, row);
  const prior = row?.runId ? row : undefined;
  let turn: SessionTurn = { kind: "start", attempt: 1 };
  if (prior && fresh) turn = { kind: "fresh", attempt: 1 };
  else if (prior) {
    const task = await taskRow(identity.cwd, prior.runId!, prior.taskId);
    const latest = task?.attempts?.at(-1);
    const unsupported = NO_NATIVE_CONTINUATION.has(prior.adapter ?? "")
      ? `${prior.adapter} has no native session continuation`
      : !prior.providerSessionId ? "its last turn recorded no provider session id"
      : latest && !latest.session_id ? "its latest attempt recorded no provider session id" : undefined;
    if (unsupported)
      return {
        status: "rejected",
        error: "continuation_unsupported",
        session: name,
        fix: `Session ${name}: ${unsupported}. Pass fresh: true to start a new session primed with its last result.`,
      };
    const attempts = (task?.attempts ?? []).map((attempt: Record<string, any>) => Number(attempt.attempt) || 0);
    turn = { kind: "resume", runId: prior.runId!, taskId: prior.taskId!, attempt: Math.max(0, ...attempts) + 1 };
  }
  const claimed = store.claimSessionTurn(identity, name, prior?.runId ?? null, turn, process.pid);
  if ("conflict" in claimed) return busy(name, claimed.conflict);
  let result: Record<string, any> | undefined;
  try {
    result = turn.kind === "resume"
      ? await resumeConfiguredProvider({ ...rest, resume: prior!.runId!, task_id: prior!.taskId! }, identity, signal)
      : turn.kind === "fresh"
        ? await handoffDispatch({ ...rest, handoff: prior!.runId!, task_id: prior!.taskId! }, identity, signal)
        : await dispatchConfiguredProvider(rest, identity, signal);
  } finally {
    const runId = result?.run_id ?? result?.id;
    if (typeof runId === "string" && result?.status !== "rejected") {
      store.bindSessionTurn(identity, name, claimed.claim, {
        runId,
        taskId: String(result!.task_id ?? turn.taskId),
        attempt: turn.kind === "resume" ? turn.attempt : 1,
      });
    } else if (result === undefined && turn.kind === "resume") {
      // Interrupted mid-launch: the run's own state decides whether the turn started.
      store.bindSessionTurn(identity, name, claimed.claim, { runId: turn.runId!, taskId: turn.taskId!, attempt: turn.attempt });
    } else store.settleSessionTurn(identity, name, claimed.claim, "rejected");
  }
  return { ...result, session: name, session_turn: turn.kind };
}
