import { execFileSync, spawn, spawnSync } from "node:child_process";
import {
  chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import Database from "better-sqlite3";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { identify, withoutGitRedirects } from "../src/identity.js";
import { inspectDatabase, Store, type Message } from "../src/store.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

let temporaryDirectory: string;
let databasePath: string;
let openStores: Store[];

beforeEach(() => {
  temporaryDirectory = mkdtempSync(join(tmpdir(), "fabric-vitest-"));
  databasePath = join(temporaryDirectory, "fabric.sqlite3");
  const initialStore = new Store(databasePath);
  initialStore.close();
  openStores = [];
});

afterEach(() => {
  for (const store of openStores) {
    try {
      store.close();
    } catch {
      // A test may already have closed a store explicitly.
    }
  }
  rmSync(temporaryDirectory, { recursive: true, force: true });
});

function openStore(): Store {
  const store = new Store(databasePath);
  openStores.push(store);
  return store;
}

function agent(id: string) {
  return identify({ AGENT_FABRIC_SEAT: id }, repositoryRoot);
}

function announce(store: Store, ...ids: string[]): void {
  for (const id of ids) store.announce(agent(id));
}

describe("work ownership", () => {
  it("rejects overlapping issue and path claims across stores and fences an expired holder", () => {
    const first = openStore(), second = openStore();
    const alice = agent("alice"), bob = agent("bob");
    announce(first, "alice", "bob");
    const claim = first.acquireWork(alice, "session-a", { issue: "#869", paths: ["Runtime/Fabric"] }, 1);
    expect(claim).toMatchObject({ issue: "869", paths: ["runtime/fabric"] });
    expect(first.verifyWork(alice, "session-a", claim.id, claim.generation)).toMatchObject({ id: claim.id });
    expect(() => second.acquireWork(bob, "session-b", { issue: "869" }, 60)).toThrow(/claimed/);
    expect(() => second.acquireWork(alice, "another-session", { issue: "869" }, 60)).toThrow(/claimed/);
    expect(() => second.acquireWork(bob, "session-b", { paths: ["runtime/fabric/src"] }, 60)).toThrow(/claimed/);
    expect(() => second.acquireWork(bob, "session-b", { paths: ["RUNTIME/FABRIC/SRC"] }, 60)).toThrow(/claimed/);
    expect(second.workClaims(bob.project)).toMatchObject([{ holder: "alice/session-a", issue: "869" }]);
    const replacement = second.acquireWork(bob, "session-b", { paths: ["runtime/fabric/src"] }, 60, claim.expiresAtMs);
    expect(replacement.generation).toBeGreaterThan(claim.generation);
    expect(() => first.renewWork(alice, "session-a", claim.id, claim.generation, 60, claim.expiresAtMs)).toThrow(/stale/);
    expect(() => first.verifyWork(alice, "session-a", claim.id, claim.generation, claim.expiresAtMs)).toThrow(/stale/);
    expect(() => first.releaseWork(alice, "session-a", claim.id, claim.generation, claim.expiresAtMs)).toThrow(/stale/);
  });

  it("keeps sessions on one seat separate for work and landing", () => {
    const store = openStore(), sameSeat = agent("alice"), sha = "a".repeat(40);
    announce(store, "alice");
    const claim = store.acquireWork(sameSeat, "session-a", { issue: "869" }, 60);
    const lease = store.acquireLanding(sameSeat, "session-a", sha, 60);
    expect(() => store.acquireWork(sameSeat, "bad/session", { issue: "870" }, 60)).toThrow(/must not contain/);
    expect(() => store.renewWork(sameSeat, "session-b", claim.id, claim.generation, 60)).toThrow(/stale/);
    expect(() => store.verifyWork(sameSeat, "session-b", claim.id, claim.generation)).toThrow(/stale/);
    expect(() => store.releaseWork(sameSeat, "session-b", claim.id, claim.generation)).toThrow(/stale/);
    expect(() => store.withLandingPush(sameSeat, "session-b", lease.generation, sha, () => {})).toThrow(/stale/);
    expect(() => store.releaseLanding(sameSeat, "session-b", lease.generation)).toThrow(/stale/);
  });

  it("serialises landing, records stale takeover, and fences verify and release", () => {
    const first = openStore(), second = openStore();
    const alice = agent("alice"), bob = agent("bob");
    announce(first, "alice", "bob");
    const sha = "a".repeat(40);
    const lease = first.acquireLanding(alice, "session-a", sha, 1);
    expect(() => second.acquireLanding(bob, "session-b", sha, 60)).toThrow(/held/);
    expect(first.verifyLanding(alice, "session-a", lease.generation, sha)).toMatchObject({ holder: "alice/session-a" });
    const takeover = second.acquireLanding(bob, "session-b", sha, 60, lease.expiresAtMs);
    expect(takeover.generation).toBeGreaterThan(lease.generation);
    expect(takeover.takenOverFrom).toBe("alice/session-a");
    expect(() => first.verifyLanding(alice, "session-a", lease.generation, sha, lease.expiresAtMs)).toThrow(/stale/);
    expect(() => first.releaseLanding(alice, "session-a", lease.generation)).toThrow(/stale/);
    expect(() => second.verifyLanding(bob, "session-b", takeover.generation, "b".repeat(40))).toThrow(/SHA/);
    const renewed = second.renewLanding(bob, "session-b", takeover.generation, 60, takeover.expiresAtMs - 1);
    expect(renewed.expiresAtMs).toBeGreaterThan(takeover.expiresAtMs);
    expect(() => first.renewLanding(alice, "session-a", lease.generation, 60)).toThrow(/stale/);
    let pushed = false;
    second.withLandingPush(bob, "session-b", takeover.generation, sha, () => {
      first.note(alice, "concurrent Fabric write");
      expect(() => first.acquireLanding(alice, "session-c", sha, 60, renewed.expiresAtMs)).toThrow(/held/);
      expect(() => first.releaseLanding(bob, "session-b", takeover.generation)).toThrow(/stale/);
      expect(() => first.withLandingPush(bob, "session-b", takeover.generation, sha, () => {})).toThrow(/in progress/);
      pushed = true;
    });
    expect(pushed).toBe(true);
    expect(second.landingLease(bob.project)).toBeNull();
    expect(second.activity(bob.project).some((entry) => entry.kind === "landing_takeover")).toBe(true);
  });

  it("expires a crashed pusher's hold and records takeover", () => {
    const first = openStore(), second = openStore();
    const alice = agent("alice"), bob = agent("bob"), sha = "a".repeat(40);
    announce(first, "alice", "bob");
    const lease = first.acquireLanding(alice, "session-a", sha, 1);
    const db = new Database(databasePath);
    try {
      db.prepare("UPDATE landing_leases SET pushing_until = ? WHERE project = ?")
        .run(lease.expiresAtMs + 150_000, alice.project);
      expect(() => first.releaseLanding(alice, "session-a", lease.generation)).toThrow(/stale/);
      expect(() => second.acquireLanding(bob, "session-b", sha, 60, lease.expiresAtMs + 1)).toThrow(/held/);
      db.prepare("UPDATE landing_leases SET pushing_until = ? WHERE project = ?")
        .run(lease.expiresAtMs - 1, alice.project);
      const takeover = second.acquireLanding(bob, "session-b", sha, 60, lease.expiresAtMs + 1);
      expect(takeover).toMatchObject({ takenOverFrom: "alice/session-a", generation: lease.generation + 1 });
    } finally { db.close(); }
  });

  it("reports a completed push when lease release fails", () => {
    const store = openStore(), who = agent("alice"), sha = "a".repeat(40);
    announce(store, "alice");
    const lease = store.acquireLanding(who, "session-a", sha, 60);
    const db = new Database(databasePath);
    try {
      const outcome = store.withLandingPush(who, "session-a", lease.generation, sha, () => {
        db.exec(`CREATE TRIGGER fail_release BEFORE UPDATE OF released_at ON landing_leases
          BEGIN SELECT RAISE(ABORT, 'release blocked'); END`);
        return "pushed";
      });
      expect(outcome.result).toBe("pushed");
      expect(outcome.releaseWarning).toBeTruthy();
      expect(store.landingLease(who.project)).not.toBeNull();
    } finally { db.close(); }
  });

  it("exposes claims and landing through MCP and refuses a push without a lease", async () => {
    const serverPath = fileURLToPath(new URL("../src/server.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: ["--import", tsxLoader, serverPath],
      cwd: repositoryRoot,
      env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory,
        AGENT_FABRIC_SEAT: "claude", AGENT_FABRIC_LABEL: "chair-one", NODE_NO_WARNINGS: "1" },
    });
    const client = new Client({ name: "work-ownership-test", version: "1" });
    try {
      await client.connect(transport);
      const emptyStatus = await client.callTool({ name: "fabric_status", arguments: {} });
      expect(emptyStatus.isError).not.toBe(true);
      expect(JSON.stringify(emptyStatus)).not.toContain('"work_claims"');
      expect(JSON.stringify(emptyStatus)).not.toContain('"landing_lease"');
      const claim = await client.callTool({ name: "fabric_work_claim", arguments: {
        action: "acquire", session_id: "claude-session-one", issue: "869", seconds: 60,
      } });
      expect(claim.isError).not.toBe(true);
      const claimRecord = claim.structuredContent as { id: string; generation: number };
      const verified = await client.callTool({ name: "fabric_work_claim", arguments: {
        action: "verify", session_id: "claude-session-one", id: claimRecord.id, generation: claimRecord.generation,
      } });
      expect(verified.structuredContent).toMatchObject({ id: claimRecord.id });
      const lease = await client.callTool({ name: "fabric_landing_lease", arguments: {
        action: "acquire", session_id: "claude-session-one", expected_sha: "a".repeat(40), seconds: 60,
      } });
      expect(lease.isError).not.toBe(true);
      const status = await client.callTool({ name: "fabric_status", arguments: { detail: "full" } });
      expect(status.structuredContent).toMatchObject({
        work_claims: [{ holder: "chair-one/claude-session-one", issue: "869" }],
        landing_lease: { holder: "chair-one/claude-session-one", expectedSha: "a".repeat(40) },
      });
      const brief = await client.callTool({ name: "fabric_status", arguments: {} });
      expect(brief.content).toHaveLength(1);
      const push = runCli(["landing-push", "other-session", "1", "main"]);
      expect(push.status).toBe(1);
      expect(push.stderr).toMatch(/lease/u);
    } finally {
      await client.close().catch(() => undefined);
    }
  });

  it("pushes the leased head from the cwd repository despite Git redirects", () => {
    const work = join(temporaryDirectory, "landing-work"), bare = join(temporaryDirectory, "landing-remote.git");
    mkdirSync(work);
    const git = (cwd: string, ...args: string[]) => execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
    git(work, "init", "-b", "main");
    git(work, "config", "user.name", "Fabric Test");
    git(work, "config", "user.email", "fabric-test@example.invalid");
    writeFileSync(join(work, "claim.txt"), "first\n");
    git(work, "add", "claim.txt");
    git(work, "commit", "-m", "first");
    git(temporaryDirectory, "init", "--bare", "-b", "main", bare);
    git(work, "remote", "add", "origin", bare);
    git(work, "push", "origin", "HEAD:refs/heads/main");
    const base = git(work, "rev-parse", "HEAD");
    writeFileSync(join(work, "claim.txt"), "second\n");
    git(work, "commit", "-am", "second");
    const head = git(work, "rev-parse", "HEAD");
    const store = openStore();
    const who = identify({ AGENT_FABRIC_SEAT: "codex", AGENT_FABRIC_LABEL: "cli-reviewer" }, work);
    store.announce(who);
    const lease = store.acquireLanding(who, "landing-session", base, 60);
    const pushed = runCli(["landing-push", "landing-session", String(lease.generation), "main", "--label", "cli-reviewer"],
      temporaryDirectory, { GIT_DIR: bare, AGENT_FABRIC_SEAT: "agent", AGENT_FABRIC_LABEL: "other-seat", GIT_NAMESPACE: "shadow" }, work);
    expect(pushed.status, pushed.stderr).toBe(0);
    expect(git(bare, "rev-parse", "refs/heads/main")).toBe(head);
    expect(store.landingLease(who.project)).toBeNull();
  });

  it("removes Git configuration redirects from landing commands", () => {
    const env = withoutGitRedirects({ GIT_CONFIG_PARAMETERS: "'url.bad.pushInsteadOf=origin'",
      GIT_CONFIG_COUNT: "1", GIT_CONFIG_KEY_0: "url.bad.pushInsteadOf", GIT_CONFIG_VALUE_0: "origin",
      GIT_CONFIG_GLOBAL: "/tmp/bogus", GIT_NAMESPACE: "shadow" });
    expect(Object.keys(env).filter((key) => key.startsWith("GIT_"))).toEqual([]);
  });
});

