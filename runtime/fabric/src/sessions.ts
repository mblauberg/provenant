/**
 * Named provider sessions: a project-scoped alias over the existing run resume
 * path (ADR 0022). A turn is an ordinary dispatch, resume or handoff; this
 * module only chooses which, serialises turns per name and moves the alias
 * after a clean turn. Busy state is reconciled from run state on every read.
 */
import { resolve } from "node:path";

import { InputError, rejected, type DispatchInput } from "./execution-input.js";
import { dispatchConfiguredProvider, type LaunchObserver } from "./execution.js";
import type { Identity } from "./identity.js";
import { handoffDispatch, resumeConfiguredProvider } from "./resume.js";
import { runProcessAlive, statusRows } from "./run-registry.js";
import { canonicalSuccessStatus } from "./success-status.js";
import type { NamedSession, SessionLaunch, SessionTurnKind, Store } from "./store.js";

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

/**
 * The attempts one owner invocation made from `first`: the attempt it launched
 * and each fallback attempt it chained on after a retryable failure.
 */
function turnAttempts(task: Record<string, any>, first: number): Record<string, any>[] {
  const attempts = (task.attempts ?? []) as Record<string, any>[];
  const chain: Record<string, any>[] = [];
  let current = attempts.find((attempt) => Number(attempt.attempt) === first);
  while (current) {
    const number = Number(current.attempt);
    // The task row carries the registry's closure of an attempt whose owner died.
    chain.push(number === Number(task.attempt) ? task : current);
    current = attempts.find((attempt) =>
      Number(attempt.attempt) === number + 1 && Number(attempt.provenance?.fallback_from?.attempt) === number);
  }
  return chain;
}

/**
 * Settle a finished or abandoned turn from run state; return the current row.
 * Busy follows the run's lifecycle: a turn is open while its launcher is still
 * recording or starting it, then while its run's task is open or its owner lives.
 */
export async function reconcileSession(store: Store, who: Identity, name: string): Promise<NamedSession | undefined> {
  const row = store.session(who.project, name);
  if (!row || row.turnStatus !== null) return row;
  let status = "interrupted",
    runDir = "",
    final: Record<string, any> | undefined;
  if (alive(row.turnPid)) return row;
  // A dead launcher with no recorded run launched nothing.
  if (row.turnRunId !== null) {
    const task = await taskRow(who.cwd, row.turnRunId, row.turnTaskId);
    // The owner also lives between an attempt and the fallback it chains on, when every attempt reads terminal.
    if (task && (task.state !== "terminal" ||
      runProcessAlive(String(task.run_dir), String(task.task_id), Number(task.attempt)))) return row;
    runDir = String(task?.run_dir ?? "");
    const attempts = task ? turnAttempts(task, row.turnAttempt!) : [];
    final = task && (attempts.at(-1) ??
      // An owner that rejected the turn before its attempt existed.
      (Number(task.attempt) === row.turnAttempt ? task : undefined));
    if (final) status = final.error === "continuation_unsupported"
      ? "continuation_unsupported" : String(canonicalSuccessStatus(final.status) ?? "interrupted");
  }
  const clean = final && CLEAN.has(status) && typeof final.task_id === "string";
  store.settleSessionTurn(who, name, row.turnClaim, status, clean ? {
    adapter: final!.provenance?.transport ?? final!.provenance?.requested?.adapter ?? final!.adapter ?? null,
    providerSessionId: typeof final!.session_id === "string" && final!.session_id ? final!.session_id : null,
    runId: row.turnRunId!,
    taskId: final!.task_id,
    attempt: Number(final!.attempt),
    resultPath: typeof final!.result_path === "string" ? final!.result_path
      : typeof final!.paths?.result === "string" ? resolve(runDir, final!.paths.result) : null,
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

function continuationUnsupported(name: string, reason: string): Record<string, unknown> {
  return {
    status: "rejected",
    error: "continuation_unsupported",
    session: name,
    fix: `Session ${name}: ${reason}. Pass fresh: true to start a new session primed with its last clean result.`,
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
 * otherwise resume its provider session from the last clean attempt, or, only
 * when `fresh` is passed, hand that attempt's result to a new session. The
 * reply is the ordinary run row.
 */
export async function sessionDispatch(
  input: DispatchInput,
  identity: Identity,
  store: Store,
  signal: AbortSignal,
  observer: LaunchObserver = {},
): Promise<Record<string, any>> {
  const { federatedLane } = await import("./hosts.js");
  const remote = await federatedLane("dispatch", identity.cwd, input as Record<string, unknown>, signal);
  if (remote) return remote;
  const { session, fresh, ...rest } = input;
  let name: string;
  try {
    name = sessionName(session);
    // A council runs several provider sessions as a batch; a name holds one.
    if (rest.council !== undefined || rest.models !== undefined)
      throw new InputError("session_invalid", "A named session is one provider session; drop council and models, or dispatch them without session.");
  } catch (error) {
    return rejected(error);
  }
  const row = await reconcileSession(store, identity, name);
  if (row?.turnStatus === null) return busy(name, row);
  const prior = row?.runId ? row : undefined;
  const kind: SessionTurnKind = !prior ? "start" : fresh ? "fresh" : "resume";
  if (kind === "resume") {
    const unsupported = NO_NATIVE_CONTINUATION.has(prior!.adapter ?? "")
      ? `${prior!.adapter} has no native session continuation`
      : !prior!.providerSessionId ? "its last clean turn recorded no provider session id" : undefined;
    if (unsupported) return continuationUnsupported(name, unsupported);
  }
  const claimed = store.claimSessionTurn(identity, name, row?.turnClaim ?? null, kind, process.pid);
  if ("conflict" in claimed) return busy(name, claimed.conflict);
  let recorded = false,
    result: Record<string, any>;
  const pin = {
    attempt: prior?.attempt ?? undefined,
    // Record the run before its owner starts; if that fails, nothing launches.
    onLaunch: (launch: SessionLaunch) => {
      observer.onLaunch?.(launch);
      try {
        store.bindSessionTurn(identity, name, claimed.claim, launch);
      } catch (error) {
        throw new InputError("session_unrecorded", `Session ${name} could not record its run, so nothing was launched ` +
          `(${error instanceof Error ? error.message : String(error)}); retry.`);
      }
      recorded = true;
    },
  };
  try {
    const localEnv = { ...process.env, PROVENANT_HOST_LOCAL_ONLY: "1" };
    result = kind === "resume"
      ? await resumeConfiguredProvider({ ...rest, resume: prior!.runId!, task_id: prior!.taskId! }, identity, signal,
        localEnv, { ...pin, session: prior!.providerSessionId! })
      : kind === "fresh"
        ? await handoffDispatch({ ...rest, handoff: prior!.runId!, task_id: prior!.taskId! }, identity, signal, localEnv, pin)
        : await dispatchConfiguredProvider(rest, identity, signal, localEnv, pin);
  } finally {
    // A recorded turn is settled from its run; one that recorded nothing ends here.
    if (!recorded) store.settleSessionTurn(identity, name, claimed.claim, "rejected");
    else {
      try {
        store.releaseSessionLauncher(identity, name, claimed.claim);
      } catch {
        /* Fails closed: the name stays busy while this process lives. */
      }
    }
  }
  if (result.error === "session_unrecorded") return { ...result, session: name, session_turn: kind, session_error: result.fix };
  if (result.error === "continuation_unsupported")
    return { ...result, session: name, session_turn: kind, fix: continuationUnsupported(name, "the provider no longer has its session").fix };
  return { ...result, session: name, session_turn: kind };
}
