import { spawn } from "node:child_process";
import { chmod, link, mkdir, mkdtemp, readdir, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import { BootstrapElection, FLOCK_ELECTION_LOCK_PORT } from "../../../src/daemon/bootstrap-election.ts";

const cleanup: string[] = [];

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("bootstrap election receipts", () => {
  it("publishes one generation through private non-SQLite runtime artifacts", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-election-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const election = new BootstrapElection({
      runtimeDirectory,
      clock: () => 1_000,
      leaseDurationMs: 5_000,
      waitTimeoutMs: 200,
      pollIntervalMs: 5,
    });

    const result = await election.withExclusiveLock("bootstrap_action_01", async (held) => {
      const generation = await held.beginGeneration();
      await generation.appendAttempt("database-owned", "progress");
      const ready = await generation.publishReady({
        daemonInstanceGeneration: 7,
        socketPath: join(runtimeDirectory, "fabric.sock"),
        protocolVersion: 1,
        features: ["fabric-core.v1", "project-sessions.v1"],
        evidence: {
          databaseOwned: true,
          migrationsComplete: true,
          recoveryComplete: true,
          socketBound: true,
        },
      });
      await generation.confirmReady();
      return ready;
    });

    expect(result).toMatchObject({
      role: "owner",
      value: {
        actionId: "bootstrap_action_01",
        electionGeneration: 1,
        daemonInstanceGeneration: 7,
      },
    });
    await expect(election.readGenerationOutcome(1)).resolves.toMatchObject({
      kind: "ready",
      receipt: { electionGeneration: 1, daemonInstanceGeneration: 7 },
    });

    const names = await readdir(runtimeDirectory);
    expect(names.some((name) => name.endsWith(".sqlite") || name.endsWith(".sqlite3"))).toBe(false);
    expect(names.sort()).toEqual([
      "daemon-election.attempts.jsonl",
      "daemon-election.lease.json",
      "daemon-election.lock",
      "daemon-election.ready.json",
    ]);
    expect((await stat(runtimeDirectory)).mode & 0o777).toBe(0o700);
    for (const name of names) {
      expect((await stat(join(runtimeDirectory, name))).mode & 0o777).toBe(0o600);
    }
    const attempts = (await readFile(join(runtimeDirectory, "daemon-election.attempts.jsonl"), "utf8"))
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line) as { actionId: string; electionGeneration: number; phase: string });
    expect(attempts).toEqual([
      expect.objectContaining({ actionId: "bootstrap_action_01", electionGeneration: 1, phase: "election-acquired" }),
      expect.objectContaining({ actionId: "bootstrap_action_01", electionGeneration: 1, phase: "database-owned" }),
      expect.objectContaining({ actionId: "bootstrap_action_01", electionGeneration: 1, phase: "ready-receipt" }),
    ]);
  });

  it("uses a kernel flock that excludes a separate process until release", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-flock-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const holder = fileURLToPath(new URL("./flock-holder.ts", import.meta.url));
    const child = spawn(process.execPath, ["--import", "tsx", holder], {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      env: { ...process.env, FABRIC_TEST_RUNTIME_DIRECTORY: runtimeDirectory },
      stdio: ["pipe", "pipe", "pipe"],
    });
    if (child.stdout === null || child.stdin === null) throw new Error("flock child stdio is unavailable");
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("flock child did not acquire its lock")), 5_000);
      lines.once("line", (line) => {
        clearTimeout(timeout);
        line === "locked" ? resolve() : reject(new Error(`unexpected flock child output: ${line}`));
      });
      child.once("exit", (code) => reject(new Error(`flock child exited before release: ${String(code)}`)));
    });

    const lockPath = join(runtimeDirectory, "daemon-election.lock");
    await expect(FLOCK_ELECTION_LOCK_PORT.tryAcquire(lockPath)).resolves.toBeUndefined();
    child.stdin.write("release\n");
    await new Promise<void>((resolve, reject) => {
      child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`flock child failed: ${String(code)}`)));
    });
    lines.close();
    const acquired = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(lockPath);
    expect(acquired).toBeDefined();
    await acquired?.release();
  });

  it("fences an initially absent lock path before inspecting election artifacts", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-probe-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const lockPath = join(runtimeDirectory, "daemon-election.lock");

    const probe = await FLOCK_ELECTION_LOCK_PORT.probe(lockPath);
    expect(probe.status).toBe("acquired");
    if (probe.status !== "acquired") throw new Error("probe did not fence the absent lock path");
    await expect(FLOCK_ELECTION_LOCK_PORT.tryAcquire(lockPath)).resolves.toBeUndefined();
    expect((await stat(lockPath)).mode & 0o777).toBe(0o600);
    await probe.handle.release();

    const acquired = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(lockPath);
    expect(acquired).toBeDefined();
    await acquired?.release();
  });

  it("allows concurrent shared inspection while excluding bootstrap ownership", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-shared-probe-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const lockPath = join(runtimeDirectory, "daemon-election.lock");

    const first = await FLOCK_ELECTION_LOCK_PORT.probe(lockPath);
    const second = await FLOCK_ELECTION_LOCK_PORT.probe(lockPath);
    expect(first.status).toBe("acquired");
    expect(second.status).toBe("acquired");
    await expect(FLOCK_ELECTION_LOCK_PORT.tryAcquire(lockPath)).resolves.toBeUndefined();
    if (second.status === "acquired") await second.handle.release();
    if (first.status === "acquired") await first.handle.release();
  });

  it("holds the shared inspection fence through the caller snapshot", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-snapshot-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const election = new BootstrapElection({ runtimeDirectory });
    let entered: (() => void) | undefined;
    let release: (() => void) | undefined;
    const enteredPromise = new Promise<void>((resolve) => { entered = resolve; });
    const releasePromise = new Promise<void>((resolve) => { release = resolve; });
    const snapshot = election.inspectCurrentWith(async (inspection) => {
      entered?.();
      await releasePromise;
      return inspection.status;
    });

    await enteredPromise;
    await expect(FLOCK_ELECTION_LOCK_PORT.tryAcquire(election.paths.lockPath)).resolves.toBeUndefined();
    release?.();
    await expect(snapshot).resolves.toBe("absent");
  });

  it("retries under the shared fence when an initially absent lock appears during the read-only snapshot", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-read-only-race-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const election = new BootstrapElection({ runtimeDirectory });
    let held: Awaited<ReturnType<typeof FLOCK_ELECTION_LOCK_PORT.tryAcquire>>;
    const observations: string[] = [];

    try {
      const result = await election.inspectCurrentReadOnlyWith(async (inspection) => {
        observations.push(inspection.status);
        if (observations.length === 1) {
          held = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(election.paths.lockPath);
          expect(held).toBeDefined();
        }
        return inspection.status;
      });

      expect(result).toBe("active");
      expect(observations).toEqual(["absent", "active"]);
    } finally {
      await held?.release();
    }
  });

  it("requires both lease expiry and kernel-lock release before reclaiming a generation", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-reclaim-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const holder = fileURLToPath(new URL("./generation-holder.ts", import.meta.url));
    const child = spawn(process.execPath, ["--import", "tsx", holder], {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      env: { ...process.env, FABRIC_TEST_RUNTIME_DIRECTORY: runtimeDirectory },
      stdio: ["pipe", "pipe", "pipe"],
    });
    if (child.stdout === null) throw new Error("generation child stdout is unavailable");
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("generation child did not begin")), 5_000);
      lines.once("line", (line) => {
        clearTimeout(timeout);
        line === "1" ? resolve() : reject(new Error(`unexpected generation: ${line}`));
      });
    });
    await new Promise((resolve) => setTimeout(resolve, 120));
    const contender = new BootstrapElection({
      runtimeDirectory,
      leaseDurationMs: 80,
      waitTimeoutMs: 60,
      pollIntervalMs: 5,
    });
    await expect(contender.withExclusiveLock("blocked_action", async () => "unreachable"))
      .rejects.toMatchObject({ code: "BOOTSTRAP_ELECTION_TIMEOUT" });

    child.kill("SIGKILL");
    await new Promise<void>((resolve) => child.once("exit", () => resolve()));
    lines.close();
    const recovered = await contender.withExclusiveLock("recovered_action", async (held) => {
      const generation = await held.beginGeneration();
      await generation.recordTerminal({ status: "failed", code: "EXPECTED_TEST_END", message: "test completed" });
      return generation.electionGeneration;
    });
    expect(recovered).toEqual({ role: "owner", value: 2 });
  });

  it("fails closed for symbolic links, hard links and insecure election material", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-unsafe-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const target = join(root, "target.json");
    await writeFile(target, "{}\n", { mode: 0o600 });
    await symlink(target, join(runtimeDirectory, "daemon-election.lease.json"));
    const symlinked = new BootstrapElection({ runtimeDirectory });
    await expect(symlinked.withExclusiveLock("unsafe_symlink", async (held) => held.beginGeneration()))
      .rejects.toMatchObject({ code: "BOOTSTRAP_PATH_UNSAFE" });

    await rm(join(runtimeDirectory, "daemon-election.lease.json"));
    await link(target, join(runtimeDirectory, "daemon-election.attempts.jsonl"));
    const hardLinked = new BootstrapElection({ runtimeDirectory });
    await expect(hardLinked.withExclusiveLock("unsafe_hardlink", async (held) => held.beginGeneration()))
      .rejects.toMatchObject({ code: "BOOTSTRAP_PATH_UNSAFE" });

    await rm(join(runtimeDirectory, "daemon-election.attempts.jsonl"));
    await chmod(join(runtimeDirectory, "daemon-election.lock"), 0o644);
    const insecure = new BootstrapElection({ runtimeDirectory });
    await expect(insecure.withExclusiveLock("unsafe_mode", async () => "unreachable"))
      .rejects.toMatchObject({ code: "BOOTSTRAP_PATH_UNSAFE" });
  });

  it("never reports an older ready receipt while a newer generation owns the lock", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-stale-ready-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const election = new BootstrapElection({ runtimeDirectory, waitTimeoutMs: 500, pollIntervalMs: 2 });
    await election.withExclusiveLock("ready_generation_1", async (held) => {
      const generation = await held.beginGeneration();
      await generation.publishReady({
        daemonInstanceGeneration: 1,
        socketPath,
        protocolVersion: 1,
        features: ["fabric-core.v1"],
        evidence: { databaseOwned: true, migrationsComplete: true, recoveryComplete: true, socketBound: true },
      });
      await generation.confirmReady();
    });

    let began: (() => void) | undefined;
    let release: (() => void) | undefined;
    const beganPromise = new Promise<void>((resolve) => { began = resolve; });
    const releasePromise = new Promise<void>((resolve) => { release = resolve; });
    const newer = election.withExclusiveLock("active_generation_2", async (held) => {
      const generation = await held.beginGeneration();
      began?.();
      await releasePromise;
      await generation.recordTerminal({ status: "failed", code: "TEST_END", message: "test complete" });
    });
    await beganPromise;
    const contender = new BootstrapElection({ runtimeDirectory, waitTimeoutMs: 40, pollIntervalMs: 2 });
    await expect(contender.withExclusiveLock("contender", async () => "unreachable"))
      .rejects.toMatchObject({ code: "BOOTSTRAP_ELECTION_TIMEOUT" });
    release?.();
    await newer;
  });

  it("does not treat a ready file without its confirmed generation lease as authoritative", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-orphan-ready-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const election = new BootstrapElection({ runtimeDirectory });
    await election.withExclusiveLock("orphan_ready", async (held) => {
      const generation = await held.beginGeneration();
      await generation.publishReady({
        daemonInstanceGeneration: 3,
        socketPath: join(runtimeDirectory, "fabric.sock"),
        protocolVersion: 1,
        features: ["fabric-core.v1"],
        evidence: { databaseOwned: true, migrationsComplete: true, recoveryComplete: true, socketBound: true },
      });
      await generation.confirmReady();
    });
    await rm(election.paths.leasePath);
    const heldLock = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(election.paths.lockPath);
    if (heldLock === undefined) throw new Error("test could not hold election lock");
    try {
      const contender = new BootstrapElection({ runtimeDirectory, waitTimeoutMs: 40, pollIntervalMs: 2 });
      await expect(contender.withExclusiveLock("orphan_contender", async () => "unreachable"))
        .rejects.toMatchObject({ code: "BOOTSTRAP_RECEIPT_INVALID" });
    } finally {
      await heldLock.release();
    }
  });
});
