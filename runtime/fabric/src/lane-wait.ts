import type { Identity } from "./identity.js";
import type { RunRead, RunReadResponse } from "./run-reader.js";
import { isSQLiteContention, laneTaskId, type LaneAttempt, type Store } from "./store.js";

export interface LaneWaitDependencies {
  who: Identity;
  store: Pick<Store, "unseenLanes" | "markLanesSeen">;
  read: () => Promise<RunReadResponse>;
  /** Resolves once the text has been handed to the output, or rejects. */
  write: (text: string) => Promise<void>;
  fail: (text: string) => void;
  sleep: (ms: number) => Promise<void>;
  pollMs?: number;
  /** Hold the report until every listed lane is finished or needs input. */
  all?: boolean;
  /** Stop waiting after this long; the wait then exits 124. */
  timeoutSeconds?: number;
  now?: () => number;
}

const done = (row: RunRead) =>
  row.state === "terminal" || row.state === "input_required" || row.status === "input_required";

/**
 * Report every completed lane this seat has not been told about, or wait for
 * one. The cursor advances only after the report is written, so an
 * interrupted or failed write repeats a lane rather than losing it.
 */
export async function waitForLanes(deps: LaneWaitDependencies): Promise<number> {
  const { who, store, write, sleep } = deps;
  const now = deps.now ?? Date.now;
  const deadline = deps.timeoutSeconds === undefined ? Infinity : now() + deps.timeoutSeconds * 1000;
  let current = await deps.read();
  if (current.status !== "ok") {
    deps.fail(JSON.stringify(current));
    return 1;
  }
  for (;;) {
    const completed = current.runs.filter(done);
    const attempts: LaneAttempt[] = completed.map((row) => ({
      runId: row.run_id, taskId: laneTaskId(row), attempt: row.attempt,
    }));
    let unseen: number[] | undefined;
    try {
      unseen = store.unseenLanes(who, attempts);
    } catch (error) {
      // A busy mailbox delays the report to the next poll; it never ends the wait.
      if (!isSQLiteContention(error)) throw error;
    }
    const pending = current.runs.filter((row) => !done(row));
    const expired = now() >= deadline;
    const timeout = () => `timeout after ${deps.timeoutSeconds}s; still running: ${pending.map((row) => row.id).join(" ")}\n`;
    // With --all a finished lane waits for the others, unless time runs out.
    if (unseen?.length && (!deps.all || pending.length === 0 || expired)) {
      await write(unseen.map((index) => {
        const row = completed[index]!;
        return `${row.status ?? row.state}  ${row.id}  ${row.route ?? "-"}  ${row.result_path ?? "-"}\n`;
      }).join(""));
      if (expired && pending.length) await write(timeout());
      for (;;) {
        try {
          store.markLanesSeen(who, unseen.map((index) => attempts[index]!));
          return expired && pending.length ? 124 : 0;
        } catch (error) {
          if (!isSQLiteContention(error)) throw error;
          await sleep(deps.pollMs ?? 2000);
        }
      }
    }
    if (unseen !== undefined && !pending.length) {
      await write("no lanes are running\n");
      return 0;
    }
    if (expired) {
      await write(timeout());
      return 124;
    }
    await sleep(deps.pollMs ?? 2000);
    const next = await deps.read();
    if (next.status === "ok") current = next;
  }
}
