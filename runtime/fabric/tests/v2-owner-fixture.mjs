import { spawnSync } from "node:child_process";
import { appendFileSync, mkdirSync, readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, isAbsolute } from "node:path";
const args = process.argv.slice(2),
  value = (key) => args.includes(key) ? args[args.indexOf(key) + 1] : undefined;
const pidLog = process.env.PROVENANT_FIXTURE_PID_LOG;
if (pidLog) {
  appendFileSync(pidLog, JSON.stringify({ pid: process.pid, event: "start" }) + "\n");
  process.on("exit", () => appendFileSync(pidLog, JSON.stringify({ pid: process.pid, event: "exit" }) + "\n"));
}
const deadlineMs = Number(process.env.PROVENANT_FIXTURE_DEADLINE_MS ?? 20000);
const deadline = setTimeout(() => process.exit(124), Number.isFinite(deadlineMs) && deadlineMs > 0 ? deadlineMs : 20000);
deadline.unref();
if (args.includes("--deadline-loop")) {
  while (true) await new Promise((resolve) => setTimeout(resolve, 20));
}
const owner = process.env.PROVENANT_FIXTURE_OWNER;
if (args.includes("--preflight-json")) {
  let text = "";
  for await (const chunk of process.stdin) text += chunk;
  const { tasks } = JSON.parse(text);
  const errors = tasks.filter((t) => t.prompt_file && (!isAbsolute(t.prompt_file) || !existsSync(t.prompt_file)))
    .map((t) => ({task_id:t.id,error:"prompt_unavailable",fix:"Pass an existing absolute prompt_file."}));
  if (tasks.some((t) => t.access_mode === "worktree_write" && !t.worktree))
    errors.push({task_id:tasks.find((t) => t.access_mode === "worktree_write" && !t.worktree).id,error:"worktree_required",fix:"Pass the registered writer worktree."});
  if (errors.length) { console.log(JSON.stringify({status:"rejected",...errors[0],errors})); process.exit(); }
  if (tasks.some((t) => "network" in t && typeof t.network !== "boolean")) {
    console.log(JSON.stringify({status:"rejected",error:"network_invalid",fix:"Pass network true or false."})); process.exit();
  }
  console.log(
    JSON.stringify({
      status: "ready",
      routes: tasks.map((t) => ({
        adapter: t.adapter ?? "codex",
        resolved_model: t.model ?? "fixture",
        provider_family: "openai",
        execution_intent: "ordinary",
        notes: t.alias && t.model ? ["alias and model both supplied; model won"] : [],
      })),
    }),
  );
  process.exit();
}
if (owner === "run_dir_init.sh") {
  if (args.includes("--owner-logs")) mkdirSync(join(args[0], "_owner"));
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
        ...["adapter", "alias", "model", "effort", "cwd", "context_ceiling"].flatMap((key) => task[key] === undefined ? [] : ["--" + key, String(task[key])]),
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
  task = value("--task-id") ?? readdirSync(join(dir, "tasks"))[0];
  attempt = readdirSync(join(dir, "tasks", task)).length + 1;
}
if (args.includes("--alias") && args.includes("--model")) {
  console.error("--alias and --model are mutually exclusive"); process.exit(2);
}
const prompt = readFileSync(value("--prompt-file"), "utf8");
writeFileSync(join(dir, "_owner", `${task}-args-${attempt}.json`), JSON.stringify(args));
writeFileSync(join(dir, "_owner", `${task}-env-${attempt}.json`), JSON.stringify({ chair: process.env.PROVENANT_CHAIR }));
writeFileSync(join(dir, "_owner", `${task}-prompt-${attempt}.md`), prompt);
if (prompt === "reject-before-attempt") {
  console.log(JSON.stringify({schema_version:1,status:"rejected",message:"dispatch a new run",fix:"dispatch a new run"})); process.exit(2);
}
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
  cwd: value("--cwd") ?? process.cwd(),
  mode: args.includes("--access-mode") ? value("--access-mode") : "read_only",
  worktree: args.includes("--worktree") ? value("--worktree") : null,
  started_at: new Date().toISOString(),
  ended_at: null,
  evidence: { owner_cwd: process.cwd(), prompt_file: value("--prompt-file"), timeout: Number(value("--timeout")) },
  applied: {
    sandbox: prompt === "no-controls" ? null : value("--sandbox") ?? "read-only",
    network: prompt === "no-controls" ? null : value("--network") === "false" ? false : true,
    add_dirs: [],
  },
  provenance: { requested: { adapter: value("--adapter"), model: value("--model"), alias: value("--alias"), effort: value("--effort") }, line: "Route: codex/fixture@high (openai; observed)" },
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
if (prompt === "stubborn") {
  // Ignores SIGTERM and never honours the cancel file: only SIGKILL ends it,
  // before any terminal row is written.
  process.on("SIGTERM", () => {});
  while (true) await new Promise((r) => setTimeout(r, 50));
}
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
