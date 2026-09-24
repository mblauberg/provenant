/**
 * The same store from a shell, for agents that would rather run a command than
 * hold an MCP connection, and for the user watching what their agents are doing.
 *
 *   fabric whoami
 *   fabric send <to> <body...>
 *   fabric inbox [--peek] [--limit N]
 *   fabric ack <message-id> <claim-id>
 *   fabric tasks [state]
 *   fabric watch [--interval 2]
 */
import { digest } from "./surface.js";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { databasePath, identify, withoutGitRedirects } from "./identity.js";
import { dispatchConfiguredBatch, dispatchConfiguredProvider, type BatchInput, type DispatchInput } from "./execution.js";
import type { RouteInput } from "./execution-input.js";
import {
  statusRows, fabricStatus, findRecordedRun, listRecordedRuns, retentionHours, terminateRecordedRun,
} from "./run-registry.js";
import { inspectDatabase, Store } from "./store.js";
import { readEvents, readRuns } from "./run-reader.js";

const USAGE = `fabric <command>

  whoami                      who am I, and who else is in this project
  send <to> <body...>         to an agent id, a team id, or "all"
       [--reply-to <id>]      thread this onto an existing message
       [--kind <kind>]        note (default), request, response
       [--task-id <id>]       link an existing Fabric task
       [--output-path <path>]  opaque output or run path metadata
  inbox [--peek]              claim unacknowledged messages; --peek does not claim
        [--digest]            bounded unread counts and summaries
        [--limit N]           return at most N deliveries (default 20)
        [--claim-seconds N]   claim lifetime, 1 to 3600 seconds (default 300)
        [--task-id <id>]       return only deliveries linked to this task
  ack <message-id> <claim-id> acknowledge one claimed delivery
  note <text...>              record something in the activity log
  tasks [state]               list tasks, optionally filtered by state
  task <objective...>         open a task
  claim <task-id>             atomically claim an open, unowned task
  work-claims                 active work claims and landing lease
  landing-push <session> <generation> <branch> [--label <seat>]  verify lease and remote SHA, then push HEAD
  done <task-id>              close a task
  activity [--after-seq N]    list activity, optionally after a cursor
           [--limit N]
  watch [ids…] [--interval N] print run state changes; exit when all terminal
  lanes [--json] [id]         versioned run reader, root-relative paths
  events [--follow] [--until-idle]  JSON lines; follow stays open unless idle exit is requested
  status [id] [--wait-seconds N]  run status by task, batch or run directory; no id: store summary
  doctor [--json]             read-only schema and integrity diagnostics
  adapters [--json]           configured providers: dispatch state, aliases,
                              read-only guarantee, endpoint profiles
  dispatch list [--json]      configured-provider runs recorded in this workspace
  dispatch kill <run> [--json]  stop one recorded run and the group it leads
  dispatch --adapter A --model M --effort E --mode MODE --prompt-file F [--wait]
  dispatch --tasks F          run a JSON task manifest

Identity comes from the working directory and AGENT_FABRIC_LABEL (or
landing-push --label for that command). Registered
worktrees share one repository project while retaining their own cwd. There is
nothing to install, trust or provision.`;

const argv = process.argv.slice(2);
const command = argv[0] ?? "whoami";
const commands = new Set([
  "whoami", "send", "inbox", "ack", "note", "tasks", "task", "claim", "done",
  "activity", "watch", "status", "doctor", "dispatch", "adapters", "lanes", "events",
  "work-claims", "landing-push",
]);

if (command === "--help" || command === "-h" || command === "help") {
  console.log(USAGE);
  process.exit(0);
}
if (!commands.has(command)) {
  console.error(`unknown command "${command}"\n\n${USAGE}`);
  process.exit(2);
}

