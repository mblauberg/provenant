import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { createConnection, createServer, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { BootstrapElection, FLOCK_ELECTION_LOCK_PORT } from "../../../src/daemon/bootstrap-election.ts";
import { attemptDrainedStop, attemptIdleStop } from "../../../src/lifecycle/global-liveness.ts";
import {
  closeRecoverableUnixListener,
  openRecoverableUnixListener,
  RecoverableServingAdmissionFence,
} from "../../../src/daemon/recoverable-serving-socket.ts";
import { createLivenessDatabase, seedProject } from "./liveness-fixture.ts";

const databases: ReturnType<typeof createLivenessDatabase>[] = [];
const directories: string[] = [];
const servers: Server[] = [];

export async function connectRawSocket(socketPath: string): Promise<Socket> {
  const socket = createConnection({ path: socketPath, allowHalfOpen: true });
  socket.on("error", () => undefined);
  await once(socket, "connect");
  return socket;
}

afterEach(async () => {
  vi.useRealTimers();
  await Promise.all(servers.splice(0).map(async (server) => {
    if (!server.listening) return;
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }));
  for (const database of databases.splice(0)) database.close();
  await Promise.allSettled(directories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("global idle stop races", () => {
  it("cancels quiesce when a concurrent launch advances global state", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    const root = await mkdtemp(join(tmpdir(), "fabric-idle-race-"));
    directories.push(root);
    const closeSocket = vi.fn();
    const reopenSocket = vi.fn();

    await expect(attemptIdleStop({
      actionId: "idle_race_01",
      election: new BootstrapElection({ runtimeDirectory: join(root, "runtime") }),
      database,
      daemonInstanceGeneration: 7,
      clock: () => 1_000,
      beforeFinalRecheck: async () => {
        seedProject(database, { sessionState: "active" });
        database.prepare("UPDATE daemon_global_state SET revision = revision + 1 WHERE singleton = 1").run();
      },
      closeSocket,
      reopenSocket,
    })).resolves.toMatchObject({ state: "busy", reason: "state-changed" });
    expect(closeSocket).not.toHaveBeenCalled();
    expect(reopenSocket).not.toHaveBeenCalled();
    expect(database.prepare("SELECT state, observed_global_revision FROM daemon_runtime_epochs WHERE instance_generation = 7").get())
      .toEqual({ state: "running", observed_global_revision: null });
  });

  it("holds the kernel election lock through final recheck and socket close", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    const root = await mkdtemp(join(tmpdir(), "fabric-idle-lock-order-"));
    directories.push(root);
    const runtimeDirectory = join(root, "runtime");
    const election = new BootstrapElection({ runtimeDirectory });
    const reopenSocket = vi.fn();
    let shutdownTransition: Awaited<ReturnType<typeof FLOCK_ELECTION_LOCK_PORT.tryAcquire>>;
    const closeSocket = vi.fn().mockImplementation(async () => {
      await expect(FLOCK_ELECTION_LOCK_PORT.tryAcquire(election.paths.lockPath)).resolves.toBeUndefined();
    });

    await expect(attemptIdleStop({
      actionId: "idle_stop_01",
      election,
      database,
      daemonInstanceGeneration: 7,
      clock: () => 1_000,
      closeSocket,
      reopenSocket,
      beforeElectionRelease: async () => {
        await expect(FLOCK_ELECTION_LOCK_PORT.tryAcquire(election.paths.lockPath)).resolves.toBeUndefined();
        shutdownTransition = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(join(runtimeDirectory, "daemon-shutdown.lock"));
        expect(shutdownTransition).toBeDefined();
      },
    })).resolves.toMatchObject({ state: "stopped", globalStateRevision: 1 });
    expect(closeSocket).toHaveBeenCalledTimes(1);
    expect(reopenSocket).not.toHaveBeenCalled();
    expect(database.prepare("SELECT state, stopped_at FROM daemon_runtime_epochs WHERE instance_generation = 7").get())
      .toEqual({ state: "stopped", stopped_at: 1_000 });
    await expect(FLOCK_ELECTION_LOCK_PORT.probe(join(runtimeDirectory, "daemon-shutdown.lock")))
      .resolves.toMatchObject({ status: "held" });
    await shutdownTransition?.release();

    await expect(attemptIdleStop({
      actionId: "idle_stop_repeat",
      election,
      database,
      daemonInstanceGeneration: 7,
      clock: () => 1_001,
      closeSocket,
      reopenSocket,
    })).resolves.toMatchObject({ state: "busy", reason: "epoch-not-running" });
    expect(closeSocket).toHaveBeenCalledTimes(1);
  });

  it("restores serving when a drained in-flight command creates a liveness contributor", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    const root = await mkdtemp(join(tmpdir(), "fabric-idle-post-drain-race-"));
    directories.push(root);
    const reopenSocket = vi.fn();

    await expect(attemptIdleStop({
      actionId: "idle_stop_post_drain_race_01",
      election: new BootstrapElection({ runtimeDirectory: join(root, "runtime") }),
      database,
      daemonInstanceGeneration: 7,
      clock: () => 1_000,
      closeSocket: async () => {
        seedProject(database, { sessionState: "active" });
        database.prepare("UPDATE daemon_global_state SET revision = revision + 1 WHERE singleton = 1").run();
      },
      reopenSocket,
    })).resolves.toMatchObject({ state: "busy", reason: "state-changed" });
    expect(reopenSocket).toHaveBeenCalledTimes(1);
    expect(database.prepare("SELECT state, observed_global_revision FROM daemon_runtime_epochs WHERE instance_generation = 7").get())
      .toEqual({ state: "running", observed_global_revision: null });
  });

  it("reopens the Unix listener and accepts connections after a post-drain busy result", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    const root = await mkdtemp(join(tmpdir(), "fabric-relisten-"));
    directories.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(root, "fabric.sock");
    const activeSockets = new Set<Socket>();
    let acceptConnection!: () => void;
    const accepted = new Promise<void>((resolve) => { acceptConnection = resolve; });
    const server = createServer((socket) => {
      activeSockets.add(socket);
      socket.once("close", () => activeSockets.delete(socket));
      acceptConnection();
      socket.end();
    });
    servers.push(server);
    await openRecoverableUnixListener(server, socketPath);

    await expect(attemptIdleStop({
      actionId: "idle_stop_relisten_01",
      election: new BootstrapElection({ runtimeDirectory }),
      database,
      daemonInstanceGeneration: 7,
      clock: () => 1_000,
      closeSocket: async () => await closeRecoverableUnixListener({
        server,
        sockets: activeSockets,
        waitForInFlight: async () => {
          seedProject(database, { sessionState: "active" });
          database.prepare("UPDATE daemon_global_state SET revision = revision + 1 WHERE singleton = 1").run();
        },
        closeTimeoutMs: 500,
        drainTimeoutMs: 500,
      }),
      reopenSocket: async () => await openRecoverableUnixListener(server, socketPath),
    })).resolves.toMatchObject({ state: "busy", reason: "state-changed" });

    const client = createConnection(socketPath);
    await new Promise<void>((resolve, reject) => {
      client.once("connect", resolve);
      client.once("error", reject);
    });
    await accepted;
    client.end();
    expect(server.listening).toBe(true);
  });

  it("does not finish a Unix listener drain while an in-flight operation remains tracked", async () => {
    vi.useFakeTimers();
    const root = await mkdtemp(join(tmpdir(), "fabric-late-frame-drain-"));
    directories.push(root);
    const socketPath = join(root, "fabric.sock");
    const activeSockets = new Set<Socket>();
    let totalInFlight = 0;
    const inFlightDrainers = new Set<() => void>();
    let startCommand!: () => void;
    const commandStarted = new Promise<void>((resolve) => { startCommand = resolve; });
    let finishCommand!: () => void;
    const commandFinished = new Promise<void>((resolve) => { finishCommand = resolve; });
    const server = createServer((socket) => {
      activeSockets.add(socket);
      socket.once("close", () => activeSockets.delete(socket));
      socket.once("data", () => {
        totalInFlight += 1;
        startCommand();
        socket.destroy();
        void commandFinished.then(() => {
          totalInFlight -= 1;
          if (totalInFlight !== 0) return;
          for (const resolvePromise of inFlightDrainers) resolvePromise();
          inFlightDrainers.clear();
        });
      });
    });
    servers.push(server);
    await openRecoverableUnixListener(server, socketPath);

    const client = createConnection(socketPath);
    await new Promise<void>((resolve, reject) => {
      client.once("connect", resolve);
      client.once("error", reject);
    });
    client.write("held frame\n");
    await commandStarted;
    const closing = closeRecoverableUnixListener({
      server,
      sockets: activeSockets,
      waitForInFlight: async () => {
        if (totalInFlight === 0) return;
        await new Promise<void>((resolvePromise) => inFlightDrainers.add(resolvePromise));
      },
      closeTimeoutMs: 500,
      drainTimeoutMs: 500,
    });

    const finishedBeforeCommand = Promise.race([
      closing.then(() => true),
      new Promise<false>((resolve) => setTimeout(() => resolve(false), 25)),
    ]);
    await vi.advanceTimersByTimeAsync(25);
    await expect(finishedBeforeCommand).resolves.toBe(false);
    finishCommand();
    await closing;
    client.destroy();
  });

  it("fails with typed evidence when real in-flight work misses the shutdown drain deadline", async () => {
    vi.useFakeTimers();
    const operationFinished = Promise.withResolvers<void>();
    let inFlight = false;
    const server = {
      listening: true,
      close(callback: (error?: Error) => void): void {
        this.listening = false;
        callback();
      },
    } as unknown as Server;
    inFlight = true;
    const outcome = Promise.race([
      closeRecoverableUnixListener({
        server,
        sockets: [],
        waitForInFlight: async () => {
          if (inFlight) await operationFinished.promise;
        },
        closeTimeoutMs: 50,
        drainTimeoutMs: 10,
      }).catch((error: unknown) => error),
      new Promise<{ code: string }>((resolve) => setTimeout(() => resolve({ code: "TEST_TIMEOUT" }), 100)),
    ]);
    await vi.advanceTimersByTimeAsync(100);

    await expect(outcome).resolves.toMatchObject({
      code: "DAEMON_SHUTDOWN_DRAIN_TIMEOUT",
      message: "in-flight operations did not drain within 10ms",
    });
    operationFinished.resolve();
  });

  it("fails with distinct typed evidence when an open socket misses the shutdown close deadline", async () => {
    vi.useFakeTimers();
    const server = { listening: true, close(): void {} } as unknown as Server;
    const outcome = Promise.race([
      closeRecoverableUnixListener({
        server,
        sockets: [],
        waitForInFlight: async () => undefined,
        closeTimeoutMs: 10,
        drainTimeoutMs: 50,
      }).catch((error: unknown) => error),
      new Promise<{ code: string }>((resolve) => setTimeout(() => resolve({ code: "TEST_TIMEOUT" }), 100)),
    ]);
    await vi.advanceTimersByTimeAsync(100);

    await expect(outcome).resolves.toMatchObject({
      code: "DAEMON_SHUTDOWN_CLOSE_TIMEOUT",
      message: "serving socket did not close within 10ms",
    });
  });

  it("rejects a frame arriving after the serving admission fence closes", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-late-frame-fence-"));
    directories.push(root);
    const socketPath = join(root, "fabric.sock");
    const activeSockets = new Set<Socket>();
    const admissionFence = new RecoverableServingAdmissionFence();
    const operationStarted = Promise.withResolvers<void>();
    const releaseOperation = Promise.withResolvers<void>();
    const operationFinished = Promise.withResolvers<void>();
    let inFlight = 0;
    let decide!: (decision: "admitted" | "rejected") => void;
    const decision = new Promise<"admitted" | "rejected">((resolve) => { decide = resolve; });
    const server = createServer((socket) => {
      activeSockets.add(socket);
      socket.once("close", () => activeSockets.delete(socket));
      socket.on("data", (chunk) => {
        if (chunk.toString("utf8").includes("held frame")) {
          inFlight += 1;
          operationStarted.resolve();
          void releaseOperation.promise.then(() => {
            inFlight -= 1;
            operationFinished.resolve();
            socket.destroy();
          });
          return;
        }
        if (chunk.toString("utf8").includes("queued frame")) {
          decide(admissionFence.tryAdmit() ? "admitted" : "rejected");
        }
      });
    });
    servers.push(server);
    await openRecoverableUnixListener(server, socketPath, { admissionFence });

    const client = createConnection(socketPath);
    await new Promise<void>((resolve, reject) => {
      client.once("connect", resolve);
      client.once("error", reject);
    });
    client.write("held frame\n");
    await operationStarted.promise;
    const closing = closeRecoverableUnixListener({
      server,
      sockets: activeSockets,
      waitForInFlight: async () => {
        if (inFlight !== 0) await operationFinished.promise;
      },
      closeTimeoutMs: 500,
      drainTimeoutMs: 500,
      admissionFence,
    });
    client.write("queued frame\n");

    await expect(decision).resolves.toBe("rejected");
    releaseOperation.resolve();
    await closing;
    client.destroy();
  });

  it("reopens admission when listener recovery finds the Unix server already listening", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-listener-idempotent-recovery-"));
    directories.push(root);
    const socketPath = join(root, "fabric.sock");
    const admissionFence = new RecoverableServingAdmissionFence();
    const server = createServer();
    servers.push(server);
    await openRecoverableUnixListener(server, socketPath, { admissionFence });
    admissionFence.close();
    expect(admissionFence.tryAdmit()).toBe(false);

    await openRecoverableUnixListener(server, socketPath, { admissionFence });

    expect(server.listening).toBe(true);
    expect(admissionFence.tryAdmit()).toBe(true);
  });

  it("closes a newly opened Unix listener when socket permission hardening fails", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-listener-mode-failure-"));
    directories.push(root);
    const socketPath = join(root, "fabric.sock");
    const server = createServer();
    servers.push(server);

    await expect(openRecoverableUnixListener(server, socketPath, {
      setMode: () => { throw new Error("mode hardening failed"); },
    })).rejects.toThrow("mode hardening failed");
    expect(server.listening).toBe(false);
  });

  it("atomically rejects a liveness revision that advances after the post-drain recheck", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    const root = await mkdtemp(join(tmpdir(), "fabric-idle-stop-cas-race-"));
    directories.push(root);
    const runtimeDirectory = join(root, "runtime");
    const reopenSocket = vi.fn();
    let leakedTransition: Awaited<ReturnType<typeof FLOCK_ELECTION_LOCK_PORT.tryAcquire>>;
    const beforeElectionRelease = vi.fn(async () => {
      leakedTransition = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(join(runtimeDirectory, "daemon-shutdown.lock"));
      expect(leakedTransition).toBeDefined();
    });

    await expect(attemptIdleStop({
      actionId: "idle_stop_cas_race_01",
      election: new BootstrapElection({ runtimeDirectory }),
      database,
      daemonInstanceGeneration: 7,
      clock: () => 1_000,
      beforeStopCommit: async () => {
        seedProject(database, { sessionState: "active" });
        database.prepare("UPDATE daemon_global_state SET revision = revision + 1 WHERE singleton = 1").run();
      },
      closeSocket: vi.fn(),
      reopenSocket,
      beforeElectionRelease,
    })).resolves.toMatchObject({ state: "busy", reason: "state-changed" });
    expect(reopenSocket).toHaveBeenCalledTimes(1);
    expect(beforeElectionRelease).not.toHaveBeenCalled();
    const shutdownProbe = await FLOCK_ELECTION_LOCK_PORT.probe(join(runtimeDirectory, "daemon-shutdown.lock"));
    expect(shutdownProbe.status).toBe("acquired");
    if (shutdownProbe.status === "acquired") await shutdownProbe.handle.release();
    await leakedTransition?.release();
    expect(database.prepare("SELECT state, stopped_at FROM daemon_runtime_epochs WHERE instance_generation = 7").get())
      .toEqual({ state: "running", stopped_at: null });
  });

  it("completes only the exact receipt-bound quiescing epoch", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    database.prepare(`
      UPDATE daemon_runtime_epochs SET state='quiescing', observed_global_revision=1
       WHERE instance_generation=7
    `).run();
    const root = await mkdtemp(join(tmpdir(), "fabric-drained-stop-"));
    directories.push(root);
    const closeSocket = vi.fn();
    const reopenSocket = vi.fn();

    await expect(attemptDrainedStop({
      actionId: "drained_stop_01",
      token: { daemonInstanceGeneration: 7, observedGlobalStateRevision: 1 },
      election: new BootstrapElection({ runtimeDirectory: join(root, "runtime") }),
      database,
      clock: () => 1_000,
      closeSocket,
      reopenSocket,
    })).resolves.toMatchObject({ state: "stopped", globalStateRevision: 1 });
    expect(closeSocket).toHaveBeenCalledTimes(1);
    expect(reopenSocket).not.toHaveBeenCalled();
  });

  it("does not publish a shutdown transition when a drained stop is cancelled", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    database.prepare(`
      UPDATE daemon_runtime_epochs SET state='quiescing', observed_global_revision=1
       WHERE instance_generation=7
    `).run();
    const root = await mkdtemp(join(tmpdir(), "fabric-drained-stop-cancelled-"));
    directories.push(root);
    const runtimeDirectory = join(root, "runtime");
    const beforeElectionRelease = vi.fn();

    await expect(attemptDrainedStop({
      actionId: "drained_stop_cancelled_01",
      token: { daemonInstanceGeneration: 7, observedGlobalStateRevision: 1 },
      election: new BootstrapElection({ runtimeDirectory }),
      database,
      clock: () => 1_000,
      beforeStopCommit: async () => {
        database.prepare("UPDATE daemon_global_state SET revision = revision + 1 WHERE singleton = 1").run();
      },
      beforeElectionRelease,
      closeSocket: vi.fn(),
      reopenSocket: vi.fn(),
    })).resolves.toMatchObject({ state: "busy", reason: "state-changed" });
    expect(beforeElectionRelease).not.toHaveBeenCalled();
    const shutdownProbe = await FLOCK_ELECTION_LOCK_PORT.probe(join(runtimeDirectory, "daemon-shutdown.lock"));
    expect(shutdownProbe.status).toBe("acquired");
    if (shutdownProbe.status === "acquired") await shutdownProbe.handle.release();
  });

  it("excludes only the dispatching daemon-stop command from its drained liveness check", async () => {
    const database = createLivenessDatabase();
    databases.push(database);
    seedProject(database);
    database.prepare(`
      INSERT INTO operator_effect_custody(custody_id, project_session_id, state)
      VALUES ('daemon_stop_self_01', 'session_01', 'dispatching')
    `).run();
    database.prepare(`
      UPDATE daemon_runtime_epochs SET state='quiescing', observed_global_revision=1
       WHERE instance_generation=7
    `).run();
    const root = await mkdtemp(join(tmpdir(), "fabric-drained-stop-self-custody-"));
    directories.push(root);

    await expect(attemptDrainedStop({
      actionId: "drained_stop_self_custody_01",
      token: { daemonInstanceGeneration: 7, observedGlobalStateRevision: 1 },
      election: new BootstrapElection({ runtimeDirectory: join(root, "runtime") }),
      database,
      clock: () => 1_000,
      closeSocket: vi.fn(),
      reopenSocket: vi.fn(),
      excludeOperatorEffectCustodyId: "daemon_stop_self_01",
    })).resolves.toMatchObject({ state: "stopped", globalStateRevision: 1 });
    expect(database.prepare("SELECT state FROM operator_effect_custody WHERE custody_id='daemon_stop_self_01'").get())
      .toEqual({ state: "dispatching" });
  });
});
