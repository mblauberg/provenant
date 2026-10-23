import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { createRequire } from "node:module";
import { afterAll, expect, it } from "vitest";
import { scratchRoot } from "./helpers/temp-root.js";

const root = realpathSync(scratchRoot("fabric-lanes-cli-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));
// Recent stamps: lanes hides terminal rows older than its window.
const hour = new Date(Date.now() - 2 * 3_600_000).toISOString().slice(0, 14);
const cli = resolve(import.meta.dirname, "../src/cli.ts");
const loader = createRequire(import.meta.url).resolve("tsx");

function lanes(cwd: string, ...args: string[]) {
  const result = spawnSync(process.execPath, ["--import", loader, cli, "lanes", ...args], {
    cwd, encoding: "utf8", timeout: 60_000,
    env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"), AGENT_FABRIC_LABEL: "lanes-cli" },
  });
  return { code: result.status, out: result.stdout, err: result.stderr };
}

function createAttempt(workspace: string, taskId: string, state: string, status: string | null, date: string, extra?: Record<string, any>) {
  const run = `20260924-1012-dispatch-fixture-${taskId.slice(-6).padStart(6, "a")}`;
  const dir = join(workspace, ".agent-run", "runs", run, "tasks", taskId, "attempt-001");
  mkdirSync(dir, { recursive: true });
  const row = JSON.parse(readFileSync(new URL("fixtures/attempt.json", import.meta.url), "utf8"));
  Object.assign(row, {
    run_id: `mcp-${taskId.slice(-6).padStart(6, "a")}`,
    task_id: taskId,
    state,
    status,
    started_at: date,
    ended_at: state === "terminal" ? date : null,
    paths: { result: `tasks/${taskId}/attempt-001/result.md` },
  });
  if (extra) Object.assign(row, extra);
  writeFileSync(join(dir, "attempt.json"), JSON.stringify(row));
  writeFileSync(join(dir, "result.md"), "result");
  return dir;
}

it("shows retained silence observations in lanes and their digest", async () => {
  const repo = join(root, "repo-watchdog");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  const dir = createAttempt(repo, "silent", "running", null, new Date().toISOString(), {
    paths: { watchdog: "dispatch/tasks/silent/attempt-001/watchdog.json" },
  });
  const observation = { schema: "fabric.watchdog.v1", checked_at: new Date().toISOString(),
    process_alive: true, silent_seconds: 301, last_event_at: null, last_worktree_write_at: null };
  const capture = join(dir, "../../../dispatch/tasks/silent/attempt-001");
  mkdirSync(capture, { recursive: true });
  writeFileSync(join(capture, "watchdog.json"), JSON.stringify(observation));
  const listing = lanes(repo);
  expect(listing.code).toBe(0);
  expect(listing.out).toContain("watchdog: silent 301s; alive");
  const json = JSON.parse(lanes(repo, "--json").out);
  expect(json.runs[0].watchdog).toMatchObject(observation);
  const { digest, lanesDigest } = await import("../src/surface.js");
  expect(digest(json.runs[0])).toContain("watchdog: silent 301s; alive");
  expect(lanesDigest(json)).toContain("watchdog: silent 301s; alive");
});

it("says which project an empty lane listing resolved to, and accepts --project", () => {
  const repo = join(root, "repo");
  const nested = join(repo, "sub");
  mkdirSync(nested, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  const bare = lanes(repo);
  expect(bare.code).toBe(0);
  expect(bare.out).toBe(`no lanes for project ${repo}\n`);
  const fromNested = lanes(nested);
  expect(fromNested.out).toContain(`no lanes for project ${repo}`);
  expect(fromNested.out).toContain(`cwd ${nested} resolves to it`);
  const elsewhere = lanes(root, "--project", repo);
  expect(elsewhere.code).toBe(0);
  expect(elsewhere.out).toBe(`no lanes for project ${repo}\n`);
});

it("rejects --timeout without --wait, and a bad timeout", () => {
  expect(lanes(root, "--timeout", "5").code).toBe(2);
  expect(lanes(root, "--wait", "--timeout", "abc").code).toBe(2);
  expect(lanes(root, "--wait", "--timeout", "-1").code).toBe(2);
  expect(lanes(root, "--wait", "--timeout", "0").code).toBe(0);
});

it("accepts --all without --wait", () => {
  const repo = join(root, "repo-all");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  const res = lanes(repo, "--all");
  expect(res.code).toBe(0);
  expect(res.out).toBe(`no lanes for project ${repo}\n`);
});

it("writes and parses the complete JSON listing when stdout is a pipe", () => {
  const repo = join(root, "repo-large-listing");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  for (let i = 1; i <= 300; i++) {
    createAttempt(repo, `large-${String(i).padStart(4, "0")}`, "terminal", "ok",
      `${hour}10:${String(i % 60).padStart(2, "0")}.000Z`);
  }

  const listing = lanes(repo, "--all", "--json");
  expect(listing.code).toBe(0);
  expect(Buffer.byteLength(listing.out)).toBeGreaterThan(65_536);
  expect(JSON.parse(listing.out).runs).toHaveLength(300);
});

it.each(["failed", "cancelled", "interrupted", "usage_limited", "auth_required"])(
  "returns 1 when a waited lane ends %s",
  (status) => {
    const repo = join(root, `repo-wait-${status}`);
    mkdirSync(repo, { recursive: true });
    spawnSync("git", ["init", "-q", repo]);
    createAttempt(repo, "waited", "terminal", status, `${hour}10:00.000Z`);

    const result = lanes(repo, "--wait", "--timeout", "0");
    expect(result.code).toBe(1);
    expect(result.out).toContain("waited");
  },
);

it("fails a later wait only when it names the already-reported failed lane", () => {
  const repo = join(root, "repo-wait-reported-failure");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  createAttempt(repo, "old-failure", "terminal", "failed", `${hour}10:00.000Z`);
  expect(lanes(repo, "--wait", "--timeout", "0").code).toBe(1);

  const bare = lanes(repo, "--wait", "--timeout", "0");
  expect(bare.code).toBe(0);
  expect(bare.out).toBe("no lanes are running\n");
  expect(lanes(repo, "--wait", "--timeout", "0", "old-failure").code).toBe(1);
});

it("keeps input-required lanes successful for --wait", () => {
  const repo = join(root, "repo-wait-input-required");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  createAttempt(repo, "needs-input", "input_required", "input_required", `${hour}10:00.000Z`);

  const result = lanes(repo, "--wait", "--timeout", "0");
  expect(result.code).toBe(0);
  expect(result.out).toContain("needs-input");
});

it("defaults to active lanes plus 5 most recent terminal ones, then N more; use --all", () => {
  const repo = join(root, "repo-lanes-budget");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);

  // Create 2 active lanes
  createAttempt(repo, "active-1", "running", "running", `${hour}01:00.000Z`);
  createAttempt(repo, "active-2", "running", "running", `${hour}02:00.000Z`);

  // Create 8 terminal lanes with ascending timestamps
  for (let i = 1; i <= 8; i++) {
    const minute = String(i).padStart(2, "0");
    createAttempt(repo, `term-${String(i).padStart(2, "0")}`, "terminal", "ok", `${hour}10:${minute}.000Z`);
  }

  const defaultListing = lanes(repo);
  expect(defaultListing.code).toBe(0);
  const defaultLines = defaultListing.out.trim().split("\n");
  // 2 active + 5 terminal + 1 "3 more; use --all"
  expect(defaultLines.length).toBe(8);
  expect(defaultListing.out).toContain("active-1");
  expect(defaultListing.out).toContain("active-2");
  // The 5 most recent terminal lanes are term-08, term-07, term-06, term-05, term-04
  expect(defaultListing.out).toContain("term-08");
  expect(defaultListing.out).toContain("term-07");
  expect(defaultListing.out).toContain("term-06");
  expect(defaultListing.out).toContain("term-05");
  expect(defaultListing.out).toContain("term-04");
  // The older ones should not be in the default output
  expect(defaultListing.out).not.toContain("term-03");
  expect(defaultListing.out).not.toContain("term-02");
  expect(defaultListing.out).not.toContain("term-01");
  expect(defaultLines[defaultLines.length - 1]).toBe("3 more; use --all");

  // With --all, all 10 lanes are listed and no omission line
  const allListing = lanes(repo, "--all");
  expect(allListing.code).toBe(0);
  const allLines = allListing.out.trim().split("\n");
  expect(allLines.length).toBe(10);
  expect(allListing.out).toContain("term-01");
  expect(allListing.out).toContain("term-02");
  expect(allListing.out).toContain("term-03");
  expect(allListing.out).not.toContain("more; use --all");
});

it("never truncates terminal lanes needing attention (quarantined, input_required) behind the 5-terminal window", () => {
  const repo = join(root, "repo-lanes-attention");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);

  // Create 6 terminal ok lanes with timestamps from 00:10:03 to 00:10:08
  for (let i = 3; i <= 8; i++) {
    const minute = String(i).padStart(2, "0");
    createAttempt(repo, `term-${minute}`, "terminal", "ok", `${hour}10:${minute}.000Z`);
  }
  // Create an older quarantined lane at 00:10:02
  createAttempt(repo, "term-quarantine", "terminal", "quarantined", `${hour}10:02.000Z`, {
    landable: false,
    quarantine: { reason: "protected_instructions_changed", patches: ["p.patch"] },
  });
  // Create an even older input_required lane at 00:10:01
  createAttempt(repo, "term-input-req", "terminal", "input_required", `${hour}10:01.000Z`);

  const defaultListing = lanes(repo);
  expect(defaultListing.code).toBe(0);
  const lines = defaultListing.out.trim().split("\n");
  // 5 most recent ok (term-08..term-04) + term-quarantine + term-input-req + "1 more; use --all" (term-03 is omitted)
  expect(defaultListing.out).toContain("term-quarantine");
  expect(defaultListing.out).toContain("not landable; review quarantine: protected_instructions_changed");
  expect(defaultListing.out).toContain("term-input-req");
  expect(defaultListing.out).not.toContain("term-03");
  expect(lines[lines.length - 1]).toBe("1 more; use --all");
});

it("lanes --wait --summary caps at 20 lines and leaves the omitted lane unacknowledged", () => {
  const repo = join(root, "repo-summary");
  mkdirSync(repo, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  const base = Date.now() - 600_000;
  for (let i = 1; i <= 21; i++) {
    createAttempt(repo, `sum-${String(i).padStart(2, "0")}`, "terminal", "ok", new Date(base + i * 1000).toISOString());
  }
  const first = lanes(repo, "--wait", "--summary", "--timeout", "0");
  expect(first.code).toBe(0);
  const lines = first.out.trim().split("\n");
  expect(lines.length).toBe(21);
  expect(lines[20]).toBe("+1 more; run lanes --wait again");
  const shown = lines.slice(0, 20).map((line) => /sum-\d\d/.exec(line)?.[0]);
  const second = lanes(repo, "--wait", "--summary", "--timeout", "0");
  expect(second.out.trim().split("\n").length).toBe(1);
  expect(shown).not.toContain(/sum-\d\d/.exec(second.out)?.[0]);
  expect(second.out).toMatch(/sum-\d\d/);
  expect(lanes(repo, "--summary").code).toBe(2);
});
