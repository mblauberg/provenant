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
  if (process.env.PROVENANT_FIXTURE_PREFLIGHT_LOG)
    appendFileSync(process.env.PROVENANT_FIXTURE_PREFLIGHT_LOG, JSON.stringify(tasks) + "\n");
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
        ...(task.capabilities?.length ? ["--capabilities", JSON.stringify(task.capabilities)] : []),
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
if (prompt === "pause-before-attempt") await new Promise((r) => setTimeout(r, 1000));
const attemptDir = (number) => join(dir, "tasks", task, `attempt-${String(number).padStart(3, "0")}`);
// A named session resumes a pinned attempt; otherwise the latest one.
const prior = attempt > 1
  ? JSON.parse(readFileSync(join(attemptDir(Number(value("--resume-attempt") ?? attempt - 1)), "attempt.json"), "utf8"))
  : {};
if (value("--require-session") !== undefined && prior.session_id !== value("--require-session")) {
  console.log(JSON.stringify({schema_version:1,status:"rejected",error:"continuation_unsupported",
    message:"no provider session; pass fresh: true"})); process.exit(2);
}
if (prompt === "fallback-slow" || prompt === "fallback-gap") {
  // A retryable first attempt, then the owner's fallback attempt in the same invocation.
  mkdirSync(attemptDir(attempt), { recursive: true });
  writeFileSync(join(attemptDir(attempt), "attempt.json"), JSON.stringify({
    schema: "fabric.attempt.v1", run_id: process.env.PROVENANT_RUN_ID, task_id: task, attempt, state: "terminal",
    status: "failed", retryable: true, session_id: null, provenance: { transport: "codex" },
    started_at: new Date().toISOString(), ended_at: new Date().toISOString(), paths: {},
  }));
  attempt += 1;
  // The owner is alive between attempts, with every attempt so far terminal.
  while (prompt === "fallback-gap" && !existsSync(join(dir, "release"))) await new Promise((r) => setTimeout(r, 20));
}
const path = attemptDir(attempt);
mkdirSync(path, { recursive: true });
const priorApplied = prior.applied ?? {};
const run_id = process.env.PROVENANT_RUN_ID;
const row = {
  schema: "fabric.attempt.v1",
  run_id,
  task_id: task,
  attempt,
  state: "running",
  status: null,
  cwd: value("--cwd") ?? process.cwd(),
  // A resume keeps the provider session; "lose-session" models a turn that recorded none.
  session_id: prompt === "lose-session" ? null : prompt === "fail-new-session" ? `other-${attempt}`
    : prior.session_id ?? `fixture-${process.env.PROVENANT_RUN_ID}-${task}`,
  mode: args.includes("--access-mode") ? value("--access-mode") : "read_only",
  worktree: args.includes("--worktree") ? value("--worktree") : null,
  started_at: new Date().toISOString(),
  ended_at: null,
  evidence: { owner_cwd: process.cwd(), prompt_file: value("--prompt-file"), timeout: Number(value("--timeout")) },
  applied: {
    sandbox: prompt === "no-controls" ? null : value("--sandbox") ?? "read-only",
    network: prompt === "no-controls" ? null : value("--network") === "false" ? false : true,
    add_dirs: [],
    capabilities: value("--capabilities") ? JSON.parse(value("--capabilities")) : priorApplied.capabilities ?? [],
  },
  provenance: { requested: { adapter: value("--adapter"), model: value("--model"), alias: value("--alias"), effort: value("--effort") }, transport: value("--adapter") ?? prior.provenance?.transport ?? "codex", line: "Route: codex/fixture@high (openai; observed)",
    ...(prompt.startsWith("fallback-") ? { fallback_from: { attempt: attempt - 1, status: "failed" } } : {}) },
  paths: {
    result: join(path, "result.md"),
    stderr: join(path, "stderr.log"),
    events: join(path, "events.jsonl"),
    receipt: join(path, "attempt.json"),
  },
  digest: `running ${run_id} codex/fixture@high`,
};
const readRoots = args.filter((_, index) => args[index - 1] === "--read-root");
if (readRoots.length) row.read_roots = readRoots;
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
if (prompt === "slow" || prompt === "fallback-slow") {
  while (true) {
    try {
      readFileSync(join(dir, "cancel"));
      row.status = "cancelled";
      break;
    } catch {}
    if (prompt === "fallback-slow" && existsSync(join(dir, "release"))) break;
    await new Promise((r) => setTimeout(r, 20));
  }
}
if (prompt === "no-conversation" && value("--require-session") !== undefined) {
  row.status = "rejected";
  row.error = "continuation_unsupported";
  row.fix = "The provider no longer has this session; pass fresh: true.";
}
row.state = "terminal";
row.status ??= prompt === "question" ? "input_required"
  : ["fail", "lose-session", "fail-new-session", "mark-fail"].includes(prompt) ? "failed" : "ok";
row.ended_at = new Date().toISOString();
row.question = row.status === "input_required" ? "Which branch?" : null;
row.digest = `${row.status} ${run_id} codex/fixture@high · result ${row.paths.result}\n  ${row.provenance.line}`;
// "mark" prompts end their result with its attempt, so a handoff shows which result it carried.
writeFileSync(join(path, "result.md"), "x".repeat(25000) + (prompt.startsWith("mark") ? `\nresult of ${run_id}/${task}#${attempt}\n` : ""));
write();
writeFileSync(join(dir, "RUN_RECEIPT.json"), JSON.stringify({ status: row.status, attempts: [row] }));
console.log(JSON.stringify(row));
