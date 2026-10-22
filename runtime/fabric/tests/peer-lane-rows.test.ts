import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";

import { annotatePeerLaneRows } from "../src/peer-lane-rows.js";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

function git(cwd: string, ...args: string[]): string {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

it("annotates local lane paths from a linked worktree's main repository", () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-peer-lanes-"));
  roots.push(root);
  const repository = join(root, "main");
  const worktree = join(repository, ".worktrees", "peer");
  mkdirSync(repository, { recursive: true });
  git(repository, "init", "--quiet");
  git(repository, "config", "user.name", "Fabric test");
  git(repository, "config", "user.email", "fabric@example.invalid");
  git(repository, "commit", "--quiet", "--allow-empty", "-m", "fixture");
  mkdirSync(join(repository, ".worktrees"), { recursive: true });
  git(repository, "worktree", "add", "--quiet", "--detach", worktree, "HEAD");

  const existingRunDir = join(root, "existing-run");
  const rows = [
    { run_path: "runs/x", state: "running" },
    { source_project: "/source/project", run_path: "", state: "running" },
    { run_path: "runs/y", run_dir: existingRunDir, state: "running" },
    { run_path: "", state: "running" },
  ];
  annotatePeerLaneRows(rows, realpathSync(worktree));

  expect(rows[0]?.run_dir).toBe(join(realpathSync(repository), ".agent-run", "runs/x"));
  expect(rows[1]).not.toHaveProperty("run_dir");
  expect(rows[2]?.run_dir).toBe(existingRunDir);
  expect(rows[3]).not.toHaveProperty("run_dir");
});
