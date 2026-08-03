import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import Database from "better-sqlite3";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { identify, projectRoot } from "../src/identity.js";
import { Store } from "../src/store.js";

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

describe("identity derivation", () => {
  it("uses the git toplevel in a repository and the directory outside one", () => {
    expect(projectRoot(repositoryRoot)).toBe(repositoryRoot);
    expect(projectRoot(temporaryDirectory)).toBe(resolve(temporaryDirectory));
    expect(execFileSync("git", ["rev-parse", "--show-toplevel"], {
      cwd: repositoryRoot,
      encoding: "utf8",
    }).trim()).toBe(repositoryRoot);
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
    expect(message).toContain("Known agents:");
    for (const known of ["alice", "bob", "carol"]) expect(message).toContain(known);
  });

  it("marks reads, leaves peeks unread, and keeps broadcasts independent", () => {
    const store = openStore();
    announce(store, "alice", "bob", "carol");
    const alice = agent("alice");
    const bob = agent("bob");
    const carol = agent("carol");

    store.send(alice, "bob", "peekable");
    expect(store.inbox(bob, { peek: true }).map((message) => message.body)).toEqual(["peekable"]);
    expect(store.inbox(bob).map((message) => message.body)).toEqual(["peekable"]);
    expect(store.inbox(bob)).toEqual([]);

    store.send(alice, "all", "independent read");
    expect(store.inbox(bob).map((message) => message.body)).toEqual(["independent read"]);
    expect(store.inbox(bob)).toEqual([]);
    expect(store.inbox(carol).map((message) => message.body)).toEqual(["independent read"]);
  });

  it("threads replies and tolerates a missing parent", () => {
    const store = openStore();
    announce(store, "alice", "bob");
    const alice = agent("alice");
    const bob = agent("bob");

    const parent = store.send(alice, "bob", "question");
    const parentMessage = store.inbox(bob)[0];
    expect(parentMessage?.messageId).toBe(parent.messageId);

    const reply = store.send(bob, "alice", "answer", { replyTo: parent.messageId });
    const missingParentReply = store.send(bob, "alice", "orphan answer", {
      replyTo: "message-that-does-not-exist",
    });
    const replies = store.inbox(alice);
    const threaded = replies.find((message) => message.messageId === reply.messageId);
    const orphan = replies.find((message) => message.messageId === missingParentReply.messageId);

    expect(threaded).toMatchObject({
      conversationId: parent.messageId,
      replyTo: parent.messageId,
    });
    expect(orphan).toMatchObject({
      conversationId: missingParentReply.messageId,
      replyTo: null,
    });
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
    expect(() => store.updateTask(alice, "missing", "done")).toThrowError(
      /no task missing in /,
    );
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
});

describe("multi-process WAL concurrency", () => {
  it("delivers every message while eight OS processes send and read together", async () => {
    const workerCount = 8;
    const operationsPerProcess = 50;
    const bootstrap = openStore();
    announce(bootstrap, ...Array.from({ length: workerCount }, (_, index) => `worker-${index}`));
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