function runCli(
  args: string[],
  stateDirectory = temporaryDirectory,
  identityEnv: Record<string, string> = {},
  cwd = repositoryRoot,
) {
  const cliPath = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
  const tsxLoader = createRequire(import.meta.url).resolve("tsx");
  return spawnSync(process.execPath, ["--import", tsxLoader, cliPath, ...args], {
    cwd,
    encoding: "utf8",
    env: {
      ...process.env,
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "cli-reviewer",
      NODE_NO_WARNINGS: "1",
      ...identityEnv,
    },
  });
}

function statSnapshot(path: string) {
  const stat = statSync(path);
  return { mode: stat.mode, size: stat.size, mtimeMs: stat.mtimeMs, ino: stat.ino };
}

describe("CLI boundaries", () => {
  it("summarises unread messages by task and sender within a bounded digest", () => {
    const store = openStore();
    announce(store, "chair", "worker");
    for (let index = 0; index < 30; index += 1) {
      store.send(agent("worker"), "chair", `update ${index} ${"x".repeat(2000)}`);
    }
    const task = store.createTask(agent("worker"), "Review the result");
    store.send(agent("worker"), "chair", "Task update", { taskId: task.taskId });
    const digest = store.inboxDigest(agent("chair"));
    expect(digest.groups).toEqual(expect.arrayContaining([
      expect.objectContaining({ from: "worker", taskId: null }),
      expect.objectContaining({ from: "worker", taskId: task.taskId }),
    ]));
    expect(digest.groups.find((group) => group.taskId === task.taskId)?.count).toBeGreaterThan(0);
    expect(store.inbox(agent("chair"), { ids: [store.inbox(agent("chair"), { peek: true, limit: 1 })[0]!.messageId] })).toHaveLength(1);
  });
  it("accepts a task-filtered digest and pairs its sample ID with its summary", () => {
    const store = openStore();
    announce(store, "chair", "worker");
    const task = store.createTask(agent("worker"), "Review a task");
    const ids = [0, 1].map(() => store.send(agent("worker"), "chair", "placeholder", { taskId: task.taskId }).messageId).sort();
    const db = new Database(databasePath);
    db.prepare("UPDATE messages SET body = ? WHERE message_id = ?").run("Zebra update", ids[0]);
    db.prepare("UPDATE messages SET body = ? WHERE message_id = ?").run("Apple update", ids[1]);
    db.close();
    store.send(agent("worker"), "chair", "Unrelated update");
    const result = runCli(["inbox", "--digest", "--task-id", task.taskId], temporaryDirectory,
      { AGENT_FABRIC_SEAT: "chair", AGENT_FABRIC_LABEL: "chair" });
    expect(result.status, result.stderr).toBe(0);
    const digest = JSON.parse(result.stdout);
    expect(digest.groups).toEqual(expect.arrayContaining([
      expect.objectContaining({ taskId: task.taskId, sampleId: ids[0], summary: "Zebra update" }),
    ]));
  });
  it("rejects an unknown command before creating or announcing", () => {
    const stateDirectory = join(temporaryDirectory, "unknown-command-state");
    const result = runCli(["frobnicate"], stateDirectory);

    expect(result.status).toBe(2);
    expect(result.stderr).toBeTruthy();
    expect(result.stderr).not.toContain("src/cli.ts");
    expect(existsSync(stateDirectory)).toBe(false);
  });

  it("reports expected store errors without a TypeScript stack", () => {
    const result = runCli(["send", "missing-agent", "hello"]);

    expect(result.status).toBe(1);
    expect(result.stderr).toBeTruthy();
    expect(result.stderr).not.toContain("src/store.ts");
  });

  it("reports a client-seat collision without a startup stack", () => {
    const store = openStore();
    store.announce(identify({
      AGENT_FABRIC_SEAT: "claude",
      AGENT_FABRIC_LABEL: "shared-label",
    }, repositoryRoot));
    store.close();

    const result = runCli(["whoami"], temporaryDirectory, {
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "shared-label",
    });

    expect(result.status).toBe(1);
    expect(result.stderr).toBeTruthy();
    expect(result.stderr).not.toContain("src/store.ts");
    expect(result.stderr).not.toContain("at Store.announce");
    expect(result.stdout).toBe("");
  });

  it("rejects empty task and output links at the CLI boundary", () => {
    const store = openStore();
    announce(store, "sender");

    for (const args of [
      ["send", "sender", "empty task", "--task-id", ""],
      ["send", "sender", "empty path", "--output-path", ""],
      ["inbox", "--task-id", ""],
    ]) {
      const result = runCli(args);
      expect(result.status, result.stderr).toBe(1);
      expect(result.stderr).toBeTruthy();
      expect(result.stderr).not.toContain("src/cli.ts");
    }
  });

  it("reports status and doctor results without creating or updating the database", () => {
    for (const command of ["status", "doctor"]) {
      const absentState = join(temporaryDirectory, `${command}-absent`);
      const absent = runCli([command, "--json"], absentState);
      expect(absent.status, absent.stderr).toBe(0);
      expect(JSON.parse(absent.stdout)).toMatchObject({ status: "absent", exists: false });
      expect(existsSync(absentState)).toBe(false);
    }

    const store = openStore();
    const alice = agent("alice");
    store.announce(alice);
    store.note(alice, "seed diagnostic state");
    store.close();
    const before = readFileSync(databasePath);
    chmodSync(databasePath, 0o444);
    chmodSync(temporaryDirectory, 0o555);
    const beforeEntries = readdirSync(temporaryDirectory).sort();
    const beforeDatabaseStat = statSnapshot(databasePath);
    const beforeDirectoryStat = statSnapshot(temporaryDirectory);
    let status: ReturnType<typeof runCli>;
    let doctor: ReturnType<typeof runCli>;
    let afterEntries: string[];
    let afterDatabaseStat: ReturnType<typeof statSnapshot>;
    let afterDirectoryStat: ReturnType<typeof statSnapshot>;
    try {
      status = runCli(["status", "--json"]);
      doctor = runCli(["doctor"]);
      afterEntries = readdirSync(temporaryDirectory).sort();
      afterDatabaseStat = statSnapshot(databasePath);
      afterDirectoryStat = statSnapshot(temporaryDirectory);
    } finally {
      chmodSync(temporaryDirectory, 0o700);
      chmodSync(databasePath, 0o600);
    }

    expect(status.status, `stderr=${status.stderr}\nstdout=${status.stdout}`).toBe(0);
    const statusResult = JSON.parse(status.stdout);
    expect(statusResult).toMatchObject({ status: "ok", exists: true, readOnly: true, project: alice.project });
    expect(statusResult.counts.agents).toBeGreaterThan(0);
    expect(statusResult.counts.activity).toBeGreaterThan(0);
    expect(doctor.status, `stderr=${doctor.stderr}\nstdout=${doctor.stdout}`).toBe(0);
    expect(JSON.parse(doctor.stdout)).toMatchObject({
      status: "ok", exists: true, readOnly: true,
      checks: expect.arrayContaining([
        expect.objectContaining({ name: "schema", ok: true }),
        expect.objectContaining({ name: "integrity", ok: true }),
      ]),
    });
    expect(readFileSync(databasePath)).toEqual(before);
    expect(afterEntries).toEqual(beforeEntries);
    expect(afterDatabaseStat).toEqual(beforeDatabaseStat);
    expect(afterDirectoryStat).toEqual(beforeDirectoryStat);
  });

  it("reports inaccessible diagnostic state as an error rather than healthy absence", () => {
    const store = openStore();
    store.close();

    chmodSync(temporaryDirectory, 0o000);
    let result: ReturnType<typeof runCli>;
    try {
      result = runCli(["doctor", "--json"]);
    } finally {
      chmodSync(temporaryDirectory, 0o700);
    }

    expect(result.status).toBe(1);
    expect(JSON.parse(result.stdout)).toMatchObject({
      status: "error",
      exists: true,
      readOnly: true,
    });
  });

  it("diagnoses an incomplete delivery-claim migration", () => {
    const database = new Database(databasePath);
    database.exec(`
      DROP TABLE delivery_claims;
      CREATE TABLE delivery_claims (
        message_id TEXT NOT NULL,
        recipient_id TEXT NOT NULL,
        claim_id TEXT NOT NULL,
        PRIMARY KEY (message_id, recipient_id)
      );
    `);
    database.close();

    const diagnostic = inspectDatabase(databasePath, agent("alice").project, "doctor");
    expect(diagnostic).toMatchObject({
      status: "error",
      checks: expect.arrayContaining([expect.objectContaining({
        name: "delivery-claim-schema",
        ok: false,
        detail: expect.stringMatching(/project|claimed_at|expires_at/),
      })]),
    });
  });

  it("diagnoses foreign-key violations", () => {
    const database = new Database(databasePath);
    database.pragma("foreign_keys = OFF");
    database.prepare(
      `INSERT INTO deliveries(message_id, project, recipient_id, read_at)
       VALUES ('orphan-message', ?, 'bob', NULL)`,
    ).run(agent("alice").project);
    database.close();

    const diagnostic = inspectDatabase(databasePath, agent("alice").project, "doctor");
    expect(diagnostic).toMatchObject({
      status: "error",
      checks: expect.arrayContaining([expect.objectContaining({
        name: "foreign-keys",
        ok: false,
        detail: expect.stringContaining("deliveries"),
      })]),
    });
  });

  it("reads a coherent active-WAL snapshot without mutating source state", () => {
    const writer = openStore();
    const alice = agent("alice");
    writer.announce(alice);
    writer.note(alice, "committed only through active WAL");
    const sourcePaths = [databasePath, `${databasePath}-wal`, `${databasePath}-shm`];
    expect(sourcePaths.every(existsSync)).toBe(true);
    const before = sourcePaths.map((path) => ({
      path,
      bytes: readFileSync(path),
      stat: statSnapshot(path),
    }));

    expect(inspectDatabase(databasePath, alice.project, "doctor")).toMatchObject({
      status: "ok",
      counts: { agents: 1, activity: 1 },
    });
    for (const snapshot of before) {
      expect(readFileSync(snapshot.path)).toEqual(snapshot.bytes);
      expect(statSnapshot(snapshot.path)).toEqual(snapshot.stat);
    }
  });

  it("claims and acknowledges a delivery through the CLI", () => {
    const store = openStore();
    const sender = agent("sender");
    const recipient = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "cli-reviewer",
    }, repositoryRoot);
    store.announce(sender);
    store.announce(recipient);
    const sent = store.send(sender, recipient.agentId, "CLI delivery");
    store.close();

    const inbox = runCli(["inbox", "--claim-seconds", "60"]);
    expect(inbox.status, inbox.stderr).toBe(0);
    const claim = (JSON.parse(inbox.stdout) as Message[])[0]!;
    expect(claim).toMatchObject({
      messageId: sent.messageId,
      claimId: expect.any(String),
      claimExpiresAt: expect.any(String),
    });
    const acknowledgement = runCli(["ack", claim.messageId, claim.claimId!]);
    expect(acknowledgement.status, acknowledgement.stderr).toBe(0);
    expect(JSON.parse(acknowledgement.stdout)).toMatchObject({
      messageId: sent.messageId,
      alreadyAcknowledged: false,
    });
    expect(runCli(["inbox", "--peek"]).stdout.trim()).toBe("[]");
  });

  it("requires whole claim seconds within the documented CLI range", () => {
    for (const value of ["0.5", "0", "3601"]) {
      const result = runCli(["inbox", "--claim-seconds", value]);
      expect(result.status).toBe(1);
      expect(result.stderr).toBeTruthy();
      expect(result.stderr).not.toContain("src/cli.ts");
    }
  });

  it("forwards a positive inbox limit and rejects invalid limits", () => {

    const store = openStore();
    const sender = agent("limit-sender");
    const recipient = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "cli-reviewer",
    }, repositoryRoot);
    store.announce(sender);
    store.announce(recipient);
    for (const body of ["first", "second", "third"]) {
      store.send(sender, recipient.agentId, body);
    }
    store.close();

    const limited = runCli(["inbox", "--peek", "--limit", "2"]);
    expect(limited.status, limited.stderr).toBe(0);
    expect(JSON.parse(limited.stdout)).toHaveLength(2);

    for (const value of ["0", "1.5", "-1"]) {
      const result = runCli(["inbox", "--limit", value]);
      expect(result.status).toBe(1);
      expect(result.stderr).toBeTruthy();
      expect(result.stderr).not.toContain("src/cli.ts");
    }
  });

  it("rejects extra fixed-shape CLI arguments before mutation", () => {
    const store = openStore();
    const cliAgent = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "cli-reviewer",
    }, repositoryRoot);
    store.announce(cliAgent);
    store.createTask(cliAgent, "must stay open", { taskId: "extra-args" });
    store.close();

    for (const invocation of [
      ["whoami", "unexpected"],
      ["tasks", "open", "unexpected"],
      ["done", "extra-args", "unexpected"],
    ]) {
      const result = runCli(invocation);
      expect(result.status).toBe(1);
    expect(result.stderr).toBeTruthy();
    }
    expect(openStore().tasks(cliAgent.project)).toMatchObject([{
      taskId: "extra-args",
      state: "open",
    }]);
  });

  it("keeps activity watch live after its initial 200-row window", async () => {
    const seed = openStore();
    const watcher = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "cli-reviewer",
    }, repositoryRoot);
    seed.announce(watcher);
    for (let index = 0; index < 200; index += 1) seed.note(watcher, `before-${index}`);
    seed.close();

    const cliPath = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const child = spawn(process.execPath, [
      "--import", tsxLoader, cliPath, "watch", "--activity", "--interval", "0.02",
    ], {
      cwd: repositoryRoot,
      env: {
        ...process.env,
        AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory,
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "cli-reviewer",
        NODE_NO_WARNINGS: "1",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let injected = false;
    const observed = await new Promise<boolean>((done) => {
      let matched = false;
      const timeout = setTimeout(() => {
        child.kill("SIGTERM");
      }, 5_000);
      child.once("close", () => {
        clearTimeout(timeout);
        done(matched);
      });
      child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
      child.stdout.on("data", (chunk: Buffer | string) => {
        stdout += chunk.toString();
        if (!injected && stdout.includes("before-199")) {
          injected = true;
          const writer = openStore();
          for (let index = 0; index < 250; index += 1) writer.note(watcher, `after-${index}`);
          writer.close();
        }
        if (stdout.includes("after-249")) {
          matched = true;
          child.kill("SIGTERM");
        }
      });
    });
    expect(observed, `stdout=${stdout}\nstderr=${stderr}`).toBe(true);
  }, 10_000);
});