/** Read `--flag value` out of argv, so the remaining words form the body. */
const flag = (name: string): string | undefined => {
  const at = argv.indexOf(`--${name}`);
  if (at === -1) return undefined;
  const value = argv[at + 1];
  if (value === undefined || value.startsWith("--")) {
    throw new Error(`--${name} requires a value`);
  }
  argv.splice(at, 2);
  return value;
};
let landingLabel: string | undefined;
try {
  landingLabel = command === "landing-push" ? flag("label") : undefined;
} catch (error) {
  console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
  process.exit(2);
}
const who = identify(landingLabel === undefined ? process.env : { ...process.env, AGENT_FABRIC_LABEL: landingLabel });
const executionIdentity = () => {
  const store = new Store(databasePath());
  try {
    store.announce(who);
    return { ...who, registeredProjects: store.projects() };
  } finally {
    store.close();
  }
};
if (command === "lanes") {
  const rest = argv.slice(1).filter((value) => value !== "--json");
  if (rest.length > 1 || rest.some((value) => value.startsWith("--"))) {
    console.error("fabric: usage: fabric lanes [--json] [id]");
    process.exit(2);
  }
  const result = await readRuns(who.cwd, rest.length ? rest : undefined);
  console.log(JSON.stringify(result, null, 2));
  process.exit(result.status === "ok" ? 0 : 1);
}
if (command === "status") {
  try {
    const wait = flag("wait-seconds");
    const rest = argv.slice(1).filter((value) => value !== "--json" && value !== "--runs");
    if (rest.length > 1 || rest.some((value) => value.startsWith("--"))) {
      throw new Error("usage: fabric status [id] [--wait-seconds N] [--json]");
    }
    if (rest[0] !== undefined || argv.includes("--runs") || wait !== undefined) {
      console.log(JSON.stringify(await fabricStatus(who.cwd, rest[0], wait === undefined ? 0 : Number(wait)), null, 2));
      process.exit(0);
    }
  } catch (error) {
    console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
    process.exit(2);
  }
}
/**
 * Dispatch runs are recorded on disk, not in the store, so these read and act
 * from a cold start: a run started by an MCP host that has since died is still
 * listed and still killable here.
 */
