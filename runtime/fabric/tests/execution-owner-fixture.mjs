#!/usr/bin/env node
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

if (owner === "dispatch_run.py") {
  const taskId = value("--task-id");
  const promptPath = value("--prompt-file");
  const prompt = readFileSync(promptPath, "utf8");
  if (prompt === "sleep until cancelled") {
    writeFileSync(join(runDir, "sleeping.pid"), `${process.pid}\n`);
    process.once("SIGTERM", () => {
      writeFileSync(join(runDir, "cancelled.marker"), "cancelled\n");
      process.exit(143);
    });
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
      resolved_model: value("--model") ?? value("--alias") ?? value("--task-class"),
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
    writeFileSync(attemptPath, JSON.stringify(record, null, 2) + "\n");
    process.stdout.write(JSON.stringify(record) + "\n");
  }
} else if (owner === "batch_run.py") {
  const manifest = JSON.parse(readFileSync(value("--manifest"), "utf8"));
  const batchDir = join(runDir, "dispatch", "batches", "batch-001");
  mkdirSync(batchDir, { recursive: true });
  const tasks = manifest.tasks.map((task) => ({
    task_id: task.id,
    status: "succeeded",
    outcome: "ok",
    route: {
      adapter: task.adapter,
      provider_family: task.adapter,
      resolved_model: task.model ?? task.alias ?? task.task_class,
      execution_intent: "ordinary",
    },
  }));
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
} else {
  throw new Error(`unexpected fixture owner: ${owner}`);
}