describe("MCP startup boundaries", () => {
  it("announces a connected seat before any coordination tool is called", async () => {
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: ["--import", createRequire(import.meta.url).resolve("tsx"),
        fileURLToPath(new URL("../src/server.ts", import.meta.url))],
      cwd: repositoryRoot,
      stderr: "pipe",
      env: { HOME: process.env.HOME ?? temporaryDirectory, PATH: process.env.PATH ?? "/usr/bin:/bin",
        AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory, AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "connected-seat", NODE_NO_WARNINGS: "1" },
    });
    const client = new Client({ name: "presence-regression", version: "1" });
    try {
      await client.connect(transport);
      expect(openStore().agents(agent("observer").project)).toEqual(expect.arrayContaining([
        expect.objectContaining({ agentId: "connected-seat", provider: "codex" }),
      ]));
    } finally { await client.close(); }
  });

  it("closes an active wait when stdin reaches EOF", async () => {
    const serverPath = fileURLToPath(new URL("../src/server.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const child = spawn(process.execPath, ["--import", tsxLoader, serverPath], {
      cwd: repositoryRoot,
      env: {
        ...process.env,
        AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory,
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "eof-recipient",
        NODE_NO_WARNINGS: "1",
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer | string) => { stdout += chunk.toString(); });
    child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
    const closed = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>(
      (resolveClosed) => {
        child.once("close", (code, signal) => resolveClosed({ code, signal }));
      },
    );
    child.stdin.write([
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2025-06-18",
          capabilities: {},
          clientInfo: { name: "eof-regression", version: "1" },
        },
      }),
      JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "tools/call",
        params: { name: "fabric_inbox", arguments: { claim: true, wait_seconds: 2 } },
      }),
      "",
    ].join("\n"));
    for (let attempt = 0; attempt < 150 && !stdout.includes('"id":1'); attempt += 1) {
      await delay(10);
    }
    const initializedBeforeEof = stdout.includes('"id":1');
    const waitPendingBeforeEof = !stdout.includes('"id":2');
    child.stdin.end();

    const exit = await Promise.race([
      closed,
      delay(800).then(() => null),
    ]);
    if (exit === null) child.kill("SIGTERM");
    const finalExit = await closed;
    expect(exit, stderr).not.toBeNull();
    expect(finalExit).toEqual({ code: 0, signal: null });
    expect(initializedBeforeEof).toBe(true);
    expect(waitPendingBeforeEof).toBe(true);
    expect(stdout).toContain('"id":1');
  }, 4_000);

  it("waits inside one inbox call until a message arrives", async () => {
    const sender = agent("wait-sender");
    const seed = openStore();
    seed.announce(sender);
    seed.createTask(sender, "wait for one task", { taskId: "wait-task" });
    seed.close();

    const serverPath = fileURLToPath(new URL("../src/server.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: ["--import", tsxLoader, serverPath],
      cwd: repositoryRoot,
      stderr: "pipe",
      env: {
        HOME: process.env.HOME ?? temporaryDirectory,
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        NODE_NO_WARNINGS: "1",
        AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory,
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "wait-recipient",
      },
    });
    const client = new Client({ name: "wait-regression", version: "1" });
    try {
      await client.connect(transport);
      let settled = false;
      const waiting = client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true, wait_seconds: 1, task_id: "wait-task" },
      }).finally(() => { settled = true; });

      await delay(75);
      expect(settled).toBe(false);
      const writer = openStore();
      writer.send(sender, "wait-recipient", "arrived during the MCP wait", { taskId: "wait-task" });
      writer.close();

      const result = await waiting;
      expect(result.isError).toBeUndefined();
      const content = result.content as Array<{ type: "text"; text: string }>;
      expect(JSON.parse(content[0]!.text).messages).toMatchObject([{
        body: "arrived during the MCP wait",
        claimId: expect.any(String),
      }]);
      const messages = JSON.parse(content[0]!.text).messages as Message[];
      await client.callTool({
        name: "fabric_acknowledge",
        arguments: {
          message_id: messages[0]!.messageId,
          claim_id: messages[0]!.claimId,
        },
      });

      const timeoutStarted = Date.now();
      const timedOut = await client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true, wait_seconds: 1 },
      });
      expect(Date.now() - timeoutStarted).toBeGreaterThanOrEqual(900);
      const timeoutContent = timedOut.content as Array<{ type: "text"; text: string }>;
      expect(JSON.parse(timeoutContent[0]!.text).messages).toEqual([]);

      const controller = new AbortController();
      const abandoned = client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true, wait_seconds: 2 },
      }, undefined, { signal: controller.signal });
      await delay(75);
      controller.abort();
      await expect(abandoned).rejects.toThrow();

      const writerAfterCancellation = openStore();
      writerAfterCancellation.send(sender, "wait-recipient", "arrived after cancellation");
      writerAfterCancellation.close();
      await delay(150);

      const afterCancellation = await client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true,},
      });
      const afterCancellationContent = afterCancellation.content as Array<{
        type: "text"; text: string;
      }>;
      const afterCancellationMessages = JSON.parse(
        afterCancellationContent[0]!.text,
      ).messages as Message[];
      expect(afterCancellationMessages).toMatchObject([{
        body: "arrived after cancellation",
        claimId: expect.any(String),
      }]);
      await client.callTool({
        name: "fabric_acknowledge",
        arguments: {
          message_id: afterCancellationMessages[0]!.messageId,
          claim_id: afterCancellationMessages[0]!.claimId,
        },
      });

      const queuedWriter = openStore();
      queuedWriter.send(sender, "wait-recipient", "queued before locked cancellation");
      queuedWriter.close();
      const cancellationBlocker = new Database(databasePath);
      cancellationBlocker.exec("BEGIN IMMEDIATE");
      const lockedController = new AbortController();
      const lockedCancellation = client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true, wait_seconds: 2 },
      }, undefined, { signal: lockedController.signal });
      const lockedRejection = expect(lockedCancellation).rejects.toThrow();
      await delay(25);
      lockedController.abort();
      await delay(5);
      cancellationBlocker.exec("ROLLBACK");
      cancellationBlocker.close();
      await lockedRejection;
      await delay(100);

      const afterLockedCancellation = await client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true,},
      });
      const afterLockedContent = afterLockedCancellation.content as Array<{
        type: "text"; text: string;
      }>;
      const afterLockedMessages = JSON.parse(afterLockedContent[0]!.text).messages as Message[];
      expect(afterLockedMessages).toMatchObject([{
        body: "queued before locked cancellation",
        claimId: expect.any(String),
      }]);
      await client.callTool({
        name: "fabric_acknowledge",
        arguments: {
          message_id: afterLockedMessages[0]!.messageId,
          claim_id: afterLockedMessages[0]!.claimId,
        },
      });

      const blocker = new Database(databasePath);
      blocker.exec("BEGIN IMMEDIATE");
      try {
        const lockedStarted = performance.now();
        const locked = await client.callTool({
          name: "fabric_inbox",
          arguments: { claim: true, wait_seconds: 1 },
        }, undefined, { timeout: 2_000 });
        expect(performance.now() - lockedStarted).toBeLessThan(1_400);
        const lockedContent = locked.content as Array<{ type: "text"; text: string }>;
        expect(JSON.parse(lockedContent[0]!.text).messages).toEqual([]);
      } finally {
        blocker.exec("ROLLBACK");
        blocker.close();
      }
    } finally {
      await client.close().catch(() => undefined);
    }
  }, 10_000);

  it("keeps the transport open with a stable tool error after a client-seat collision", async () => {
    const store = openStore();
    store.announce(identify({
      AGENT_FABRIC_SEAT: "claude",
      AGENT_FABRIC_LABEL: "shared-label",
    }, repositoryRoot));
    store.close();

    const serverPath = fileURLToPath(new URL("../src/server.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: ["--import", tsxLoader, serverPath],
      cwd: repositoryRoot,
      stderr: "pipe",
      env: {
        HOME: process.env.HOME ?? temporaryDirectory,
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        NODE_NO_WARNINGS: "1",
        AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory,
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "shared-label",
      },
    });
    let stderr = "";
    transport.stderr?.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
    const client = new Client({ name: "collision-regression", version: "1" });
    try {
      await client.connect(transport);
      for (let attempt = 0; attempt < 2; attempt += 1) {
        const result = await client.callTool({ name: "fabric_whoami", arguments: {} });
        expect(result.isError).toBe(true);
        expect(result.content).toMatchObject([{
          type: "text",
          text: expect.any(String),
        }]);
      }
      expect(stderr).not.toContain("src/store.ts");
      expect(stderr).not.toContain("at Store.announce");
    } finally {
      await client.close().catch(() => undefined);
    }
  });

  it("recovers lazily after a transient startup lock is released", async () => {
    const seed = openStore();
    const sender = agent("lock-sender");
    const recipient = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "lock-recovery",
    }, repositoryRoot);
    seed.announce(sender);
    seed.announce(recipient);
    seed.send(sender, recipient.agentId, "recover claim and acknowledgement");
    seed.close();
    const blocker = new Database(databasePath);
    blocker.exec("BEGIN IMMEDIATE");
    let lockHeld = true;

    const serverPath = fileURLToPath(new URL("../src/server.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: ["--import", tsxLoader, serverPath],
      cwd: repositoryRoot,
      stderr: "pipe",
      env: {
        HOME: process.env.HOME ?? temporaryDirectory,
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        NODE_NO_WARNINGS: "1",
        AGENT_FABRIC_STATE_DIRECTORY: temporaryDirectory,
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "lock-recovery",
      },
    });
    const client = new Client({ name: "lock-recovery", version: "1" });
    try {
      await client.connect(transport);
      const boundedStarted = performance.now();
      const bounded = await client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true, wait_seconds: 1 },
      });
      expect(performance.now() - boundedStarted).toBeLessThan(1_400);
      expect(bounded.isError).toBeUndefined();
      const boundedContent = bounded.content as Array<{ type: "text"; text: string }>;
      expect(JSON.parse(boundedContent[0]!.text).messages).toEqual([]);

      const locked = await client.callTool({ name: "fabric_whoami", arguments: {} });
      expect(locked.isError).toBe(true);
      expect(locked.content).toMatchObject([{
        type: "text",
        text: expect.any(String),
      }]);

      const peekedPromise = client.callTool({
        name: "fabric_inbox",
        arguments: { claim: true, peek: true, wait_seconds: 2 },
      });
      await delay(100);
      blocker.exec("COMMIT");
      lockHeld = false;
      const peeked = await peekedPromise;
      expect(peeked.isError).toBeUndefined();

      const writeBlocker = new Database(databasePath);
      writeBlocker.exec("BEGIN IMMEDIATE");
      const notePromise = client.callTool({
        name: "fabric_note",
        arguments: { detail: "write after lazy peek startup" },
      });
      await delay(100);
      writeBlocker.exec("ROLLBACK");
      writeBlocker.close();
      const noted = await notePromise;
      expect(noted.isError).toBeUndefined();

      const recovered = await client.callTool({ name: "fabric_whoami", arguments: {} });
      expect(recovered.isError).toBeUndefined();
      expect(recovered.content).toMatchObject([{
        type: "text",
        text: expect.stringContaining('"agentId":"lock-recovery"'),
      }]);
      const inbox = await client.callTool({ name: "fabric_inbox", arguments: { claim: true,} });
      expect(inbox.isError).toBeUndefined();
      const inboxContent = inbox.content as Array<{ type: "text"; text: string }>;
      const messages = JSON.parse(inboxContent[0]!.text).messages as Message[];
      expect(messages).toMatchObject([{
        body: "recover claim and acknowledgement",
        claimId: expect.any(String),
      }]);
      const acknowledgement = await client.callTool({
        name: "fabric_acknowledge",
        arguments: {
          message_id: messages[0]!.messageId,
          claim_id: messages[0]!.claimId,
        },
      });
      expect(acknowledgement.isError).toBeUndefined();
    } finally {
      if (lockHeld) blocker.exec("ROLLBACK");
      blocker.close();
      await client.close().catch(() => undefined);
    }
  }, 20_000);
});

