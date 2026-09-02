#!/usr/bin/env node
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, relative } from "node:path";

const value = (flag) => {
  const index = process.argv.indexOf(flag);
  return index < 0 ? undefined : process.argv[index + 1];
};

const owner = process.env.PROVENANT_FIXTURE_OWNER ?? basename(process.argv[1]);
const waitForRelease = (runDir, callback) => {
  const readyPath = join(runDir, "delayed-ready");
  const releasePath = join(runDir, "delayed-release");
  writeFileSync(readyPath, "ready\n");
  const wait = () => {
    try {
      readFileSync(releasePath);
      callback();
    } catch {
      setTimeout(wait, 10);
    }
  };
  wait();
};

if (owner === "run_dir_init.sh") {
  const runDir = process.argv[2];
  mkdirSync(join(runDir, "findings"), { recursive: true });
  mkdirSync(join(runDir, "crossfamily"), { recursive: true });
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
writeFileSync(join(runDir, `${owner}.invocation.json`), JSON.stringify({
  argv: process.argv.slice(2),
  cwd: process.cwd(),
}, null, 2) + "\n");

if (owner === "run_controls.py") {
  const sleepingPid = join(runDir, "sleeping.pid");
  const cancelledMarker = join(runDir, "cancelled.marker");
  const cancelWhenReady = () => {
    try {
      process.kill(Number(readFileSync(sleepingPid, "utf8")), "SIGTERM");
    } catch {
      setTimeout(cancelWhenReady, 10);
      return;
    }
    const finishWhenReady = () => {
      try {
        readFileSync(cancelledMarker, "utf8");
      } catch {
        setTimeout(finishWhenReady, 10);
        return;
      }
      process.stdout.write('{"status":"cancelled"}\n');
    };
    finishWhenReady();
  };
  cancelWhenReady();
} else if (owner === "dispatch_run.py") {
  const taskId = value("--task-id");
  const promptPath = value("--prompt-file");
  const prompt = readFileSync(promptPath, "utf8");
  if (prompt === "emit malformed owner output") {
    process.stdout.write("{}\n");
  } else if (prompt === "emit typed running") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      record_type: "dispatch-attempt",
      status: "running",
    }) + "\n");
  } else if (prompt === "emit untyped running") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "running",
      message: "owner already exited",
    }) + "\n");
  } else if (prompt === "emit untyped cancelled") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "cancelled",
      message: "no retained attempt",
    }) + "\n");
  } else if (prompt === "emit untyped preflight error") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "adapter_unavailable",
      message: "provider adapter is unavailable",
    }) + "\n");
  } else if (prompt === "emit untyped success") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "succeeded",
      message: "not an attempt",
    }) + "\n");
  } else if (prompt === "emit incomplete success") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      record_type: "dispatch-attempt",
      status: "succeeded",
    }) + "\n");
  } else if (prompt === "emit nonexistent success") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      record_type: "dispatch-attempt",
      status: "succeeded",
      outcome: "ok",
      task_id: taskId,
      attempt_id: "attempt-001",
      attempt_path: `dispatch/tasks/${taskId}/attempt-001/attempt.json`,
      result: { path: `dispatch/tasks/${taskId}/attempt-001/result.md` },
      stderr: { path: `dispatch/tasks/${taskId}/attempt-001/stderr.log` },
      route: {
        adapter: "codex",
        provider_family: "openai",
        resolved_model: "gpt-fixture",
        execution_intent: "ordinary",
      },
    }) + "\n");
  } else if (prompt.startsWith("delayed dispatch ")) {
    const kind = prompt.slice("delayed dispatch ".length);
    waitForRelease(runDir, () => {
      const attemptDir = join(runDir, "dispatch", "tasks", taskId, "attempt-001");
      mkdirSync(attemptDir, { recursive: true });
      const attemptPath = join(attemptDir, "attempt.json");
      const record = {
        schema_version: 1,
        record_type: "dispatch-attempt",
        status: kind === "success" ? "succeeded" : kind === "timeout" ? "timed_out" : "failed",
        outcome: `delayed dispatch ${kind}`,
        task_id: taskId,
        attempt_id: "attempt-001",
        attempt_path: relative(runDir, attemptPath),
      };
      if (kind === "success") {
        const resultPath = join(attemptDir, "result.md");
        const stderrPath = join(attemptDir, "stderr.log");
        writeFileSync(resultPath, "delayed dispatch success content\n");
        writeFileSync(stderrPath, "");
        record.result = { path: relative(runDir, resultPath) };
        record.stderr = { path: relative(runDir, stderrPath) };
        record.route = {
          adapter: value("--adapter"),
          provider_family: value("--adapter"),
          resolved_model: value("--alias"),
          execution_intent: "ordinary",
        };
      }
      writeFileSync(attemptPath, JSON.stringify(record, null, 2) + "\n");
      process.stdout.write(JSON.stringify(record) + "\n");
      process.exit(0);
    });
  } else if (prompt === "sleep with provider until cancelled") {
    // A provider child in the owner's process group, answering only its own
    // SIGTERM: it survives anything that signals the owner pid alone.
    const provider = spawn(process.execPath, [
      "-e",
      "process.on('SIGTERM', () => process.exit(143)); setInterval(() => undefined, 1000);",
    ], { stdio: "ignore" });
    provider.unref();
    process.once("SIGTERM", () => {
      writeFileSync(join(runDir, "cancelled.marker"), "cancelled\n");
      process.exit(143);
    });
    mkdirSync(join(runDir, "dispatch", "tasks", taskId, "attempt-001"), { recursive: true });
    writeFileSync(join(runDir, "provider.pid"), `${provider.pid}\n`);
    writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
    setInterval(() => undefined, 1000);
  } else if (prompt === "sleep until cancelled") {
    process.once("SIGTERM", () => {
      writeFileSync(join(runDir, "cancelled.marker"), "cancelled\n");
      process.exit(143);
    });
    const attemptDir = join(runDir, "dispatch", "tasks", taskId, "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
    setInterval(() => undefined, 1000);
  } else {
    const attemptDir = join(runDir, "dispatch", "tasks", taskId, "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    const resultPath = join(attemptDir, "result.md");
    const stderrPath = join(attemptDir, "stderr.log");
    const attemptPath = join(attemptDir, "attempt.json");
    writeFileSync(resultPath, `fixture result for: ${prompt}`);
    writeFileSync(stderrPath, "");
    const route = {
      adapter: value("--adapter"),
      provider_family: value("--adapter"),
      model_family: "fixture",
      resolved_model: value("--alias"),
      endpoint_provider: value("--adapter"),
      execution_intent: "ordinary",
    };
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
      route,
    };
    if (prompt === "emit incomplete route success") record.route = { adapter: "codex" };
    writeFileSync(attemptPath, JSON.stringify(record, null, 2) + "\n");
    process.stdout.write(JSON.stringify(record) + "\n");
    if (prompt === "claim success then fail") process.exitCode = 1;
  }
} else if (owner === "batch_run.py") {
  const manifest = JSON.parse(readFileSync(value("--manifest"), "utf8"));
  const batchPrompt = manifest.tasks[0]?.prompt;
  if (batchPrompt?.startsWith("delayed batch ")) {
    const kind = batchPrompt.slice("delayed batch ".length);
    waitForRelease(runDir, () => {
      const task = manifest.tasks[0];
      const batchDir = join(runDir, "dispatch", "batches", "batch-001");
      const attemptDir = join(runDir, "dispatch", "tasks", task.id, "attempt-001");
      mkdirSync(attemptDir, { recursive: true });
      mkdirSync(batchDir, { recursive: true });
      const attemptPath = join(attemptDir, "attempt.json");
      const taskStatus = kind === "success" ? "succeeded" : kind === "timeout" ? "timed_out" : "failed";
      const taskRecord = {
        task_id: task.id,
        status: taskStatus,
        outcome: `delayed batch ${kind}`,
        attempt_path: relative(runDir, attemptPath),
      };
      if (kind === "success") {
        const resultPath = join(attemptDir, "result.md");
        writeFileSync(resultPath, "delayed batch success content\n");
        taskRecord.result_path = relative(runDir, resultPath);
        taskRecord.route = {
          adapter: task.adapter,
          provider_family: task.adapter,
          resolved_model: task.alias,
          execution_intent: "ordinary",
        };
      }
      writeFileSync(attemptPath, JSON.stringify({ status: taskStatus }) + "\n");
      const summaryPath = join(batchDir, "summary.json");
      const summary = {
        schema_version: 1,
        record_type: "dispatch-batch",
        status: kind === "success" ? "completed" : "failed",
        batch_id: "batch-001",
        task_count: 1,
        concurrency: 1,
        counts: { [taskStatus]: 1 },
        tasks: [taskRecord],
        summary_path: relative(runDir, summaryPath),
      };
      writeFileSync(summaryPath, JSON.stringify(summary, null, 2) + "\n");
      process.stdout.write(JSON.stringify(summary) + "\n");
      process.exit(0);
    });
  } else if (batchPrompt === "sleep until cancelled batch") {
    const batchDir = join(runDir, "dispatch", "batches", "batch-001");
    mkdirSync(batchDir, { recursive: true });
    writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
    process.once("SIGTERM", () => {
      writeFileSync(join(runDir, "cancelled.marker"), "cancelled\n");
      process.exit(143);
    });
    setInterval(() => undefined, 1000);
  } else if (batchPrompt === "emit incomplete completed batch") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      record_type: "dispatch-batch",
      status: "completed",
    }) + "\n");
    process.exit(0);
  } else if (batchPrompt === "emit typed running batch") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      record_type: "dispatch-batch",
      status: "running",
    }) + "\n");
    process.exit(0);
  } else if (batchPrompt === "emit untyped running batch") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "running",
      message: "owner already exited",
    }) + "\n");
    process.exit(0);
  } else if (batchPrompt === "emit untyped cancelled batch") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "cancelled",
      message: "no retained batch",
    }) + "\n");
    process.exit(0);
  } else if (batchPrompt === "emit untyped batch preflight error") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      status: "invalid_manifest",
      message: "task manifest is invalid",
    }) + "\n");
    process.exit(2);
  } else if (batchPrompt === "emit nonexistent completed batch") {
    process.stdout.write(JSON.stringify({
      schema_version: 1,
      record_type: "dispatch-batch",
      status: "completed",
      batch_id: "batch-001",
      task_count: 1,
      concurrency: 1,
      counts: { succeeded: 1 },
      tasks: [{ task_id: "missing", status: "succeeded" }],
      summary_path: "dispatch/batches/batch-001/summary.json",
    }) + "\n");
    process.exit(0);
  } else {
  const batchDir = join(runDir, "dispatch", "batches", "batch-001");
  mkdirSync(batchDir, { recursive: true });
  const tasks = manifest.tasks.map((task) => {
    const attemptDir = join(runDir, "dispatch", "tasks", task.id, "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    const attemptPath = join(attemptDir, "attempt.json");
    const resultPath = join(attemptDir, "result.md");
    writeFileSync(attemptPath, "{}\n");
    writeFileSync(resultPath, `fixture result for: ${task.prompt}`);
    return {
      task_id: task.id,
      status: "succeeded",
      outcome: "ok",
      attempt_path: relative(runDir, attemptPath),
      result_path: relative(runDir, resultPath),
      route: {
        adapter: task.adapter,
        provider_family: task.adapter,
        resolved_model: task.alias,
        execution_intent: "ordinary",
      },
    };
  });
  const summaryPath = join(batchDir, "summary.json");
  const summary = {
    schema_version: 1,
    record_type: "dispatch-batch",
    status: "completed",
    batch_id: "batch-001",
    task_count: tasks.length,
    concurrency: Number(value("--concurrency")),
    counts: { succeeded: tasks.length },
    tasks,
    summary_path: relative(runDir, summaryPath),
  };
  writeFileSync(summaryPath, JSON.stringify(summary, null, 2) + "\n");
  process.stdout.write(JSON.stringify(summary) + "\n");
  if (manifest.tasks[0]?.prompt === "claim batch success then fail") process.exitCode = 7;
  }
} else {
  throw new Error(`unexpected fixture owner: ${owner}`);
}
