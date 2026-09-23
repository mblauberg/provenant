#!/usr/bin/env node
// A dispatch owner that behaves like the real one where run lifecycle is
// concerned: it spawns a provider child in its own process group, records both
// pids in the run directory, and stays alive until something signals it.
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, relative } from "node:path";

const value = (flag) => {
  const index = process.argv.indexOf(flag);
  return index < 0 ? undefined : process.argv[index + 1];
};

if (process.argv.includes("--preflight-json")) {
  let input = "";
  for await (const chunk of process.stdin) input += chunk;
  if (process.env.FIXTURE_PREFLIGHT_FAILURE === "exit") process.exit(9);
  if (process.env.FIXTURE_PREFLIGHT_FAILURE === "json") {
    process.stdout.write("not JSON");
    process.exit(0);
  }
  const tasks = JSON.parse(input).tasks;
  if (process.env.FIXTURE_PREFLIGHT_PID) {
    writeFileSync(process.env.FIXTURE_PREFLIGHT_PID, String(process.pid));
    while (!existsSync(process.env.FIXTURE_PREFLIGHT_RELEASE)) {
      await new Promise((done) => setTimeout(done, 20));
    }
  }
  process.stdout.write(JSON.stringify({ status: "validated", routes: tasks }));
  process.exit(0);
}

const owner = process.env.PROVENANT_FIXTURE_OWNER ?? basename(process.argv[1]);

if (owner === "run_dir_init.sh") {
  if (process.env.FIXTURE_SETUP_FAILURE) process.exit(9);
  const runDir = process.argv[2];
  if (process.env.FIXTURE_SETUP_PID) {
    writeFileSync(process.env.FIXTURE_SETUP_PID, String(process.pid));
    while (!existsSync(process.env.FIXTURE_SETUP_RELEASE)) {
      await new Promise((done) => setTimeout(done, 20));
    }
  }
  mkdirSync(join(runDir, "findings"), { recursive: true });
  if (process.argv.includes("--owner-logs")) mkdirSync(join(runDir, "_owner"));
  mkdirSync(join(runDir, "traces"), { recursive: true });
  writeFileSync(join(runDir, "MANIFEST.md"), "# fixture manifest\n");
  writeFileSync(join(runDir, "RUN_RECEIPT.json"), JSON.stringify({
    schema_version: 1,
    status: "active",
  }) + "\n");
  if (process.env.FIXTURE_STATUS_DIRECTORY) mkdirSync(join(runDir, "dispatch-status.json"));
  process.stdout.write(`${runDir}\n`);
  process.exit(0);
}

const runDir = value("--run-dir");

/**
 * A provider may lead its own session. It answers only its own SIGTERM, so a
 * record must retain the provider group after the owner exits.
 */
const startProvider = ({ ignoreTerm = false, detached = false } = {}) => {
  const providerEnvironment = {
    ...process.env,
    PROVIDER_READY_MARKER: join(runDir, "provider-ready.marker"),
    ...(ignoreTerm ? { PROVIDER_TERM_MARKER: join(runDir, "provider-term-ignored.marker") } : {}),
  };
  const provider = spawn(process.execPath, [
    "-e",
    ignoreTerm
      ? "const { writeFileSync } = require('node:fs'); process.on('SIGTERM', () => writeFileSync(process.env.PROVIDER_TERM_MARKER, 'SIGTERM\\n')); writeFileSync(process.env.PROVIDER_READY_MARKER, 'ready\\n'); setInterval(() => undefined, 1000);"
      : "const { writeFileSync } = require('node:fs'); process.on('SIGTERM', () => process.exit(143)); writeFileSync(process.env.PROVIDER_READY_MARKER, 'ready\\n'); setInterval(() => undefined, 1000);",
  ], { detached, stdio: "ignore", env: providerEnvironment });
  provider.unref();
  writeFileSync(join(runDir, "provider.pid"), `${provider.pid}\n`);
  let providerStartedAt;
  try {
    providerStartedAt = execFileSync("/bin/ps", ["-o", "lstart=", "-p", String(provider.pid)], {
      encoding: "utf8",
    }).trim();
  } catch (error) {
    provider.kill("SIGKILL");
    throw error;
  }
  writeFileSync(join(runDir, "dispatch-provider.json"), JSON.stringify({
    run_token: process.env.PROVENANT_RUN_TOKEN,
    provider_pid: provider.pid,
    provider_pgid: detached ? provider.pid : process.pid,
    provider_started_at: providerStartedAt,
  }) + "\n");
};

const sleepUntilSignalled = ({ ignoreTerm = false } = {}) => {
  if (ignoreTerm) {
    process.on("SIGTERM", () => {
      writeFileSync(join(runDir, "term-ignored.marker"), "SIGTERM\n");
    });
    writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
    setInterval(() => undefined, 1000);
    return;
  }
  process.once("SIGTERM", () => {
    writeFileSync(join(runDir, "cancelled.marker"), "cancelled\n");
    process.exit(143);
  });
  writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
  setInterval(() => undefined, 1000);
};

const exitAfterOwnerRecord = ({ release = false } = {}) => {
  const ownerRecord = join(runDir, "dispatch-owner.json");
  const releasePath = join(runDir, "exit-owner.release");
  const providerReady = join(runDir, "provider-ready.marker");
  const waitForOwnerRecord = setInterval(() => {
    if (!existsSync(ownerRecord) || !existsSync(providerReady) || (release && !existsSync(releasePath))) return;
    clearInterval(waitForOwnerRecord);
    process.exit(0);
  }, 10);
};

