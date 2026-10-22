import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import Database from "better-sqlite3";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { scratchRoot } from "./helpers/temp-root.js";

const repositoryRoot = resolve(import.meta.dirname, "../../..");
const cli = resolve(import.meta.dirname, "../src/cli.ts");
const loader = createRequire(import.meta.url).resolve("tsx");

let root: string, state: string, primary: string, linked: string, bare: string, base: string;

const git = (cwd: string, ...args: string[]) => execFileSync("git", args, { cwd, encoding: "utf8" }).trim();

beforeEach(() => {
  root = scratchRoot("fabric-landing-cli-");
  state = join(root, "state");
  primary = join(root, "primary");
  linked = join(root, "linked");
  bare = join(root, "remote.git");
  mkdirSync(primary);
  git(primary, "init", "-q", "-b", "main");
  git(primary, "config", "user.name", "Fabric Test");
  git(primary, "config", "user.email", "fabric-test@example.invalid");
  writeFileSync(join(primary, "file.txt"), "first\n");
  git(primary, "add", "file.txt");
  git(primary, "commit", "-qm", "first");
  git(root, "init", "-q", "--bare", "-b", "main", bare);
  git(primary, "remote", "add", "origin", bare);
  git(primary, "push", "-q", "origin", "HEAD:refs/heads/main");
  base = git(primary, "rev-parse", "HEAD");
  git(primary, "worktree", "add", "-q", "-b", "topic", linked);
});

afterEach(() => rmSync(root, { recursive: true, force: true }));

function fabric(cwd: string, label: string | undefined, ...args: string[]) {
  const env: NodeJS.ProcessEnv = { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: state, AGENT_FABRIC_SEAT: "codex",
    AGENT_FABRIC_PRODUCT_ROOT: repositoryRoot, NODE_NO_WARNINGS: "1" };
  delete env.AGENT_FABRIC_LABEL;
  if (label !== undefined) env.AGENT_FABRIC_LABEL = label;
  const result = spawnSync(process.execPath, ["--import", loader, cli, ...args], { cwd, encoding: "utf8", env, timeout: 60_000 });
  let json: any;
  try { json = JSON.parse(result.stdout); } catch { json = undefined; }
  return { code: result.status, out: result.stdout, err: result.stderr, json };
}

function database<T>(use: (db: Database.Database) => T): T {
  const db = new Database(join(state, "fabric.sqlite3"));
  try { return use(db); } finally { db.close(); }
}

const lapse = () => database((db) => db.prepare("UPDATE landing_leases SET expires_at = ?").run(Date.now() - 1));