describe("identity derivation", () => {
  it("creates a new state directory for its single user only", () => {
    const stateDirectory = join(temporaryDirectory, "new-private-state");
    const store = new Store(join(stateDirectory, "fabric.sqlite3"));
    store.close();

    expect(statSync(stateDirectory).mode & 0o777).toBe(0o700);
  });

  it("uses seat for provider and label for agent id, with client-label fallback", () => {
    expect(identify({
      AGENT_FABRIC_SEAT: "seat",
      AGENT_FABRIC_CLIENT_LABEL: "client",
      AGENT_FABRIC_LABEL: "label",
    }, repositoryRoot)).toMatchObject({ provider: "seat", agentId: "label" });

    expect(identify({ AGENT_FABRIC_CLIENT_LABEL: "client" }, repositoryRoot))
      .toMatchObject({ provider: "client", agentId: "client" });
    expect(identify({}, repositoryRoot)).toMatchObject({ provider: "agent", agentId: "agent" });
  });

  it("rejects reusing one project label from a different client seat", () => {
    const store = openStore();
    const claude = identify({
      AGENT_FABRIC_SEAT: "claude",
      AGENT_FABRIC_LABEL: "reviewer",
    }, repositoryRoot);
    const codex = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "reviewer",
    }, repositoryRoot);

    store.announce(claude);
    expect(() => store.announce(codex)).toThrowError(
      /agent label reviewer .* already belongs to client seat claude/,
    );
    expect(store.agents(claude.project)).toMatchObject([
      { agentId: "reviewer", provider: "claude" },
    ]);
  });

  it("reserves all for broadcast instead of an agent or team recipient", () => {
    const store = openStore();
    const alice = agent("alice");
    store.announce(alice);

    expect(() => store.announce(agent("all"))).toThrowError(
      /recipient id "all" is reserved for broadcast/,
    );
    expect(() => store.createTeam(alice, "all", ["alice"])).toThrowError(
      /recipient id "all" is reserved for broadcast/,
    );
    expect(() => store.createTeam(alice, "reviewers", ["all"])).toThrowError(
      /recipient id "all" is reserved for broadcast/,
    );
  });
});