if (command === "dispatch") {
  const json = argv.includes("--json");
  const rest = argv.slice(1).filter((argument) => argument !== "--json");
  const subcommand = rest[0];
  if (subcommand === "list") {
    if (rest.length !== 1) {
      console.error("fabric: usage: fabric dispatch list [--json]");
      process.exit(2);
    }
    const runs = listRecordedRuns(who.cwd);
    if (json) {
      console.log(JSON.stringify({
        workspace: who.cwd,
        retention_hours: retentionHours(process.env),
        runs,
      }, null, 2));
    } else if (runs.length === 0) {
      console.log("no recorded dispatch runs");
    } else {
      for (const run of runs) {
        const state = run.orphaned ? "orphaned" : run.running ? "running" : "exited";
        console.log(`${run.run_id.padEnd(12)} ${state.padEnd(9)} pid ${String(run.owner_pid).padEnd(8)} ` +
          `${run.kind} ${run.task_id ?? run.batch_id ?? ""}`);
      }
    }
    process.exit(0);
  }
  if (subcommand === "kill") {
    const reference = rest[1];
    if (rest.length !== 2 || reference === undefined) {
      console.error("fabric: usage: fabric dispatch kill <run-id|run-dir> [--json]");
      process.exit(2);
    }
    const run = findRecordedRun(who.cwd, reference);
    if (run === undefined) {
      console.error(`fabric: no recorded dispatch run: ${reference}`);
      process.exit(1);
    }
    const outcome = await terminateRecordedRun(run);
    const state = outcome.reason === "still running"
      ? outcome.reason
      : outcome.signalled ? "signalled" : "not running";
    const output = json ? JSON.stringify(outcome, null, 2)
      : `${run.run_id} ${state}${outcome.escalated ? " (escalated to SIGKILL)" : ""}` +
        `${outcome.reason !== undefined && outcome.reason !== state ? ` (${outcome.reason})` : ""}`;
    await new Promise<void>((resolveWrite) => process.stdout.write(`${output}\n`, "utf8", () => resolveWrite()));
    process.exit(outcome.signalled && outcome.reason !== "still running" ? 0 : 1);
  }
  try {
    const options = subcommand?.startsWith("--") ? rest : rest.slice(1);
    const values = new Map<string, string>();
    const switches = new Set<string>();
    const allowed = new Set(["--adapter", "--alias", "--model", "--effort", "--mode", "--worktree", "--cwd", "--prompt-file", "--id", "--tasks"]);
    for (let index = 0; index < options.length; index += 1) {
      const option = options[index]!;
      if (option === "--wait") {
        if (switches.has(option)) throw new Error(`${option} may be passed once`);
        switches.add(option);
        continue;
      }
      if (!option.startsWith("--")) throw new Error(`unexpected argument: ${option}`);
      if (!allowed.has(option)) throw new Error(`unknown option ${option}; choose ${[...allowed].join(", ")} or --wait`);
      const value = options[++index];
      if (value === undefined || value.startsWith("--")) throw new Error(`${option} requires a value`);
      if (values.has(option)) throw new Error(`${option} may be passed once`);
      values.set(option, value);
    }
    const read = (name: string): string | undefined => values.get(`--${name}`);
    if (subcommand !== undefined && !subcommand.startsWith("--"))
      throw new Error(`unknown dispatch subcommand: ${subcommand}`);
    const route: RouteInput = {
      ...(read("adapter") === undefined ? {} : { adapter: read("adapter") }),
      ...(read("alias") === undefined ? {} : { alias: read("alias") }),
      ...(read("model") === undefined ? {} : { model: read("model") }),
      ...(read("effort") === undefined ? {} : { effort: read("effort") }),
      ...(read("mode") === undefined ? {} : { mode: read("mode") as RouteInput["mode"] }),
      ...(read("worktree") === undefined ? {} : { worktree: read("worktree") }),
      ...(read("cwd") === undefined ? {} : { cwd: read("cwd") }),
    };
    const tasksFile = read("tasks");
    let result: Record<string, unknown>;
    if (tasksFile !== undefined) {
      if (["prompt-file", "id"].some((key) => read(key) !== undefined))
        throw new Error("--tasks cannot be combined with prompt or id flags");
      const document = JSON.parse(readFileSync(resolve(who.cwd, tasksFile), "utf8")) as unknown;
      const batch = (Array.isArray(document) ? { tasks: document } : document) as BatchInput;
      if (!Array.isArray(batch.tasks)) throw new Error("--tasks file must contain a tasks array");
      result = await dispatchConfiguredBatch({ ...route, ...batch, wait_seconds: switches.has("--wait") ? 55 : 0 }, executionIdentity(), new AbortController().signal);
    } else {
      const input: DispatchInput = {
        ...route,
        prompt_file: read("prompt-file"),
        wait_seconds: switches.has("--wait") ? 55 : 0,
        ...(read("id") === undefined ? {} : { task_id: read("id") }),
        ...(read("worktree") === undefined ? {} : { worktree: read("worktree") }),
        ...(read("cwd") === undefined ? {} : { cwd: read("cwd") }),
      };
      result = await dispatchConfiguredProvider(input, executionIdentity(), new AbortController().signal);
    }
    console.log(String(result.id ?? "unassigned"));
    const taskRows = Array.isArray(result.tasks)
      ? result.tasks.filter((item): item is Record<string, unknown> =>
        item !== null && typeof item === "object" && (item as Record<string, unknown>).status === "rejected")
      : [];
    const rejectedDetails = result.status === "rejected"
      ? ` error: ${String(result.error ?? "rejected").replace(/\s+/gu, " ")} fix: ${String(result.fix ?? "Check the input and try again.").replace(/\s+/gu, " ")}`
      : "";
    const rejectedTaskDetails = taskRows.length > 0
      ? ` tasks: ${taskRows.map((error) =>
        `${String(error.task_id ?? "task")}: ${String(error.error ?? "rejected")} (${String(error.fix ?? "check input")})`)
        .join("; ").replace(/\s+/gu, " ")}`
      : "";
    console.log(`status: ${String(result.status ?? "unknown")}${rejectedDetails}${rejectedTaskDetails}`);
    process.exit(result.status === "rejected" ? 1 : 0);
  } catch (error) {
    console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
    process.exit(2);
  }
}
if (command === "adapters") {
  const unknown = argv.slice(1).filter((argument) => argument !== "--json");
  if (unknown.length > 0) {
    console.error("fabric: usage: fabric adapters [--json]");
    process.exit(2);
  }
  const { catalogueSnapshot } = await import("./catalogue.js");
  const snapshot = catalogueSnapshot();
  if (argv.includes("--json")) {
    console.log(JSON.stringify(snapshot, null, 2));
  } else if (snapshot.adapters.length === 0) {
    console.error("fabric: adapter catalogue unavailable (no product checkout found)");
    process.exit(1);
  } else {
    for (const adapter of snapshot.adapters) {
      const alias = Object.keys(adapter.aliases).sort().join("/") || "-";
      const modes = ["read_only", ...(adapter.write_modes ?? [])].join(",");
      const guarantee = adapter.read_only_guarantee ?? "-";
      const line = `${adapter.name.padEnd(9)} ${adapter.dispatch.padEnd(12)} guarantee=${guarantee.padEnd(12)} modes=${modes.padEnd(21)} aliases=${alias}`;
      console.log(adapter.disabled_reason !== undefined && adapter.dispatch !== "implemented"
        ? `${line}  (${adapter.disabled_reason})`
        : line);
    }
    const endpoints = Object.keys(snapshot.endpoints);
    if (endpoints.length > 0) {
      console.log(`endpoint profiles: ${endpoints.join(", ")}`);
    }
  }
  process.exit(0);
}
if (command === "status" || command === "doctor") {
  const unknown = argv.slice(1).filter((argument) => argument !== "--json");
  if (unknown.length > 0) {
    console.error(`fabric: usage: fabric ${command} [--json]`);
    process.exit(2);
  }
  const result = inspectDatabase(databasePath(), who.project, command);
  console.log(JSON.stringify(result, null, 2));
  process.exit(result.status === "error" ? 1 : 0);
}
let store: Store | undefined;

