/**
 * The same store from a shell, for agents that would rather run a command than
 * hold an MCP connection, and for the user watching what their agents are doing.
 *
 *   fabric whoami
 *   fabric send <to> <body...>
 *   fabric inbox [--peek]
 *   fabric tasks [state]
 *   fabric watch [--interval 2]
 */
import { databasePath, identify } from "./identity.js";
import { Store } from "./store.js";

const USAGE = `fabric <command>

  whoami                      who am I, and who else is in this project
  send <to> <body...>         to an agent id, a team id, or "all"
       [--reply-to <id>]      thread this onto an existing message
       [--kind <kind>]        note (default), request, response
  inbox [--peek]              my unread messages; --peek leaves them unread
  note <text...>              record something in the activity log
  tasks [state]               list tasks, optionally filtered by state
  task <objective...>         open a task
  done <task-id>              close a task
  watch [--interval N]        tail everything agents here are doing

Identity comes from the working directory and AGENT_FABRIC_LABEL. There is
nothing to install, trust or provision.`;

const argv = process.argv.slice(2);
const command = argv[0] ?? "whoami";

if (command === "--help" || command === "-h" || command === "help") {
  console.log(USAGE);
  process.exit(0);
}

/** Read `--flag value` out of argv, so the remaining words form the body. */
const flag = (name: string): string | undefined => {
  const at = argv.indexOf(`--${name}`);
  if (at === -1) return undefined;
  const value = argv[at + 1];
  argv.splice(at, value === undefined ? 1 : 2);
  return value;
};
const who = identify();
const store = new Store(databasePath());
store.announce(who);

const show = (value: unknown): void => {
  console.log(JSON.stringify(value, null, 2));
};

const sleep = async (ms: number): Promise<void> =>
  await new Promise((done) => setTimeout(done, ms));

switch (command) {
  case "whoami":
    show({ ...who, database: databasePath(), agents: store.agents(who.project) });
    break;

  case "send": {
    // Strip the flags first so whatever is left is the recipient and the body.
    const replyTo = flag("reply-to");
    const kind = flag("kind");
    const to = argv[1];
    const body = argv.slice(2).join(" ");
    if (to === undefined || body.length === 0) {
      throw new Error("usage: fabric send [--reply-to <id>] [--kind <kind>] <to> <body...>");
    }
    show(store.send(who, to, body, { kind, replyTo }));
    break;
  }

  case "inbox":
    show(store.inbox(who, { peek: argv.includes("--peek") }));
    break;

  case "note":
    store.note(who, argv.slice(1).join(" "));
    break;

  case "tasks":
    show(store.tasks(who.project, argv[1]));
    break;

  case "task":
    show(store.createTask(who, argv.slice(1).join(" ")));
    break;

  case "done": {
    const taskId = argv[1];
    if (taskId === undefined) throw new Error("usage: fabric done <task-id>");
    show(store.updateTask(who, taskId, "done"));
    break;
  }

  // Oversight: the whole monitoring story is tailing one table.
  case "watch": {
    const interval = Number(argv[argv.indexOf("--interval") + 1]) || 2;
    let seen = 0;
    for (;;) {
      const rows = store.activity(who.project, 200).reverse();
      for (const row of rows.slice(seen)) {
        console.log(`${row.at}  ${row.agentId.padEnd(12)} ${row.kind.padEnd(6)} ${row.detail}`);
      }
      seen = rows.length;
      await sleep(interval * 1000);
    }
  }

  default:
    console.error(USAGE);
    process.exitCode = 2;
}

if (command !== "watch") store.close();
