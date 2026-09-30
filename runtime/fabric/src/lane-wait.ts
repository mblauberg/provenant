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
    if (unseen?.length) {
      await write(unseen.map((index) => {
        const row = completed[index]!;
        return `${row.status ?? row.state}  ${row.id}  ${row.route ?? "-"}  ${row.result_path ?? "-"}\n`;
      }).join(""));
      for (;;) {
        try {
          store.markLanesSeen(who, unseen.map((index) => attempts[index]!));
          return 0;
        } catch (error) {
          if (!isSQLiteContention(error)) throw error;
          await sleep(deps.pollMs ?? 2000);
        }
      }
    }
    if (unseen !== undefined && !current.runs.some((row) => !done(row))) {
      await write("no lanes are running\n");
      return 0;
    }
    await sleep(deps.pollMs ?? 2000);
    const next = await deps.read();
    if (next.status === "ok") current = next;
  }
}