describe("messaging", () => {
  it("sends to an agent, a team, and all other announced agents", () => {
    const store = openStore();
    announce(store, "alice", "bob", "carol");
    const alice = agent("alice");

    expect(store.send(alice, "bob", "direct").recipients).toEqual(["bob"]);
    store.createTeam(alice, "reviewers", ["alice", "bob", "carol"]);
    expect(store.send(alice, "reviewers", "team").recipients).toEqual(["bob", "carol"]);
    expect(store.send(alice, "all", "broadcast").recipients.sort()).toEqual(["bob", "carol"]);
  });

  it("atomically replaces an existing team's membership", () => {
    const store = openStore();
    announce(store, "alice", "bob", "carol");
    const alice = agent("alice");

    store.createTeam(alice, "reviewers", ["alice", "bob", "carol"]);
    expect(store.createTeam(alice, "reviewers", ["alice", "bob", "bob"]))
      .toEqual({ teamId: "reviewers", members: ["alice", "bob"] });
    expect(store.send(alice, "reviewers", "replacement membership").recipients).toEqual(["bob"]);
  });

  it("keeps a legacy team/agent collision usable and diagnoses its routing ambiguity", () => {
    const store = openStore();
    announce(store, "alice", "bob", "carol", "dave");
    const alice = agent("alice");
    store.close();
    const legacy = new Database(databasePath);
    legacy.prepare(`INSERT INTO teams(project, team_id, created_at) VALUES (?, ?, ?)`).run(
      alice.project, "bob", Date.now(),
    );
    legacy.prepare(
      `INSERT INTO team_members(project, team_id, agent_id) VALUES (?, ?, ?)`,
    ).run(alice.project, "bob", "carol");
    legacy.close();

    const migrated = openStore();
    expect(() => migrated.announce(agent("bob"))).not.toThrow();
    expect(migrated.createTeam(alice, "bob", ["alice", "dave"])).toEqual({
      teamId: "bob",
      members: ["alice", "dave"],
    });
    expect(migrated.send(alice, "bob", "legacy team-first routing").recipients).toEqual(["dave"]);

    const diagnostic = inspectDatabase(databasePath, alice.project, "doctor");
    expect(diagnostic).toMatchObject({
      status: "error",
      checks: expect.arrayContaining([expect.objectContaining({
        name: "recipient-namespace",
        ok: false,
        detail: expect.stringContaining("bob"),
      })]),
    });
  });

  it("rejects a new agent label already reserved by a team", () => {
    const store = openStore();
    announce(store, "alice", "carol");
    const alice = agent("alice");
    store.createTeam(alice, "future-agent", ["carol"]);

    expect(() => store.announce(agent("future-agent"))).toThrowError(
      /agent label future-agent collides with an existing team id/,
    );
    expect(store.agents(alice.project).map((entry) => entry.agentId)).not.toContain("future-agent");
    expect(store.send(alice, "future-agent", "team remains unambiguous").recipients)
      .toEqual(["carol"]);
  });

  it("rejects an unknown recipient and names the known agents", () => {
    const store = openStore();
    announce(store, "alice", "bob", "carol");

    let error: unknown;
    try {
      store.send(agent("alice"), "ghost", "this must not be delivered");
    } catch (candidate) {
      error = candidate;
    }

    expect(error).toBeInstanceOf(Error);
    const message = (error as Error).message;
    expect(message).toBeTruthy();
    for (const known of ["alice", "bob", "carol"]) expect(message).toContain(known);
  });

  it("claims inbox messages until their claim is explicitly acknowledged", () => {
    const store = openStore();
    announce(store, "alice", "bob", "carol");
    const alice = agent("alice");
    const bob = agent("bob");
    const carol = agent("carol");

    store.send(alice, "bob", "peekable");
    const peeked = store.inbox(bob, { peek: true });
    expect(peeked).toMatchObject([{ body: "peekable", claimId: null }]);
    const claimed = store.inbox(bob);
    expect(claimed).toMatchObject([{
      body: "peekable",
      claimId: expect.any(String),
      claimExpiresAt: expect.any(String),
    }]);
    expect(store.inbox(bob)).toEqual([]);
    const acknowledged = store.acknowledge(bob, claimed[0]!.messageId, claimed[0]!.claimId!);
    expect(acknowledged).toMatchObject({
      messageId: claimed[0]!.messageId,
      acknowledgedAt: expect.any(String),
      alreadyAcknowledged: false,
    });
    expect(store.acknowledge(bob, claimed[0]!.messageId, claimed[0]!.claimId!))
      .toMatchObject({ alreadyAcknowledged: true });
    expect(store.inbox(bob, { peek: true })).toEqual([]);

    store.send(alice, "all", "independent read");
    const bobBroadcast = store.inbox(bob);
    expect(bobBroadcast.map((message) => message.body)).toEqual(["independent read"]);
    expect(store.inbox(bob)).toEqual([]);
    const carolBroadcast = store.inbox(carol);
    expect(carolBroadcast.map((message) => message.body)).toEqual(["independent read"]);
    store.acknowledge(bob, bobBroadcast[0]!.messageId, bobBroadcast[0]!.claimId!);
    store.acknowledge(carol, carolBroadcast[0]!.messageId, carolBroadcast[0]!.claimId!);
  });

  it("redelivers an expired unacknowledged claim and fences the stale claimant", async () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");

    store.send(alice, "bob", "survive transport failure");
    const first = store.inbox(bob, { claimTtlMs: 50 })[0]!;
    expect(store.inbox(bob)).toEqual([]);
    await delay(75);

    const redelivered = store.inbox(bob)[0]!;
    expect(redelivered.messageId).toBe(first.messageId);
    expect(redelivered.claimId).not.toBe(first.claimId);
    expect(() => store.acknowledge(bob, first.messageId, first.claimId!)).toThrowError(
      /does not own delivery/,
    );
    expect(store.acknowledge(bob, redelivered.messageId, redelivered.claimId!))
      .toMatchObject({ alreadyAcknowledged: false });
  });

  it("adds claim storage without changing legacy messages or delivery state", () => {
    const legacy = openStore();
    announce(legacy, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");
    const sent = legacy.send(alice, "bob", "preserve me");
    legacy.close();

    const database = new Database(databasePath);
    database.exec("DROP TABLE delivery_claims");
    const before = database.prepare(
      "SELECT message_id, project, recipient_id, read_at FROM deliveries",
    ).all();
    database.close();

    const migrated = openStore();
    const verifier = new Database(databasePath, { readonly: true });
    const after = verifier.prepare(
      "SELECT message_id, project, recipient_id, read_at FROM deliveries",
    ).all();
    verifier.close();
    expect(after).toEqual(before);
    const claim = migrated.inbox(bob)[0]!;
    expect(claim.messageId).toBe(sent.messageId);
    migrated.acknowledge(bob, claim.messageId, claim.claimId!);
  });

  it("threads replies and rejects missing or cross-project parents", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");

    const parent = store.send(alice, "bob", "question");
    const parentMessage = store.inbox(bob)[0];
    expect(parentMessage?.messageId).toBe(parent.messageId);

    const reply = store.send(bob, "alice", "answer", { replyTo: parent.messageId });
    expect(() => store.send(bob, "alice", "orphan answer", {
      replyTo: "message-that-does-not-exist",
    })).toThrowError(/reply parent .* does not exist in project/u);

    const otherProject = identify({ AGENT_FABRIC_SEAT: "mallory" }, temporaryDirectory);
    const otherRecipient = identify({
      AGENT_FABRIC_SEAT: "other-recipient",
    }, temporaryDirectory);
    store.announce(otherProject);
    store.announce(otherRecipient);
    const foreignParent = store.send(otherProject, otherRecipient.agentId, "foreign question");
    expect(() => store.send(bob, "alice", "cross-project answer", {
      replyTo: foreignParent.messageId,
    })).toThrowError(/reply parent .* does not exist in project/u);

    const replies = store.inbox(alice);
    const threaded = replies.find((message) => message.messageId === reply.messageId);

    expect(threaded).toMatchObject({
      conversationId: parent.messageId,
      replyTo: parent.messageId,
    });
    expect(replies).toHaveLength(1);
  });

  it("links messages to an existing task and leaves an output path opaque", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");
    const task = store.createTask(alice, "review the result", { taskId: "review" });
    const sent = store.send(alice, "bob", "artifact ready", {
      taskId: task.taskId,
      outputPath: "/path/that/does/not/exist",
    });
    const message = store.inbox(bob)[0]!;
    expect(message).toMatchObject({
      messageId: sent.messageId,
      taskId: "review",
      outputPath: "/path/that/does/not/exist",
    });
    expect(Object.keys(message)).toContain("outputPath");
  });

  it("rejects empty task ids and output paths for direct Store callers", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");
    expect(() => store.createTask(alice, "invalid", { taskId: "" }))
      .toThrowError();
    store.createTask(alice, "valid", { taskId: "valid" });
    expect(() => store.send(alice, "bob", "invalid task", { taskId: "" }))
      .toThrowError();
    expect(() => store.send(alice, "bob", "invalid path", { outputPath: "" }))
      .toThrowError();
    expect(() => store.inbox(bob, { taskId: "" }))
      .toThrowError();
  });

  it("validates task links atomically within the sender project", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    store.createTask(alice, "same project task", { taskId: "local-task" });
    const foreign = identify({ AGENT_FABRIC_SEAT: "foreign" }, temporaryDirectory);
    const foreignRecipient = identify({ AGENT_FABRIC_SEAT: "foreign-recipient" }, temporaryDirectory);
    store.announce(foreign);
    store.announce(foreignRecipient);
    store.createTask(foreign, "foreign task", { taskId: "foreign-task" });

    const beforeDatabase = new Database(databasePath, { readonly: true });
    const before = beforeDatabase.prepare("SELECT count(*) AS count FROM messages")
      .get() as { count: number };
    beforeDatabase.close();
    expect(() => store.send(alice, "bob", "missing task", { taskId: "missing-task" }))
      .toThrowError(/task missing-task does not exist in project/u);
    expect(() => store.send(alice, "bob", "foreign task", { taskId: "foreign-task" }))
      .toThrowError(/task foreign-task does not exist in project/u);
    const afterDatabase = new Database(databasePath, { readonly: true });
    const after = afterDatabase.prepare("SELECT count(*) AS count FROM messages").get() as { count: number };
    const deliveries = afterDatabase.prepare("SELECT count(*) AS count FROM deliveries").get() as { count: number };
    afterDatabase.close();
    expect(after.count).toBe(before.count);
    expect(deliveries.count).toBe(0);
  });

  it("migrates legacy messages and omits absent optional links", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const sent = store.send(agent("alice"), "bob", "legacy shape");
    store.close();
    const legacy = new Database(databasePath);
    legacy.exec("ALTER TABLE messages DROP COLUMN task_id; ALTER TABLE messages DROP COLUMN output_path;");
    legacy.close();

    const migrated = openStore();
    const message = migrated.inbox(agent("bob"))[0]!;
    expect(message.messageId).toBe(sent.messageId);
    expect(message).not.toHaveProperty("taskId");
    expect(message).not.toHaveProperty("outputPath");
  });

  it("filters peek and claims without disturbing other pending deliveries", async () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");
    store.createTask(alice, "first", { taskId: "first" });
    store.createTask(alice, "second", { taskId: "second" });
    store.send(alice, "bob", "first result", { taskId: "first" });
    store.send(alice, "bob", "second result", { taskId: "second" });
    store.send(alice, "bob", "unrelated result");

    expect(store.inbox(bob, { peek: true, taskId: "first" })).toMatchObject([{
      body: "first result", claimId: null, taskId: "first",
    }]);
    const first = store.inbox(bob, { taskId: "first", claimTtlMs: 50 })[0]!;
    expect(first.body).toBe("first result");
    expect(store.inbox(bob, { taskId: "second" })).toMatchObject([{ body: "second result" }]);
    expect(store.inbox(bob, { taskId: "first" })).toEqual([]);
    expect(store.inbox(bob)).toMatchObject([{ body: "unrelated result" }]);
    await delay(75);
    expect(store.inbox(bob, { taskId: "first" })).toMatchObject([{ body: "first result" }]);
  });
});

