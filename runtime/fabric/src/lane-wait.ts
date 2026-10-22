import type { Identity } from "./identity.js";
import type { RunRead, RunReadResponse } from "./run-reader.js";
import { isSQLiteContention, laneTaskId, type LaneAttempt, type Store } from "./store.js";
import { laneLine } from "./surface.js";

export interface LaneWaitDependencies {
  who: Identity;
  store: Pick<Store, "unseenLanes" | "markLanesSeen">;
  read: () => Promise<RunReadResponse>;
  /** Resolves once the text has been handed to the output, or rejects. */
  write: (text: string) => Promise<void>;
  fail: (text: string) => void;
  sleep: (ms: number) => Promise<void>;
  pollMs?: number;
  /**
   * Hold between reads until something may have changed or `ms` passes.
   * Absent, the wait sleeps a fixed `pollMs`.
   */
  idle?: (ms: number) => Promise<void>;
  /** Read everything, peers included; settles a decision made on a stale peer reading. */
  refresh?: () => Promise<RunReadResponse>;
  /** True while the last read reused earlier peer rows. */
  stale?: () => boolean;
  /** Longest idle between reads when `idle` is set. */
  fallbackMs?: number;
  /** Hold the report until every listed lane is finished or needs input. */
  all?: boolean;
  /** Stop waiting after this long; the wait then exits 124. */
  timeoutSeconds?: number;
  now?: () => number;
}

const done = (row: RunRead) =>
  row.reachability !== "unreachable" && (row.state === "terminal" || row.state === "input_required" || row.status === "input_required");

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
    // Neither "none running" nor a timeout may rest on peer rows from before the last minute.
    if (deps.stale?.() && deps.refresh && (now() >= deadline || current.runs.every(done))) {
      const fresh = await deps.refresh();
      if (fresh.status === "ok") current = fresh;
    }
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
    const timeout = () => `timeout after ${deps.timeoutSeconds}s; still running: ${pending.map((row) => row.id).join(" ")}${pending.some((row) => row.reachability === "unreachable") ? `; unreachable hosts: ${[...new Set(pending.filter((row) => row.reachability === "unreachable").map((row) => row.host))].join(" ")}` : ""}\n`;
    // With --all a finished lane waits for the others, unless time runs out.
    if (unseen?.length && (!deps.all || pending.length === 0 || expired)) {
      await write(unseen.map((index) => {
        const row = completed[index]!;
        return `${laneLine(row)}\n`;
      }).join(""));
      if (expired && pending.length) await write(timeout());
      for (let first = true; ; first = false) {
        // Every retry, including the one after a capped sleep, checks the clock before writing.
        if (!first && now() >= deadline) return 124;
        try {
          store.markLanesSeen(who, unseen.map((index) => attempts[index]!));
          return expired && pending.length ? 124 : 0;
        } catch (error) {
          if (!isSQLiteContention(error)) throw error;
          // Unmarked lanes are reported again next wait; contention must not outlast the deadline.
          const remaining = deadline - now();
          if (remaining <= 0) return 124;
          await sleep(Math.min(deps.pollMs ?? 2000, remaining));
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
    if (deps.idle) await deps.idle(Math.max(0, Math.min(deps.fallbackMs ?? 30_000, deadline - now())));
    else await sleep(deps.pollMs ?? 2000);
    const next = await deps.read();
    if (next.status === "ok") current = next;
  }
}
