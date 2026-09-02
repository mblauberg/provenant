#!/usr/bin/env node
// A dispatch owner that behaves like the real one where run lifecycle is
// concerned: it spawns a provider child in its own process group, records both
// pids in the run directory, and stays alive until something signals it.
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, relative } from "node:path";

const value = (flag) => {
  const index = process.argv.indexOf(flag);
  return index < 0 ? undefined : process.argv[index + 1];
};

const owner = process.env.PROVENANT_FIXTURE_OWNER ?? basename(process.argv[1]);

if (owner === "run_dir_init.sh") {
  const runDir = process.argv[2];
  mkdirSync(join(runDir, "findings"), { recursive: true });
  mkdirSync(join(runDir, "traces"), { recursive: true });
  writeFileSync(join(runDir, "MANIFEST.md"), "# fixture manifest\n");
  writeFileSync(join(runDir, "RUN_RECEIPT.json"), JSON.stringify({
    schema_version: 1,
    status: "active",
  }) + "\n");
  process.stdout.write(`${runDir}\n`);
  process.exit(0);
}

const runDir = value("--run-dir");

/**
 * A provider child in the owner's process group. It answers only its own
 * SIGTERM, so it survives any cancellation that signals the owner alone.
 */
const startProvider = () => {
  const provider = spawn(process.execPath, [
    "-e",
    "process.on('SIGTERM', () => process.exit(143)); setInterval(() => undefined, 1000);",
  ], { stdio: "ignore" });
  provider.unref();
  writeFileSync(join(runDir, "provider.pid"), `${provider.pid}\n`);
};

const sleepUntilSignalled = () => {
  process.once("SIGTERM", () => {
    writeFileSync(join(runDir, "cancelled.marker"), "cancelled\n");
    process.exit(143);
  });
  writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
  setInterval(() => undefined, 1000);
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
  if (prompt === "sleep with provider") {
    mkdirSync(join(runDir, "dispatch", "tasks", taskId, "attempt-001"), { recursive: true });
    startProvider();
    sleepUntilSignalled();
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
        resolved_model: value("--alias"),
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
  } else {
    process.stdout.write(JSON.stringify({ schema_version: 1, status: "failed", message: "fixture" }) + "\n");
    process.exit(1);
  }
} else {
  throw new Error(`unexpected fixture owner: ${owner}`);
}
