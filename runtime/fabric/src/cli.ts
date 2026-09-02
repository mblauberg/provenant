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
import { databasePath, identify } from "./identity.js";
import { findRecordedRun, listRecordedRuns, terminateRecordedRun } from "./run-registry.js";
import { inspectDatabase, Store } from "./store.js";

const USAGE = `fabric <command>

  whoami                      who am I, and who else is in this project
  send <to> <body...>         to an agent id, a team id, or "all"
       [--reply-to <id>]      thread this onto an existing message
       [--kind <kind>]        note (default), request, response
       [--task-id <id>]       link an existing Fabric task
       [--output-path <path>]  opaque output or run path metadata
  inbox [--peek]              claim unacknowledged messages; --peek does not claim
        [--limit N]           return at most N deliveries (default 20)
        [--claim-seconds N]   claim lifetime, 1 to 3600 seconds (default 300)
        [--task-id <id>]       return only deliveries linked to this task
  ack <message-id> <claim-id> acknowledge one claimed delivery
  note <text...>              record something in the activity log
  tasks [state]               list tasks, optionally filtered by state
  task <objective...>         open a task
  claim <task-id>             atomically claim an open, unowned task
  done <task-id>              close a task
  activity [--after-seq N]    list activity, optionally after a cursor
           [--limit N]
  watch [--interval N]        tail everything agents here are doing
  status [--json]             read-only summary; absent state is healthy
  doctor [--json]             read-only schema and integrity diagnostics
  dispatch list [--json]      configured-provider runs recorded in this workspace
  dispatch kill <run> [--json]  stop one recorded run and the group it leads

Identity comes from the working directory and AGENT_FABRIC_LABEL. Registered
worktrees share one repository project while retaining their own cwd. There is
nothing to install, trust or provision.`;

const argv = process.argv.slice(2);
const command = argv[0] ?? "whoami";
const commands = new Set([
  "whoami", "send", "inbox", "ack", "note", "tasks", "task", "claim", "done",
  "activity", "watch", "status", "doctor", "dispatch",
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
const who = identify();
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
      console.log(JSON.stringify({ workspace: who.cwd, runs }, null, 2));
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
    if (json) console.log(JSON.stringify(outcome, null, 2));
    else console.log(`${run.run_id} ${outcome.signalled ? "signalled" : "not running"}` +
      `${outcome.escalated ? " (escalated to SIGKILL)" : ""}`);
    process.exit(outcome.signalled ? 0 : 1);
  }
  console.error(`fabric: usage: fabric dispatch <list|kill> ...`);
  process.exit(2);
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
  store.announce(who);
  switch (command) {
  case "whoami":
    if (argv.length !== 1) throw new Error("usage: fabric whoami");
    show({ ...who, database: databasePath(), agents: store.agents(who.project) });
    break;

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
    if (argv.length !== 1) throw new Error("usage: fabric watch [--interval N]");
    const initial = store.activity(who.project, 200).reverse();
    printActivity(initial);
    let cursor = initial.at(-1)?.seq ?? 0;
    for (;;) {
      await sleep(interval * 1000);
      let rows;
      do {
        rows = store.activityAfter(who.project, cursor, 200);
        printActivity(rows);
        cursor = rows.at(-1)?.seq ?? cursor;
      } while (rows.length === 200);
    }
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