describe("tasks", () => {
  it("creates, updates, and round-trips dependencies", () => {
    const store = openStore();
    const alice = agent("alice");
    announce(store, "alice");

    const created = store.createTask(alice, "ship the fix", {
      taskId: "child",
      owner: "alice",
      dependsOn: ["parent", "design"],
    });
    expect(created).toMatchObject({
      taskId: "child",
      objective: "ship the fix",
      owner: "alice",
      state: "open",
      dependsOn: ["parent", "design"],
    });
    const roundTripped = store.tasks(alice.project);
    expect(roundTripped).toHaveLength(1);
    expect(roundTripped[0]).toMatchObject({
      taskId: created.taskId,
      objective: created.objective,
      owner: created.owner,
      state: created.state,
    });
    expect(roundTripped[0]?.dependsOn.slice().sort()).toEqual(["design", "parent"]);

    const updated = store.updateTask(alice, "child", "done", "shipped");
    expect(updated).toMatchObject({ taskId: "child", state: "done" });
    expect(updated.dependsOn.slice().sort()).toEqual(["design", "parent"]);
    expect(() => store.updateTask(alice, "missing", "done")).toThrowError();
  });

  it("atomically assigns an open unowned task to exactly one claimant", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");
    store.createTask(alice, "own this once", { taskId: "claim-once" });

    expect(store.claimTask(alice, "claim-once")).toMatchObject({
      taskId: "claim-once",
      owner: "alice",
      state: "open",
    });
    expect(store.claimTask(alice, "claim-once")).toMatchObject({
      taskId: "claim-once",
      owner: "alice",
      state: "open",
    });
    expect(() => store.claimTask(bob, "claim-once")).toThrowError(
      /task claim-once is already assigned to alice/,
    );
    expect(store.tasks(alice.project)).toMatchObject([{ owner: "alice" }]);
    expect(store.activity(alice.project).some((entry) => entry.kind === "task_claim")).toBe(true);
  });

  it("reserves claimed state for the atomic ownership operation", () => {
    const store = openStore();
    const alice = agent("alice");
    store.announce(alice);
    store.createTask(alice, "claim through ownership", { taskId: "reserved-claimed" });

    expect(() => store.updateTask(alice, "reserved-claimed", "claimed"))
      .toThrowError(/state claimed is reserved for atomic task claim/u);
    expect(store.tasks(alice.project)).toMatchObject([{
      taskId: "reserved-claimed",
      state: "open",
      owner: null,
    }]);
    expect(store.claimTask(alice, "reserved-claimed")).toMatchObject({ owner: "alice" });
  });
});