if (owner === "run_controls.py") {
  // The cooperative canceller only ever reaches a run whose attempt directory
  // already exists, so it does nothing for the cold-start scenarios here.
  process.stdout.write('{"status":"cancelled"}\n');
  process.exit(0);
}

if (owner === "dispatch_run.py") {
  const taskId = value("--task-id");
  const prompt = readFileSync(value("--prompt-file"), "utf8");
  if (prompt === "sleep without provider") {
    sleepUntilSignalled();
  } else if (prompt === "sleep with provider") {
    mkdirSync(join(runDir, "dispatch", "tasks", taskId, "attempt-001"), { recursive: true });
    startProvider();
    sleepUntilSignalled();
  } else if (prompt === "ignore SIGTERM") {
    mkdirSync(join(runDir, "dispatch", "tasks", taskId, "attempt-001"), { recursive: true });
    startProvider();
    sleepUntilSignalled({ ignoreTerm: true });
  } else if (prompt === "exit with provider") {
    mkdirSync(join(runDir, "dispatch", "tasks", taskId, "attempt-001"), { recursive: true });
    startProvider();
    // Let Fabric persist the owner record before this owner disappears.
    exitAfterOwnerRecord();
  } else if (prompt === "exit with resistant provider") {
    mkdirSync(join(runDir, "dispatch", "tasks", taskId, "attempt-001"), { recursive: true });
    startProvider({ ignoreTerm: true, detached: true });
    exitAfterOwnerRecord({ release: true });
  } else if (prompt === "emit empty provider result") {
    const attemptDir = join(runDir, "dispatch", "tasks", taskId, "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    const resultPath = join(attemptDir, "result.md");
    const stderrPath = join(attemptDir, "stderr.log");
    const attemptPath = join(attemptDir, "attempt.json");
    writeFileSync(resultPath, "");
    writeFileSync(stderrPath, "");
    const record = {
      schema_version: 1,
      record_type: "dispatch-attempt",
      status: "succeeded",
      outcome: "ok",
      task_id: taskId,
      attempt_id: "attempt-001",
      attempt_path: relative(runDir, attemptPath),
      result: { path: relative(runDir, resultPath) },
      stderr: { path: relative(runDir, stderrPath) },
      route: {
        adapter: value("--adapter"),
        provider_family: value("--adapter"),
        resolved_model: value("--model") ?? value("--alias"),
        execution_intent: "ordinary",
      },
    };
    writeFileSync(attemptPath, JSON.stringify(record, null, 2) + "\n");
    process.stdout.write(JSON.stringify(record) + "\n");
    process.exit(0);
  } else if (prompt === "sleep before the attempt directory") {
    // Deliberately no attempt directory: this is the cold-start shape, where
    // the cooperative canceller has nothing to act on.
    startProvider();
    sleepUntilSignalled();
  } else {
    const attemptDir = join(runDir, "dispatch", "tasks", taskId, "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    const attemptPath = join(attemptDir, "attempt.json");
    const resultPath = join(attemptDir, "result.md");
    const stderrPath = join(attemptDir, "stderr.log");
    writeFileSync(resultPath, `fixture result for: ${prompt}`);
    writeFileSync(stderrPath, "");
    const record = {
      schema_version: 1,
      record_type: "dispatch-attempt",
      status: "succeeded",
      outcome: "ok",
      task_id: taskId,
      attempt_id: "attempt-001",
      attempt_path: relative(runDir, attemptPath),
      result: { path: relative(runDir, resultPath) },
      stderr: { path: relative(runDir, stderrPath) },
      route: {
        adapter: value("--adapter"),
        provider_family: value("--adapter"),
        resolved_model: value("--model") ?? value("--alias"),
        execution_intent: "ordinary",
      },
    };
    writeFileSync(attemptPath, JSON.stringify(record, null, 2) + "\n");
    process.stdout.write(JSON.stringify(record) + "\n");
    process.exit(0);
  }
} else if (owner === "batch_run.py") {
  const manifest = JSON.parse(readFileSync(value("--manifest"), "utf8"));
  if (manifest.tasks[0]?.prompt === "sleep with provider") {
    mkdirSync(join(runDir, "dispatch", "batches", "batch-001"), { recursive: true });
    startProvider();
    sleepUntilSignalled();
  } else if (manifest.tasks[0]?.prompt === "empty batch") {
    const taskId = manifest.tasks[0].id;
    const dir = join(runDir, "dispatch", "tasks", taskId, "attempt-001");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "result.md"), "");
    writeFileSync(join(dir, "attempt.json"), "{}");
    const summary = join(runDir, "summary.json");
    writeFileSync(summary, "{}");
    process.stdout.write(JSON.stringify({ schema_version: 1, record_type: "dispatch-batch", status: "completed",
      batch_id: "batch-001", task_count: 1, concurrency: 1, counts: { succeeded: 1 }, summary_path: "summary.json",
      tasks: [{ task_id: taskId, status: "succeeded", outcome: "ok", attempt_path: relative(runDir, join(dir, "attempt.json")),
        result_path: relative(runDir, join(dir, "result.md")), route: { adapter: "codex", provider_family: "openai", resolved_model: "luna", execution_intent: "ordinary" } }] }) + "\n");
    process.exit(0);
  } else {
    process.stdout.write(JSON.stringify({ schema_version: 1, status: "failed", message: "fixture" }) + "\n");
    process.exit(1);
  }
} else {
  throw new Error(`unexpected fixture owner: ${owner}`);
}