const show = (value: unknown): void => {
  console.log(JSON.stringify(value, null, 2));
};

const sleep = async (ms: number): Promise<void> =>
  await new Promise((done) => setTimeout(done, ms));

const positiveNumber = (value: string | undefined, fallback: number, name: string): number => {
  if (value === undefined) return fallback;
  const number = Number(value);
  if (!Number.isFinite(number) || number <= 0) throw new Error(`${name} must be positive`);
  return number;
};

const printActivity = (rows: ReturnType<Store["activity"]>): void => {
  for (const row of rows) {
    console.log(
      `${row.at}  #${String(row.seq).padEnd(8)} ${row.agentId.padEnd(12)} ` +
        `${row.kind.padEnd(6)} ${row.detail}`,
    );
  }
};

try {
  store = new Store(databasePath());
  if (command !== "landing-push") store.announce(who);
  switch (command) {
  case "whoami":
    if (argv.length !== 1) throw new Error("usage: fabric whoami");
    show({ ...who, database: databasePath(), agents: store.agents(who.project) });
    break;

  case "work-claims":
    if (argv.length !== 1) throw new Error("usage: fabric work-claims");
    show({ work_claims: store.workClaims(who.project), landing_lease: store.landingLease(who.project) });
    break;

  case "landing-push": {
    const session = argv[1], generation = Number(argv[2]), branch = argv[3];
    if (argv.length !== 4 || !session || !Number.isSafeInteger(generation) || generation < 1 ||
      !branch || !/^[A-Za-z0-9][A-Za-z0-9._/-]*$/u.test(branch) || branch.includes("..") || branch.endsWith("/"))
      throw new Error("usage: fabric landing-push <session> <generation> <branch> [--label <seat>]");
    const lease = store.landingLease(who.project);
    if (!lease) throw new Error("stale landing lease");
    store.verifyLanding(who, session, generation, lease.expectedSha);
    const gitOptions = { cwd: who.cwd, env: withoutGitRedirects(process.env) };
    const remote = execFileSync("git", ["ls-remote", "--heads", "origin", `refs/heads/${branch}`],
      { ...gitOptions, encoding: "utf8" }).trim().split(/\s+/u)[0];
    if (remote !== lease.expectedSha) throw new Error("remote integration SHA changed; acquire a new landing lease");
    const head = execFileSync("git", ["rev-parse", "HEAD"], { ...gitOptions, encoding: "utf8" }).trim();
    execFileSync("git", ["merge-base", "--is-ancestor", remote, head], gitOptions);
    const pushed = store.withLandingPush(who, session, generation, remote, () =>
      execFileSync("git", ["push", `--force-with-lease=refs/heads/${branch}:${remote}`,
        "origin", `${head}:refs/heads/${branch}`], { ...gitOptions, stdio: "inherit", timeout: 120_000 }));
    show({ pushed: branch, previous_sha: remote, generation,
      ...(pushed.releaseWarning ? { release_warning: pushed.releaseWarning } : {}) });
    break;
  }

  case "send": {
    // Strip the flags first so whatever is left is the recipient and the body.
    const replyTo = flag("reply-to");
    const kind = flag("kind");
    const taskId = flag("task-id");
    const outputPath = flag("output-path");
    const to = argv[1];
    const body = argv.slice(2).join(" ");
    if (to === undefined || body.length === 0) {
      throw new Error("usage: fabric send [--reply-to <id>] [--kind <kind>] <to> <body...>");
    }
    show(store.send(who, to, body, { kind, replyTo, taskId, outputPath }));
    break;
  }

  case "inbox": {
    const digestAt = argv.indexOf("--digest");
    if (digestAt !== -1) {
      argv.splice(digestAt, 1);
      const taskId = flag("task-id");
      if (argv.length !== 1) throw new Error("usage: fabric inbox --digest [--task-id <id>]");
      show(store.inboxDigest(who, taskId));
      break;
    }
    const limitText = flag("limit");
    const limit = limitText === undefined ? 20 : Number(limitText);
    if (!Number.isSafeInteger(limit) || limit <= 0) {
      throw new Error("inbox limit must be a positive integer");
    }
    const claimSecondsText = flag("claim-seconds");
    const claimSeconds = claimSecondsText === undefined ? 300 : Number(claimSecondsText);
    if (!Number.isSafeInteger(claimSeconds) || claimSeconds < 1 || claimSeconds > 3600) {
      throw new Error("claim seconds must be an integer from 1 to 3600");
    }
    const peekAt = argv.indexOf("--peek");
    const peek = peekAt !== -1;
    if (peek) argv.splice(peekAt, 1);
    const taskId = flag("task-id");
    if (argv.length !== 1) {
      throw new Error("usage: fabric inbox [--peek] [--limit N] [--claim-seconds N] [--task-id <id>]");
    }
    show(store.inbox(who, { limit, peek, claimTtlMs: claimSeconds * 1000, taskId }));
    break;
  }

  case "ack": {
    const messageId = argv[1];
    const claimId = argv[2];
    if (messageId === undefined || claimId === undefined || argv.length !== 3) {
      throw new Error("usage: fabric ack <message-id> <claim-id>");
    }
    show(store.acknowledge(who, messageId, claimId));
    break;
  }

  case "note": {
    const detail = argv.slice(1).join(" ");
    if (detail.length === 0) throw new Error("usage: fabric note <text...>");
    store.note(who, detail);
    show({ noted: detail });
    break;
  }

  case "tasks":
    if (argv.length > 2) throw new Error("usage: fabric tasks [state]");
    show(store.tasks(who.project, argv[1]));
    break;

  case "task": {
    const objective = argv.slice(1).join(" ");
    if (objective.length === 0) throw new Error("usage: fabric task <objective...>");
    show(store.createTask(who, objective));
    break;
  }

  case "claim": {
    const taskId = argv[1];
    if (taskId === undefined || argv.length !== 2) throw new Error("usage: fabric claim <task-id>");
    show(store.claimTask(who, taskId));
    break;
  }

  case "done": {
    const taskId = argv[1];
    if (taskId === undefined || argv.length !== 2) {
      throw new Error("usage: fabric done <task-id>");
    }
    show(store.updateTask(who, taskId, "done"));
    break;
  }

  case "activity": {
    const afterSeqText = flag("after-seq");
    const limit = positiveNumber(flag("limit"), 50, "activity limit");
    if (!Number.isSafeInteger(limit) || argv.length !== 1) {
      throw new Error("usage: fabric activity [--after-seq N] [--limit N]");
    }
    if (afterSeqText === undefined) {
      show(store.activity(who.project, limit));
      break;
    }
    const afterSeq = Number(afterSeqText);
    if (!Number.isSafeInteger(afterSeq) || afterSeq < 0) {
      throw new Error("activity cursor must be a non-negative integer");
    }
    show(store.activityAfter(who.project, afterSeq, limit));
    break;
  }

  case "watch": {
    const interval = positiveNumber(flag("interval"), 2, "watch interval");
    if(argv.includes("--activity")) {
      const initial=store.activity(who.project,200).reverse();printActivity(initial);
      let cursor=initial.at(-1)?.seq ?? 0;
      for(;;) {
        await sleep(interval*1000);
        let rows;
        do {rows=store.activityAfter(who.project,cursor,200);printActivity(rows);cursor=rows.at(-1)?.seq ?? cursor;} while(rows.length === 200);
      }
    }
    const ids=argv.slice(1);
    if(ids.some(id=>id.startsWith("--"))) throw new Error("usage: fabric watch [ids…] [--interval N]");
    const seen=new Map<string,string>();
    for(;;) {
      const result=await statusRows(who.cwd,ids.length ? ids : undefined);
      if(!result.runs) throw new Error(digest(result));
      for(const row of result.runs) {
        const key=`${row.run_id}:${row.task_id}`;
        const state=`${row.attempt ?? 1}:${row.state}:${row.status}`;
        if(seen.get(key) !== state) { console.log(digest(row).split("\n")[0]);seen.set(key,state); }
      }
      if(result.runs.every(row=>row.state === "terminal")) break;
      await sleep(interval*1000);
    }
    break;
  }

  case "events": {
    const followAt = argv.indexOf("--follow");
    const follow = followAt !== -1;
    if (follow) argv.splice(followAt, 1);
    const idleAt = argv.indexOf("--until-idle");
    const untilIdle = idleAt !== -1;
    if (untilIdle) argv.splice(idleAt, 1);
    if (argv.length !== 1 || (untilIdle && !follow)) throw new Error("usage: fabric events [--follow [--until-idle]]");
    let cursor: string | undefined;
    do {
      const snapshot = await readEvents(who.cwd, cursor, store.inbox(who, { peek: true, limit: 100 }));
      if (snapshot.status !== "ok") throw new Error(String(snapshot.error));
      cursor = snapshot.cursor;
      for (const event of snapshot.events) console.log(JSON.stringify(event));
      if (untilIdle && snapshot.events.length === 0) {
        const runs = await readRuns(who.cwd);
        if (runs.status !== "ok") throw new Error(String(runs.error));
        if (runs.runs.every((run) => run.state === "terminal" || run.state === "input_required")) break;
      }
      if (follow) await sleep(250);
    } while (follow);
    break;
  }

  default:
    throw new Error(`unhandled command ${command}`);
  }
} catch (error) {
  console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
} finally {
  store?.close();
}