describe("activity", () => {
  it("records message, team, create-task, and update-task events in stable order", () => {
    const store = openStore();
    const alice = agent("alice");
    announce(store, "alice", "bob");

    store.send(alice, "bob", "hello");
    store.createTeam(alice, "reviewers", ["alice", "bob"]);
    const task = store.createTask(alice, "review the change", { taskId: "review" });
    store.updateTask(alice, task.taskId, "done");

    const entries = store.activity(alice.project).reverse();
    expect(entries.map((entry) => entry.kind)).toEqual(["send", "team", "task", "task"]);
    expect(entries.every((entry) => entry.agentId === "alice")).toBe(true);
    const timestamps = entries.map((entry) => Date.parse(entry.at));
    expect(timestamps.every((at, index) => index === 0 || at >= timestamps[index - 1]!)).toBe(true);
  });

  it("paginates forward by monotonic sequence beyond a 200-row window", () => {
    const store = openStore();
    const alice = agent("alice");
    store.announce(alice);
    for (let index = 0; index < 450; index += 1) store.note(alice, `event-${index}`);

    const first = store.activityAfter(alice.project, 0, 200);
    const second = store.activityAfter(alice.project, first.at(-1)!.seq, 200);
    const third = store.activityAfter(alice.project, second.at(-1)!.seq, 200);
    const all = [...first, ...second, ...third];

    expect(all).toHaveLength(450);
    expect(all.map((entry) => entry.detail)).toEqual(
      Array.from({ length: 450 }, (_, index) => `event-${index}`),
    );
    expect(all.every((entry, index) => index === 0 || entry.seq > all[index - 1]!.seq)).toBe(true);
  });
});

