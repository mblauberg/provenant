import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
const args = process.argv.slice(2),
  value = (key) => args[args.indexOf(key) + 1];
const owner = process.env.PROVENANT_FIXTURE_OWNER;
if (args.includes("--cwd")) {
  console.error("unrecognized --cwd");
  process.exit(2);
}
if (args.includes("--preflight-json")) {
  let text = "";
  for await (const chunk of process.stdin) text += chunk;
  const { tasks } = JSON.parse(text);
  console.log(
    JSON.stringify({
      status: "ready",
      routes: tasks.map((t) => ({
        adapter: t.adapter ?? "codex",
        resolved_model: t.model ?? "fixture",
        provider_family: "openai",
        execution_intent: "ordinary",
      })),
    }),
  );
  process.exit();
}
if (owner === "run_dir_init.sh") {
  writeFileSync(join(args[0], "RUN_RECEIPT.json"), JSON.stringify({ status: "active" }));
  process.exit();
}
const dir = value("--run-dir");
if (owner === "run_controls.py") {
  writeFileSync(join(dir, "cancel"), "");
  process.exit();
}
if (owner === "batch_run.py") {
  const { tasks } = JSON.parse(readFileSync(value("--manifest"), "utf8"));
  const rows = [];
  for (const task of tasks) {
    const prompt = join(dir, "_owner", `${task.id}.md`);
    writeFileSync(prompt, task.prompt ?? readFileSync(task.prompt_file, "utf8"));
    const child = spawnSync(
      process.execPath,
      [
        import.meta.filename,
        "--run-dir",
        dir,
        "--task-id",
        task.id,
        "--prompt-file",
        prompt,
        "--timeout",
        String(task.timeout ?? 3600),
      ],
      { env: { ...process.env, PROVENANT_FIXTURE_OWNER: "dispatch_run.py" }, encoding: "utf8" },
    );
    if (child.status !== 0) process.exit(child.status ?? 1);
    rows.push(JSON.parse(child.stdout));
  }
  const summary = join(dir, "dispatch/batches/batch-001");
  mkdirSync(summary, { recursive: true });
  writeFileSync(join(summary, "summary.json"), JSON.stringify({ status: "completed", tasks: rows }));
  writeFileSync(join(dir, "RUN_RECEIPT.json"), JSON.stringify({ status: "ok", attempts: rows }));
  console.log(JSON.stringify({ schema: "fabric.status.v1", runs: rows }));
  process.exit();
}
let task = value("--task-id"),
  attempt = 1;
if (args.includes("--resume")) {
  task = readdirSync(join(dir, "tasks"))[0];
  attempt = readdirSync(join(dir, "tasks", task)).length + 1;
}
const prompt = readFileSync(value("--prompt-file"), "utf8");
if (prompt === "crash-before-attempt") process.exit(2);
if (prompt === "pause-before-attempt") await new Promise((r) => setTimeout(r, 500));
const path = join(dir, "tasks", task, `attempt-${String(attempt).padStart(3, "0")}`);
mkdirSync(path, { recursive: true });
const run_id = process.env.PROVENANT_RUN_ID;
const row = {
  schema: "fabric.attempt.v1",
  run_id,
  task_id: task,
  attempt,
  state: "running",
  status: null,
  cwd: process.cwd(),
  worktree: process.cwd(),
  started_at: new Date().toISOString(),
  ended_at: null,
  evidence: { timeout: Number(value("--timeout")) },
  applied: {
    sandbox: value("--sandbox") ?? "read-only",
    network: value("--network") === "false" ? false : true,
    add_dirs: [],
  },
  provenance: { line: "Route: codex/fixture@high (openai; observed)" },
  paths: {
    result: join(path, "result.md"),
    stderr: join(path, "stderr.log"),
    events: join(path, "events.jsonl"),
    receipt: join(path, "attempt.json"),
  },
  digest: `running ${run_id} codex/fixture@high`,
};
const write = () => writeFileSync(join(path, "attempt.json"), JSON.stringify(row));
write();
writeFileSync(join(path, "stderr.log"), "fixture stderr");
writeFileSync(join(path, "events.jsonl"), "{}\n");
if (prompt === "slow") {
  while (true) {
    try {
      readFileSync(join(dir, "cancel"));
      row.status = "cancelled";
      break;
    } catch {}
    await new Promise((r) => setTimeout(r, 20));
  }
}
row.state = "terminal";
row.status ??= prompt === "question" ? "input_required" : "ok";
row.ended_at = new Date().toISOString();
row.question = row.status === "input_required" ? "Which branch?" : null;
row.digest = `${row.status} ${run_id} codex/fixture@high · result ${row.paths.result}\n  ${row.provenance.line}`;
writeFileSync(join(path, "result.md"), "x".repeat(25000));
write();
writeFileSync(join(dir, "RUN_RECEIPT.json"), JSON.stringify({ status: row.status, attempts: [row] }));
console.log(JSON.stringify(row));
