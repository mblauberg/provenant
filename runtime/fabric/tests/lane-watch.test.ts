import { appendFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";

import { laneIdle } from "../src/lane-watch.js";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

function runs() {
  const root = mkdtempSync(join(tmpdir(), "fabric-lane-watch-"));
  roots.push(root);
  const dir = join(root, "runs");
  mkdirSync(join(dir, "20260101-0000-batch-user-abc", "dispatch/tasks/t/attempt-001"), { recursive: true });
  return dir;
}

it("wakes on a receipt write long before the fallback", async () => {
  const dir = runs();
  const watcher = laneIdle(() => [dir]);
  try {
    await watcher.idle(0); // establishes the watch
    setTimeout(() => writeFileSync(join(dir, "20260101-0000-batch-user-abc/dispatch/tasks/t/attempt-001/attempt.json"), "{}"), 50);
    const started = Date.now();
    await watcher.idle(20_000);
    expect(Date.now() - started).toBeLessThan(5_000);
  } finally { watcher.close(); }
});

it("ignores streaming logs and falls back to the timer", async () => {
  const dir = runs();
  const watcher = laneIdle(() => [dir]);
  try {
    const log = join(dir, "20260101-0000-batch-user-abc/stdout.jsonl");
    writeFileSync(log, "");
    await watcher.idle(0);
    await new Promise((done) => setTimeout(done, 500));
    await watcher.idle(0); // drains the event for the log's creation
    setTimeout(() => appendFileSync(log, "x"), 50);
    const started = Date.now();
    await watcher.idle(800);
    expect(Date.now() - started).toBeGreaterThanOrEqual(750);
  } finally { watcher.close(); }
});

it("tolerates a missing directory", async () => {
  const watcher = laneIdle(() => ["/nonexistent/fabric-lane-watch"]);
  await watcher.idle(10);
  watcher.close();
});