describe("landing lease CLI", () => {
  it("refuses an acquire while another holder's lease is live, reporting holder and expiry", () => {
    const first = fabric(primary, "alice", "landing-acquire", "land", "--expected-sha", base, "--json");
    expect(first.code, first.err).toBe(0);
    expect(first.json).toMatchObject({ acquired: true, holder: "alice/land", label: "alice", session: "land",
      generation: 1, expected_sha: base });
    expect(Date.parse(first.json.expires_at)).toBeGreaterThan(Date.now());
    const second = fabric(primary, "bob", "landing-acquire", "land", "--expected-sha", base, "--json");
    expect(second.code).toBe(3);
    expect(second.json).toMatchObject({ acquired: false, reason: "held", holder: "alice/land", until: first.json.expires_at });
    const human = fabric(primary, "bob", "landing-acquire", "land", "--expected-sha", base);
    expect(human.code).toBe(3);
    expect(human.err).toContain("held by alice/land until");
    const status = fabric(primary, "bob", "landing-status", "--json");
    expect(status.json).toMatchObject({ lease: { holder: "alice/land", generation: 1 } });
  });

  it("records and reports a takeover of a lapsed lease", () => {
    expect(fabric(primary, "alice", "landing-acquire", "land", "--expected-sha", base).code).toBe(0);
    lapse();
    const takeover = fabric(primary, "bob", "landing-acquire", "train", "--branch", "main", "--json");
    expect(takeover.code, takeover.err).toBe(0);
    expect(takeover.json).toMatchObject({ holder: "bob/train", generation: 2, expected_sha: base, taken_over_from: "alice/land" });
    const human = fabric(primary, "carol", "landing-acquire", "x", "--expected-sha", base);
    expect(human.code).toBe(3);
    lapse();
    const line = fabric(primary, "carol", "landing-acquire", "x", "--expected-sha", base.slice(0, 8).toUpperCase());
    expect(line.code, line.err).toBe(0);
    expect(line.out).toMatch(/^acquired carol\/x generation 3 .*took over from bob\/train/u);
    const kinds = database((db) => db.prepare("SELECT kind FROM activity WHERE kind = 'landing_takeover'").all());
    expect(kinds).toHaveLength(2);
  });

  it("releases idempotently and never touches another holder's lease", () => {
    const lease = fabric(primary, "alice", "landing-acquire", "land", "--expected-sha", base, "--json").json;
    const other = fabric(primary, "bob", "landing-release", "land", String(lease.generation), "--json");
    expect(other.code).toBe(0);
    expect(other.json).toEqual({ released: false, reason: "not_held" });
    expect(fabric(primary, "bob", "landing-status", "--json").json.lease).toMatchObject({ holder: "alice/land" });

    database((db) => db.prepare("UPDATE landing_leases SET pushing_until = ?").run(Date.now() + 60_000));
    const pushing = fabric(primary, "alice", "landing-release", "land", String(lease.generation), "--json");
    expect(pushing.code).toBe(3);
    expect(pushing.json).toEqual({ released: false, reason: "pushing" });
    database((db) => db.prepare("UPDATE landing_leases SET pushing_until = NULL").run());

    const released = fabric(primary, "alice", "landing-release", "land", String(lease.generation), "--json");
    expect(released.code).toBe(0);
    expect(released.json).toEqual({ released: true, generation: lease.generation });
    const again = fabric(primary, "alice", "landing-release", "land", String(lease.generation));
    expect(again.code).toBe(0);
    expect(again.out.trim()).toBe("not released: not_held");

    const next = fabric(primary, "alice", "landing-acquire", "land", "--expected-sha", base, "--json").json;
    lapse();
    const expired = fabric(primary, "alice", "landing-release", "land", String(next.generation), "--json");
    expect(expired.code).toBe(0);
    expect(expired.json).toEqual({ released: false, reason: "expired" });
  });

  it("verifies the holder, the expected SHA and the remote SHA", () => {
    const lease = fabric(primary, "alice", "landing-acquire", "land", "--expected-sha", base, "--json").json;
    const ok = fabric(primary, "alice", "landing-verify", "land", "1", "--branch", "main", "--json");
    expect(ok.code, ok.err).toBe(0);
    expect(ok.json).toMatchObject({ valid: true, holder: "alice/land", generation: 1, expected_sha: base,
      expires_at: lease.expires_at });
    const stale = fabric(primary, "bob", "landing-verify", "land", "1", "--json");
    expect(stale.code).toBe(1);
    expect(stale.json).toMatchObject({ valid: false, reason: "stale" });
    writeFileSync(join(primary, "file.txt"), "second\n");
    git(primary, "commit", "-qam", "second");
    const second = git(primary, "rev-parse", "HEAD");
    const mismatch = fabric(primary, "alice", "landing-verify", "land", "1", "--expected-sha", second, "--json");
    expect(mismatch.code).toBe(1);
    expect(mismatch.json).toMatchObject({ valid: false, reason: "sha_mismatch", expected_sha: base });
    git(primary, "push", "-q", "origin", "HEAD:refs/heads/main");
    const moved = fabric(primary, "alice", "landing-verify", "land", "1", "--branch", "main", "--json");
    expect(moved.code).toBe(1);
    expect(moved.json).toMatchObject({ valid: false, reason: "sha_moved", expected_sha: base, remote_sha: second });
    const human = fabric(primary, "alice", "landing-verify", "land", "1", "--branch", "main");
    expect(human.code).toBe(1);
    expect(human.err).toContain("sha_moved");
  });

  it("resolves a linked worktree and the primary checkout to one project lease", () => {
    const acquired = fabric(linked, undefined, "landing-acquire", "land", "--branch", "main", "--json");
    expect(acquired.code, acquired.err).toBe(0);
    expect(acquired.json).toMatchObject({ holder: "codex/land", label: "codex" });
    const status = fabric(primary, "someone-else", "landing-status", "--json");
    expect(status.json).toMatchObject({ lease: { holder: "codex/land", generation: 1, expected_sha: base } });
    // A later call with a different ambient label names the acquire's label explicitly.
    const verify = fabric(primary, "someone-else", "landing-verify", "land", "1", "--label", acquired.json.label, "--json");
    expect(verify.code, verify.err).toBe(0);
    const release = fabric(primary, "someone-else", "landing-release", "land", "1", "--label", "codex", "--json");
    expect(release.json).toEqual({ released: true, generation: 1 });
    expect(fabric(linked, undefined, "landing-status", "--json").json).toEqual({ lease: null });
  });

  it("fails status loudly when the store cannot be opened", () => {
    writeFileSync(join(root, "not-a-directory"), "");
    const result = spawnSync(process.execPath, ["--import", loader, cli, "landing-status", "--json"], {
      cwd: primary, encoding: "utf8", timeout: 60_000,
      env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: join(root, "not-a-directory", "state"), NODE_NO_WARNINGS: "1" },
    });
    expect(result.status).not.toBe(0);
    expect(result.stdout).not.toContain("lease");
    expect(result.stderr).toContain("fabric:");
  });

  it("exits 2 on usage errors", () => {
    for (const args of [
      ["landing-acquire"],
      ["landing-acquire", "land"],
      ["landing-acquire", "land", "--expected-sha", base, "--branch", "main"],
      ["landing-acquire", "bad/session", "--expected-sha", base],
      ["landing-acquire", "land", "--expected-sha", "zzzz"],
      ["landing-acquire", "land", "--expected-sha", base, "--seconds", "0"],
      ["landing-acquire", "land", "--expected-sha", base, "--remote", "origin"],
      ["landing-verify", "land", "x"],
      ["landing-release", "land"],
      ["landing-release", "land", "1", "--bogus"],
      ["landing-status", "extra"],
      ["landing-push", "land", "1", "main", "--remote", "-bad"],
    ]) {
      const result = fabric(primary, "alice", ...args);
      expect(result.code, `${args.join(" ")}: ${result.err}`).toBe(2);
      expect(result.out).toBe("");
    }
  });

  it("pushes to a named remote with --remote", () => {
    git(primary, "remote", "rename", "origin", "upstream");
    const lease = fabric(linked, "alice", "landing-acquire", "land", "--branch", "main", "--remote", "upstream", "--json");
    expect(lease.code, lease.err).toBe(0);
    writeFileSync(join(linked, "file.txt"), "topic\n");
    git(linked, "commit", "-qam", "topic");
    const head = git(linked, "rev-parse", "HEAD");
    const noOrigin = fabric(linked, "alice", "landing-push", "land", "1", "main");
    expect(noOrigin.code).toBe(1);
    const pushed = fabric(linked, "alice", "landing-push", "land", "1", "main", "--remote", "upstream", "--json");
    expect(pushed.code, pushed.err).toBe(0);
    expect(pushed.json).toMatchObject({ pushed: "main", previous_sha: base, generation: 1 });
    expect(git(bare, "rev-parse", "refs/heads/main")).toBe(head);
    expect(fabric(primary, "alice", "landing-status", "--json").json).toEqual({ lease: null });
  });
});