describe("multi-process WAL concurrency", () => {
  it("opens and migrates absent databases across five 16-process cold starts", async () => {
    const workerPath = fileURLToPath(new URL("./cold-start-worker.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    for (let round = 0; round < 2; round += 1) {
      const coldDatabasePath = join(temporaryDirectory, `cold-start-${round}.sqlite3`);
      const startAt = Date.now() + 1_000;
      const results = await Promise.all(Array.from({ length: 8 }, (_, index) =>
        new Promise<{ code: number | null; stdout: string; stderr: string }>((done) => {
          const child = spawn(process.execPath, [
            "--import", tsxLoader, workerPath, coldDatabasePath, repositoryRoot,
            String(index), String(startAt),
          ], {
            cwd: repositoryRoot,
            env: { ...process.env, NODE_NO_WARNINGS: "1" },
            stdio: ["ignore", "pipe", "pipe"],
          });
          let stdout = "";
          let stderr = "";
          child.stdout.on("data", (chunk: Buffer | string) => { stdout += chunk.toString(); });
          child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
          child.once("close", (code) => done({ code, stdout, stderr }));
        }),
      ));
      const outcomes = results.map((result) => JSON.parse(result.stdout.trim()) as {
        index: number;
        ok: boolean;
        error?: string;
      });
      expect(results.every((result) => result.code === 0), results.map((r) => r.stderr).join("\n"))
        .toBe(true);
      expect(outcomes.filter((outcome) => !outcome.ok), JSON.stringify({ round, outcomes }))
        .toEqual([]);

      const verifier = new Store(coldDatabasePath);
      expect(verifier.agents(agent("alice").project)).toHaveLength(8);
      verifier.close();
    }
  }, 60_000);

  it("lets exactly one simultaneous agent or team reserve a recipient label", async () => {
    const bootstrap = openStore();
    bootstrap.announce(identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "namespace-creator",
    }, repositoryRoot));
    bootstrap.close();

    const workerPath = fileURLToPath(new URL("./namespace-worker.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const startAt = Date.now() + 1_000;
    const results = await Promise.all(["team", "agent"].map((role) =>
      new Promise<{ code: number | null; stdout: string; stderr: string }>((done) => {
        const child = spawn(process.execPath, [
          "--import", tsxLoader, workerPath, databasePath, repositoryRoot,
          role, "contested-recipient", String(startAt),
        ], {
          cwd: repositoryRoot,
          env: { ...process.env, NODE_NO_WARNINGS: "1" },
          stdio: ["ignore", "pipe", "pipe"],
        });
        let stdout = "";
        let stderr = "";
        child.stdout.on("data", (chunk: Buffer | string) => { stdout += chunk.toString(); });
        child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
        child.once("close", (code) => done({ code, stdout, stderr }));
      }),
    ));
    expect(results.every((result) => result.code === 0), results.map((r) => r.stderr).join("\n"))
      .toBe(true);
    const outcomes = results.map((result) => JSON.parse(result.stdout.trim()) as {
      role: string;
      won: boolean;
      error?: string;
    });
    expect(outcomes.filter((outcome) => outcome.won)).toHaveLength(1);
    expect(outcomes.filter((outcome) => !outcome.won)[0]?.error).toMatch(/collides/);
  }, 15_000);

  it("starts claim expiry after acquiring the write lock", async () => {
    const bootstrap = openStore();
    const sender = agent("sender");
    const recipient = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "contended-reader",
    }, repositoryRoot);
    bootstrap.announce(sender);
    bootstrap.announce(recipient);
    bootstrap.send(sender, recipient.agentId, "wait for the lock");
    bootstrap.close();

    const workerPath = fileURLToPath(new URL("./claim-contention-worker.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const child = spawn(process.execPath, [
      "--import", tsxLoader, workerPath, databasePath, repositoryRoot, "1000",
    ], {
      cwd: repositoryRoot,
      env: { ...process.env, NODE_NO_WARNINGS: "1" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    await new Promise<void>((ready, reject) => {
      child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
      child.stdout.on("data", (chunk: Buffer | string) => {
        stdout += chunk.toString();
        if (stdout.includes("ready\n")) ready();
      });
      child.once("error", reject);
    });

    const blocker = new Database(databasePath);
    blocker.exec("BEGIN IMMEDIATE");
    const closed = new Promise<number | null>((done) => child.once("close", done));
    child.stdin.end("claim\n");
    await delay(1_200);
    blocker.exec("COMMIT");
    blocker.close();

    const code = await closed;
    expect(code, stderr).toBe(0);
    const claimed = JSON.parse(stdout.trim().split("\n").at(-1)!) as Message;
    expect(Date.parse(claimed.claimExpiresAt!) - Date.now()).toBeGreaterThan(500);
  }, 10_000);

  it("lets exactly one simultaneous claimant own an open task", async () => {
    const workerCount = 8;
    const bootstrap = openStore();
    const creator = agent("creator");
    bootstrap.announce(creator);
    for (let index = 0; index < workerCount; index += 1) {
      bootstrap.announce(identify({
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: `claimant-${index}`,
      }, repositoryRoot));
    }
    bootstrap.createTask(creator, "claim concurrently", { taskId: "shared-task" });
    bootstrap.close();

    const workerPath = fileURLToPath(new URL("./task-claim-worker.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const startAt = Date.now() + 1_000;
    const results = await Promise.all(Array.from({ length: workerCount }, (_, index) =>
      new Promise<{ code: number | null; stdout: string; stderr: string }>((done) => {
        const child = spawn(process.execPath, [
          "--import", tsxLoader, workerPath, databasePath, repositoryRoot,
          String(index), String(startAt),
        ], {
          cwd: repositoryRoot,
          env: { ...process.env, NODE_NO_WARNINGS: "1" },
          stdio: ["ignore", "pipe", "pipe"],
        });
        let stdout = "";
        let stderr = "";
        child.stdout.on("data", (chunk: Buffer | string) => { stdout += chunk.toString(); });
        child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
        child.once("close", (code) => done({ code, stdout, stderr }));
        child.once("error", (error) => done({ code: null, stdout, stderr: error.message }));
      }),
    ));
    expect(results.filter((result) => result.code !== 0), results.map((r) => r.stderr).join("\n"))
      .toEqual([]);
    const claims = results.map((result) => JSON.parse(result.stdout.trim()) as {
      won: boolean;
      owner?: string;
    });
    expect(claims.filter((claim) => claim.won)).toHaveLength(1);
    const verifier = openStore();
    expect(verifier.tasks(creator.project)).toMatchObject([{
      taskId: "shared-task",
      owner: claims.find((claim) => claim.won)!.owner,
    }]);
  }, 30_000);

  it("lets exactly one simultaneous reader claim a shared-label delivery", async () => {
    const workerCount = 8;
    const bootstrap = openStore();
    const sender = agent("sender");
    const recipient = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "shared-reader",
    }, repositoryRoot);
    bootstrap.announce(sender);
    bootstrap.announce(recipient);
    const sent = bootstrap.send(sender, recipient.agentId, "claim me once");
    bootstrap.close();

    const workerPath = fileURLToPath(new URL("./claim-worker.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const startAt = Date.now() + 1_000;
    const results = await Promise.all(Array.from({ length: workerCount }, () =>
      new Promise<{ code: number | null; stdout: string; stderr: string }>((done) => {
        const child = spawn(process.execPath, [
          "--import", tsxLoader, workerPath, databasePath, repositoryRoot, String(startAt),
        ], {
          cwd: repositoryRoot,
          env: { ...process.env, NODE_NO_WARNINGS: "1" },
          stdio: ["ignore", "pipe", "pipe"],
        });
        let stdout = "";
        let stderr = "";
        child.stdout.on("data", (chunk: Buffer | string) => { stdout += chunk.toString(); });
        child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
        child.once("close", (code) => done({ code, stdout, stderr }));
        child.once("error", (error) => done({ code: null, stdout, stderr: error.message }));
      }),
    ));
    const parsed = results.map((result) => JSON.parse(result.stdout.trim()) as {
      claims?: Array<{ messageId: string; claimId: string }>;
      error?: string;
    });
    expect(results.filter((result) => result.code !== 0)).toEqual([]);
    expect(parsed.flatMap((result) => result.error ?? [])).toEqual([]);
    const claims = parsed.flatMap((result) => result.claims ?? []);
    expect(claims).toHaveLength(1);
    expect(claims[0]).toMatchObject({ messageId: sent.messageId, claimId: expect.any(String) });
  }, 30_000);

  it("delivers every message while eight OS processes send and read together", async () => {
    const workerCount = 4;
    const operationsPerProcess = 20;
    const bootstrap = openStore();
    for (let index = 0; index < workerCount; index += 1) {
      bootstrap.announce(identify({
        AGENT_FABRIC_SEAT: "concurrency-worker",
        AGENT_FABRIC_LABEL: `worker-${index}`,
      }, repositoryRoot));
    }
    bootstrap.close();

    const workerPath = fileURLToPath(new URL("./concurrency-worker.ts", import.meta.url));
    const tsxLoader = createRequire(import.meta.url).resolve("tsx");
    const startAt = Date.now() + 1_000;
    const results = await Promise.all(Array.from({ length: workerCount }, (_, index) =>
      new Promise<{ code: number | null; stdout: string; stderr: string }>((done) => {
        const child = spawn(process.execPath, [
          "--import", tsxLoader, workerPath, databasePath, String(index),
          String(operationsPerProcess), String(workerCount), repositoryRoot, String(startAt),
        ], {
          cwd: repositoryRoot,
          env: { ...process.env, NODE_NO_WARNINGS: "1" },
          stdio: ["ignore", "pipe", "pipe"],
        });
        let stdout = "";
        let stderr = "";
        let finished = false;
        child.stdout.on("data", (chunk: Buffer | string) => { stdout += chunk.toString(); });
        child.stderr.on("data", (chunk: Buffer | string) => { stderr += chunk.toString(); });
        const finish = (code: number | null, error?: Error) => {
          if (finished) return;
          finished = true;
          if (error !== undefined) stderr += `${error.stack ?? error.message}\n`;
          done({ code, stdout, stderr });
        };
        child.once("error", (error) => finish(null, error));
        child.once("close", (code) => finish(code));
      }),
    ));

    const parsed = results.map((result, index) => {
      const line = result.stdout.trim().split("\n").at(-1);
      try {
        return { ...result, index, payload: JSON.parse(line ?? "") as {
          index: number;
          sent: number;
          delivered: number;
          failures: string[];
        } };
      } catch {
        return {
          ...result,
          index,
          payload: { index, sent: 0, delivered: 0, failures: [
            `worker emitted no JSON; stdout=${result.stdout}; stderr=${result.stderr}`,
          ] },
        };
      }
    });
    const failures = parsed.flatMap((result) => [
      ...(result.code === 0 ? [] : [`worker ${result.index} exited ${String(result.code)}: ${result.stderr}`]),
      ...result.payload.failures.map((failure) => `worker ${result.index}: ${failure}`),
    ]);
    const totalSent = parsed.reduce((total, result) => total + result.payload.sent, 0);
    const totalDelivered = parsed.reduce((total, result) => total + result.payload.delivered, 0);
    console.log("concurrency metrics", JSON.stringify({
      operationsPerProcess: operationsPerProcess,
      processCount: workerCount,
      totalSent,
      totalDelivered,
      failures,
    }));

    expect(failures, failures.join("\n")).toEqual([]);
    expect(parsed.map((result) => result.payload.sent)).toEqual(
      Array.from({ length: workerCount }, () => operationsPerProcess),
    );
    expect(parsed.map((result) => result.payload.delivered)).toEqual(
      Array.from({ length: workerCount }, () => operationsPerProcess),
    );

    const db = new Database(databasePath);
    const count = (sql: string): number =>
      Number((db.prepare(sql).get() as { count: number }).count);
    try {
      expect(count("SELECT count(*) AS count FROM messages")).toBe(totalSent);
      expect(count("SELECT count(*) AS count FROM deliveries")).toBe(totalSent);
      expect(count("SELECT count(*) AS count FROM deliveries WHERE read_at IS NULL")).toBe(0);
      expect(totalDelivered).toBe(totalSent);
    } finally {
      db.close();
    }
  }, 90_000);
});

it('routes chair and parent aliases and excludes deliveries older than fourteen days', () => {
 const store=openStore();announce(store,'chair-seat','worker');
 const oldChair=process.env.PROVENANT_CHAIR, oldParent=process.env.PROVENANT_PARENT;
 process.env.PROVENANT_CHAIR='chair-seat';process.env.PROVENANT_PARENT='chair-seat';
 try {
  for(const alias of ['chair','/root','root','parent']) expect(store.send(agent('worker'),alias,'question').recipients).toEqual(['chair-seat']);
  const rows=store.inbox(agent('chair-seat'),{peek:true});expect(rows.map((row) => row.body)).toEqual(expect.arrayContaining(['question']));
  const db=new Database(databasePath);db.prepare('UPDATE messages SET created_at = ?').run(Date.now()-15*86400000);db.close();
  expect(store.inbox(agent('chair-seat'),{peek:true})).toEqual([]);
 } finally {
  if(oldChair === undefined) delete process.env.PROVENANT_CHAIR;else process.env.PROVENANT_CHAIR=oldChair;
  if(oldParent === undefined) delete process.env.PROVENANT_PARENT;else process.env.PROVENANT_PARENT=oldParent;
 }
});

it('acknowledges a terminal notice published after status observation', () => {
 const store=openStore();announce(store,'chair');const who=agent('chair');
 store.acknowledgeTerminal(who,{run_id:'mcp-one',task_id:'task-1',attempt:2,run_dir:'/fixture'});
 store.send(who,'chair','done',{kind:'run_terminal',outputPath:'mcp-one:task-1:1'});
 store.send(who,'chair','done',{kind:'run_terminal',outputPath:'mcp-one:task-1:2'});
 store.send(who,'chair','new attempt',{kind:'run_terminal',outputPath:'mcp-one:task-1:3'});
 expect(store.inbox(who,{peek:true})).toMatchObject([{body:'new attempt'}]);
});

it('rejects unbound chair and parent aliases instead of guessing a stale seat', () => {
 const store=openStore();announce(store,'old-seat','worker');
 const saved={chair:process.env.PROVENANT_CHAIR,parent:process.env.PROVENANT_PARENT};
 delete process.env.PROVENANT_CHAIR;delete process.env.PROVENANT_PARENT;
 try {
  for(const to of ['chair','parent','root','/root']) expect(()=>store.send(agent('worker'),to,'question')).toThrow(/pass to:<seat>/u);
  expect(store.inbox(agent('old-seat'),{peek:true})).toEqual([]);
 } finally {
  if(saved.chair!==undefined) process.env.PROVENANT_CHAIR=saved.chair;
  if(saved.parent!==undefined) process.env.PROVENANT_PARENT=saved.parent;
 }
});
