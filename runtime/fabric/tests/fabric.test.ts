import { execFileSync, spawn, spawnSync } from "node:child_process";
import {
  chmodSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync,
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

import { identify } from "../src/identity.js";
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

function runCli(
  args: string[],
  stateDirectory = temporaryDirectory,
  identityEnv: Record<string, string> = {},
) {
  const cliPath = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
  const tsxLoader = createRequire(import.meta.url).resolve("tsx");
  return spawnSync(process.execPath, ["--import", tsxLoader, cliPath, ...args], {
    cwd: repositoryRoot,
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
  it("rejects an unknown command before creating or announcing", () => {
    const stateDirectory = join(temporaryDirectory, "unknown-command-state");
    const result = runCli(["frobnicate"], stateDirectory);

    expect(result.status).toBe(2);
    expect(result.stderr).toContain('unknown command "frobnicate"');
    expect(result.stderr).not.toContain("src/cli.ts");
    expect(existsSync(stateDirectory)).toBe(false);
  });

  it("reports expected store errors without a TypeScript stack", () => {
    const result = runCli(["send", "missing-agent", "hello"]);

    expect(result.status).toBe(1);
    expect(result.stderr).toContain('no recipients for "missing-agent"');
    expect(result.stderr).toContain('Use an agent label, team id, or "all"');
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
    expect(result.stderr).toContain("already belongs to client seat claude; refusing seat codex");
    expect(result.stderr).not.toContain("src/store.ts");
    expect(result.stderr).not.toContain("at Store.announce");
    expect(result.stdout).toBe("");
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
    expect(JSON.parse(status.stdout)).toMatchObject({
      status: "ok", exists: true, readOnly: true, project: alice.project,
      counts: { agents: 1, activity: 1 },
    });
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
      expect(result.stderr).toContain("claim seconds must be an integer from 1 to 3600");
      expect(result.stderr).not.toContain("src/cli.ts");
    }
  });

  it("forwards a positive inbox limit and rejects invalid limits", () => {
    expect(runCli(["--help"]).stdout).toContain("[--limit N]");

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
      expect(result.stderr).toContain("inbox limit must be a positive integer");
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
      expect(result.stderr).toContain("usage: fabric");
    }
    expect(openStore().tasks(cliAgent.project)).toMatchObject([{
      taskId: "extra-args",
      state: "open",
    }]);
  });

  it("keeps watch live after its initial 200-row window", async () => {
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
      "--import", tsxLoader, cliPath, "watch", "--interval", "0.02",
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
  it("waits inside one inbox call until a message arrives", async () => {
    const sender = agent("wait-sender");
    const seed = openStore();
    seed.announce(sender);
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
        arguments: { wait_seconds: 1 },
      }).finally(() => { settled = true; });

      await delay(75);
      expect(settled).toBe(false);
      const writer = openStore();
      writer.send(sender, "wait-recipient", "arrived during the MCP wait");
      writer.close();

      const result = await waiting;
      expect(result.isError).toBeUndefined();
      const content = result.content as Array<{ type: "text"; text: string }>;
      expect(JSON.parse(content[0]!.text)).toMatchObject([{
        body: "arrived during the MCP wait",
        claimId: expect.any(String),
      }]);
      const messages = JSON.parse(content[0]!.text) as Message[];
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
        arguments: { wait_seconds: 1 },
      });
      expect(Date.now() - timeoutStarted).toBeGreaterThanOrEqual(900);
      const timeoutContent = timedOut.content as Array<{ type: "text"; text: string }>;
      expect(JSON.parse(timeoutContent[0]!.text)).toEqual([]);

      const controller = new AbortController();
      const abandoned = client.callTool({
        name: "fabric_inbox",
        arguments: { wait_seconds: 2 },
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
        arguments: {},
      });
      const afterCancellationContent = afterCancellation.content as Array<{
        type: "text"; text: string;
      }>;
      const afterCancellationMessages = JSON.parse(
        afterCancellationContent[0]!.text,
      ) as Message[];
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

      const blocker = new Database(databasePath);
      blocker.exec("BEGIN IMMEDIATE");
      try {
        const lockedStarted = performance.now();
        const locked = await client.callTool({
          name: "fabric_inbox",
          arguments: { wait_seconds: 1 },
        }, undefined, { timeout: 2_000 });
        expect(performance.now() - lockedStarted).toBeLessThan(1_400);
        const lockedContent = locked.content as Array<{ type: "text"; text: string }>;
        expect(JSON.parse(lockedContent[0]!.text)).toEqual([]);
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
          text: expect.stringContaining(
            "already belongs to client seat claude; refusing seat codex",
          ),
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
        arguments: { wait_seconds: 1 },
      });
      expect(performance.now() - boundedStarted).toBeLessThan(1_400);
      expect(bounded.isError).toBeUndefined();
      const boundedContent = bounded.content as Array<{ type: "text"; text: string }>;
      expect(JSON.parse(boundedContent[0]!.text)).toEqual([]);

      const locked = await client.callTool({ name: "fabric_whoami", arguments: {} });
      expect(locked.isError).toBe(true);
      expect(locked.content).toMatchObject([{
        type: "text",
        text: expect.stringMatching(/fabric startup failed:.*database is locked/),
      }]);

      blocker.exec("COMMIT");
      lockHeld = false;
      const recovered = await client.callTool({ name: "fabric_whoami", arguments: {} });
      expect(recovered.isError).toBeUndefined();
      expect(recovered.content).toMatchObject([{
        type: "text",
        text: expect.stringContaining('"agentId": "lock-recovery"'),
      }]);
      const inbox = await client.callTool({ name: "fabric_inbox", arguments: {} });
      expect(inbox.isError).toBeUndefined();
      const inboxContent = inbox.content as Array<{ type: "text"; text: string }>;
      const messages = JSON.parse(inboxContent[0]!.text) as Message[];
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
    expect(message).toContain("Known agents:");
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
    })).toThrowError(/reply parent .* does not exist in project/);

    const otherProject = identify({ AGENT_FABRIC_SEAT: "mallory" }, temporaryDirectory);
    const otherRecipient = identify({
      AGENT_FABRIC_SEAT: "other-recipient",
    }, temporaryDirectory);
    store.announce(otherProject);
    store.announce(otherRecipient);
    const foreignParent = store.send(otherProject, otherRecipient.agentId, "foreign question");
    expect(() => store.send(bob, "alice", "cross-project answer", {
      replyTo: foreignParent.messageId,
    })).toThrowError(/reply parent .* does not exist in project/);

    const replies = store.inbox(alice);
    const threaded = replies.find((message) => message.messageId === reply.messageId);

    expect(threaded).toMatchObject({
      conversationId: parent.messageId,
      replyTo: parent.messageId,
    });
    expect(replies).toHaveLength(1);
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
    expect(store.activity(alice.project).filter((entry) => entry.detail.includes("claimed by")))
      .toHaveLength(1);
  });

  it("reserves claimed state for the atomic ownership operation", () => {
    const store = openStore();
    const alice = agent("alice");
    store.announce(alice);
    store.createTask(alice, "claim through ownership", { taskId: "reserved-claimed" });

    expect(() => store.updateTask(alice, "reserved-claimed", "claimed")).toThrowError(
      /state claimed is reserved for atomic task claim/,
    );
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
    for (let round = 0; round < 5; round += 1) {
      const coldDatabasePath = join(temporaryDirectory, `cold-start-${round}.sqlite3`);
      const startAt = Date.now() + 1_000;
      const results = await Promise.all(Array.from({ length: 16 }, (_, index) =>
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
      expect(verifier.agents(agent("alice").project)).toHaveLength(16);
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
    const workerCount = 16;
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
    const workerCount = 16;
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
    const workerCount = 8;
    const operationsPerProcess = 50;
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
