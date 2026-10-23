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
import { digest, quarantineAction } from "./surface.js";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pythonOwner, pythonOwnerEnvironment } from "./python-owner.js";
import { databasePath, identify, runRoot, withoutGitRedirects } from "./identity.js";
import { dispatchConfiguredProvider, hostOwnersInThemselves, type BatchInput, type DispatchInput } from "./execution.js";
import { dispatchConfiguredBatch } from "./batch-execution.js";
import { productRoot } from "./run-root.js";
import type { RouteInput } from "./execution-input.js";
import {
 findRecordedRun, listRecordedRuns, retentionHours, terminateRecordedRun,
} from "./run-registry.js";
import { federatedStatusRows as statusRows, fabricStatus } from "./run-status.js";
import { inspectDatabase, isSQLiteContention, Store } from "./store.js";
import { waitForLanes } from "./lane-wait.js";
import { isLandingVerb, landingCommand, UsageError, validBranch, validRemote } from "./landing-cli.js";
import { readEvents, readRuns } from "./run-reader.js";
import { laneIdle } from "./lane-watch.js";
import { ancestorPids, laneReader, seatRunDirectories } from "./seat-lanes.js";
import { reconcileSession, sessionDispatch, sessionName, sessionView } from "./sessions.js";

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
  landing-wait [--timeout N]  block until the landing lease changes or frees (default 900s; exit 124 on timeout)
  landing-push <session> <generation> <branch> [--label <seat>] [--remote R]
                              verify lease and remote SHA, then push HEAD (remote default origin)
  landing-acquire <session> (--expected-sha SHA | --branch B [--remote R]) [--seconds N] [--label L] [--json]
                              take the landing lease; exit 3 while another holder's is live
  landing-verify <session> <generation> [--expected-sha SHA] [--branch B [--remote R]] [--label L] [--json]
                              check holder and generation, and optionally the remote tip
  landing-release <session> <generation> [--label L] [--json]
                              release your lease; released:false with a reason if not held
  landing-status [--json]     the live landing lease, or none
  done <task-id>              close a task
  activity [--after-seq N]    list activity, optionally after a cursor
           [--limit N]
  watch [ids…] [--interval N] print run state changes; exit when all terminal
  lanes [--json] [--all] [id…]
                              run states; default shows active and recent terminal lanes (--all lists all)
  lanes --wait [--all] [--summary] [--timeout N] [id…]
                              wait for a lane to finish or need input; --all waits
                              for every listed lane; exit 1 if any ends non-ok
                              (needs-input still exits 0); exit 124 on timeout
  lanes --project <path> …   read another project's lanes (default: from the cwd)
  graph run --file F          validate a plan graph, start its owner, print its digest
  graph status [ID] [--json] [--wait [--timeout N]]
                              one graph's digest, or the list; exit 124 on timeout
  graph cancel ID [--node N]  fenced cancel of a graph or one node
  graph resume ID             restart a graph whose owner died
  events [--follow] [--until-idle]  JSON lines; follow stays open unless idle exit is requested
  status [id] [--wait-seconds N]  run status by task, batch or run directory; no id: store summary
  doctor [--json]             read-only schema and integrity diagnostics
  adapters [--json]           configured providers: dispatch state, aliases,
                              read-only guarantee, endpoint profiles
  adapters --models A [--match S]  one adapter's live models (cached probe);
                              dispatch any as --model A/<id>
  dispatch list [--json]      configured-provider runs recorded in this workspace
  dispatch kill <run> [--json]  stop one recorded run and the group it leads
  cancel <id@host> [--operation-id ID]  stop a lane on its owning host
  fetch <operation-or-task@host>  import a verified remote writer commit
  output <id@host> [--part P] [--offset N] [--max-bytes N] [--tail]
                              bounded output from the owning host
  dispatch --prompt-file F [--adapter A] [--alias NAME | --model M] [--effort E] [--account LABEL]
           [--mode MODE] [--worktree W | --cwd D] [--id ID] [--wait]
           [--host H] [--operation-id ID] [--delivery-run-id ID] [--resume ID | --handoff ID]
  dispatch --prompt-file F --route junior|senior|principal|design|writing
                              top model of a global pool; --adapter narrows it
  dispatch --prompt-file F --route R --rotate    cycle the pool per project
  dispatch --prompt-file F --route R --council N  N members (1-8), families
                              spread first; rotate is ignored with a council
  dispatch --prompt-file F --models a/m[@e],b/m  ad-hoc council; bypasses --route
  --confidential              with any selector: never a prompt-training model,
                              fallbacks included; free models count unless
                              their entry says they do not train
  An explicit --model wins over --route; see
  skills/orchestrate/references/routing-and-tiers.md#route-pools
  dispatch --tasks F [route flags]  run a JSON task manifest; flags set task defaults
  dispatch --session NAME --prompt-file F [--fresh] [--wait]
                              start or resume a named provider session
  session list | inspect <name> | forget <name>

Identity comes from the working directory and AGENT_FABRIC_LABEL (or
--label on the landing-* commands). Landing commands exit 0 ok, 1 refused or
stale, 2 usage, 3 held by another holder or a push in progress. Registered
worktrees share one repository project while retaining their own cwd. There is
nothing to install, trust or provision.`;

const drainAndExit = async (code: number): Promise<never> => {
  const drain = (stream: NodeJS.WriteStream) => new Promise<void>((resolveDrain) =>
    stream.write("", () => resolveDrain()));
  await Promise.all([drain(process.stdout), drain(process.stderr)]);
  process.exit(code);
};

const argv = process.argv.slice(2);
const command = argv[0] ?? "whoami";
const commands = new Set([
  "whoami", "send", "inbox", "ack", "note", "tasks", "task", "claim", "done",
  "activity", "watch", "status", "doctor", "dispatch", "adapters", "lanes", "events",
  "work-claims", "landing-wait", "landing-push", "landing-acquire", "landing-verify", "landing-release",
  "landing-status", "session", "cancel", "output", "fetch", "graph",
]);

// Internal graph-runner entry points: never listed, never run by hand.
if (command === "gate-exec") {
  const { runGateExec } = await import("./graph/gates.js");
  await drainAndExit(await runGateExec(argv[1] ?? ""));
}
if (command === "graph-owner") {
  const { runGraphOwner } = await import("./graph/owner.js");
  await drainAndExit(await runGraphOwner(argv[1] ?? "", argv[2] ?? ""));
}
if (command === "--help" || command === "-h" || command === "help") {
  console.log(USAGE);
  await drainAndExit(0);
}
if (!commands.has(command)) {
  console.error(`unknown command "${command}"\n\n${USAGE}`);
  await drainAndExit(2);
}
if (argv.slice(1).some((arg) => arg === "--help" || arg === "-h")) {
  let include = false;
  const lines = USAGE.split("\n").filter((line) => {
    if (/^  \S/u.test(line)) include = line.startsWith(`  ${command} `) || line.trim() === command;
    else if (!line.startsWith("  ")) include = false;
    return include;
  });
  console.log(`fabric ${command}\n\n${lines.join("\n")}`);
  await drainAndExit(0);
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
/** Write the whole line before exiting: stdout is asynchronous on a macOS pipe. */
const finish = async (code: number, line: string, stream: NodeJS.WriteStream = process.stdout): Promise<never> => {
  await new Promise<void>((done) => stream.write(`${line}\n`, () => done()));
  return await drainAndExit(code);
};
if (isLandingVerb(command)) {
  try {
    const outcome = landingCommand(command, argv.slice(1), process.env, () => new Store(databasePath()));
    if (outcome.json) await finish(outcome.code, JSON.stringify(outcome.result));
    await finish(outcome.code, outcome.line, outcome.code === 0 ? process.stdout : process.stderr);
  } catch (error) {
    await finish(error instanceof UsageError ? 2 : 1,
      `fabric: ${error instanceof Error ? error.message : String(error)}`, process.stderr);
  }
}
let landingLabel: string | undefined;
let landingRemote = "origin";
let landingJson = false;
try {
  landingLabel = command === "landing-push" ? flag("label") : undefined;
  if (command === "landing-push") {
    landingRemote = flag("remote") ?? landingRemote;
    if (!validRemote(landingRemote)) throw new Error(`invalid remote name: ${landingRemote}`);
    // With --json, stdout carries only the result; Git and hook output go to stderr.
    landingJson = argv.includes("--json");
    if (landingJson) argv.splice(argv.indexOf("--json"), 1);
  }
} catch (error) {
  console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
  await drainAndExit(2);
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
if (command === "fetch") {
  const ids = argv.slice(1).filter((value) => value !== "--json");
  if (ids.length !== 1) { console.error("Pass exactly one remote writer ID"); await drainAndExit(2); }
  const { federatedLane } = await import("./hosts.js");
  const result = await federatedLane("fetch", who.cwd, { id: ids[0] }) ??
    { status: "rejected", error: "remote_host_required", fix: "Qualify a remote writer ID with its host." };
  console.log(JSON.stringify(result, null, 2));
  await drainAndExit(result.status === "ok" ? 0 : 1);
}
if (command === "cancel" || command === "output") {
  try {
    const operationId = flag("operation-id");
    const reason = flag("reason");
    const part = flag("part"), offset = flag("offset"), maxBytes = flag("max-bytes");
    const tail = argv.includes("--tail");
    const rest = argv.slice(1).filter((value) => !["--json", "--tail"].includes(value));
    if (rest.length !== 1 || rest[0]!.startsWith("--")) throw new Error("Pass exactly one lane ID");
    const { cancelConfiguredRun } = await import("./batch-execution.js");
    const { fabricOutput } = await import("./run-status.js");
    const result = command === "cancel"
      ? await cancelConfiguredRun(rest[0]!, who, reason, process.env, operationId)
      : await fabricOutput(who.project, { id: rest[0]!, part, offset: offset === undefined ? undefined : Number(offset),
        max_bytes: maxBytes === undefined ? undefined : Number(maxBytes), tail });
    console.log(JSON.stringify(result, null, 2));
    await drainAndExit(result.status === "rejected" ? 1 : 0);
  } catch (error) {
    console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
    await drainAndExit(2);
  }
}
if (command === "graph") {
  const { graphCancel, graphResume, graphRun, graphStatus } = await import("./graph/surface.js");
  let file: string | undefined, timeout: string | undefined, node: string | undefined;
  try { file = flag("file"); timeout = flag("timeout"); node = flag("node"); }
  catch (error) { console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`); await drainAndExit(2); }
  const [verb, ...rest] = argv.slice(1);
  const json = rest.includes("--json"), wait = rest.includes("--wait"), force = rest.includes("--force");
  const words = rest.filter((value) => value !== "--json" && value !== "--wait" && value !== "--force");
  const timeoutSeconds = timeout === undefined ? undefined : Number(timeout);
  const usage = "fabric: usage: fabric graph run --file F | status [ID] [--json] [--wait [--timeout N]] | cancel ID [--node N] | resume ID [--force]";
  const valid = (verb === "run" && file !== undefined && !words.length && !wait && node === undefined) ||
    (verb === "status" && words.length <= 1 && file === undefined && node === undefined && (timeout === undefined || wait) &&
      (timeoutSeconds === undefined || timeoutSeconds >= 0) && !(json && wait)) ||
    (verb === "cancel" && words.length === 1 && file === undefined && !wait) ||
    (verb === "resume" && words.length === 1 && file === undefined && node === undefined && !wait);
  if (!valid || (force && verb !== "resume") || words.some((value) => value.startsWith("--"))) { console.error(usage); await drainAndExit(2); }
  const chair = executionIdentity();
  const result = verb === "run" ? graphRun({ file }, chair)
    : verb === "status" ? await graphStatus(words[0], chair, process.env, { waitSeconds: wait ? (timeoutSeconds ?? 86_400) : 0 })
    : verb === "cancel" ? graphCancel(words[0]!, node, chair)
    : graphResume(words[0]!, chair, process.env, force);
  if (json) console.log(JSON.stringify(result, null, 2));
  else console.log(typeof result.digest === "string" ? result.digest : `${String(result.error ?? result.status)}${result.fix ? `\nfix: ${String(result.fix)}` : ""}`);
  await drainAndExit(result.status === "rejected" ? 1 : result.timed_out ? 124 : 0);
}
if (command === "lanes") {
  let project: string | undefined, timeout: string | undefined;
  try {
    project = flag("project");
    timeout = flag("timeout");
  } catch (error) {
    console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
    await drainAndExit(2);
  }
  // A nested repository resolves to its own project; --project reads another one.
  const laneWho = project === undefined ? who : identify(process.env, resolve(project));
  const args = argv.slice(1);
  const json = args.includes("--json");
  const wait = args.includes("--wait");
  const all = args.includes("--all");
  const timeoutSeconds = timeout === undefined ? undefined : Number(timeout);
  const summary = args.includes("--summary");
  const ids = args.filter((value) => !["--json", "--wait", "--all", "--summary"].includes(value));
  if (["--json", "--wait", "--all", "--summary"].some((name) => args.filter((value) => value === name).length > 1) ||
      (json && wait) || (summary && !wait) || (timeout !== undefined && !wait) || ids.some((value) => value.startsWith("--")) ||
      (timeoutSeconds !== undefined && !(timeoutSeconds >= 0))) {
    console.error("fabric: usage: fabric lanes [--json] [--all] [--project P] [id…] | lanes --wait [--all] [--summary] [--timeout N] [id…]");
    await drainAndExit(2);
  }
  if (wait) {
    const watched = ids.length ? ids : undefined;
    // A graph whose owner died schedules nothing more: say how to restart it before waiting.
    const hint = (await import("./graph/surface.js")).deadGraphHint(laneWho);
    if (hint) console.error(hint);
    const store = new Store(databasePath());
    store.announce(laneWho);
    let code: number;
    const seatAncestors = ancestorPids();
    const watcher = laneIdle(() => [laneWho.project, ...(project === undefined ? seatRunDirectories(seatAncestors).keys() : [])]
      .map((dir) => join(runRoot(dir), "runs")));
    const reader = laneReader({ who: laneWho, ids: watched, deps: { ancestors: seatAncestors,
      ...(project === undefined ? {} : { ancestors: new Set<number>() }) } });
    const held: string[] = [];
    // With --summary, lanes are acknowledged only once printed, so a lane cut by the cap is reported next wait.
    let toMark: Parameters<Store["markLanesSeen"]>[1] = [];
    const waitStore = summary ? {
      unseenLanes: (who: Parameters<Store["unseenLanes"]>[0], rows: Parameters<Store["unseenLanes"]>[1]) => store.unseenLanes(who, rows),
      markLanesSeen: (_who: Parameters<Store["markLanesSeen"]>[0], rows: Parameters<Store["markLanesSeen"]>[1]) => { toMark = rows; },
    } : store;
    try {
      // The registered project, not the raw cwd, owns the runs; the normal
      // retention window bounds the scan, and no row cap may hide a completion.
      code = await waitForLanes({
        who: laneWho, store: waitStore, all, watched: watched !== undefined, timeoutSeconds,
        read: reader, refresh: reader.refresh, stale: reader.stale,
        write: (text) => summary ? Promise.resolve(void held.push(text)) : new Promise((resolveWrite, reject) =>
          process.stdout.write(text, (error) => (error ? reject(error) : resolveWrite()))),
        fail: (text) => console.error(text),
        sleep: (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms)),
        idle: watcher.idle,
      });
      if (summary) {
        // One bounded line per lane, rather than the full listing; the lane lines come first.
        const lines = held.join("").split("\n").filter(Boolean);
        const laneLines = Math.min(toMark.length, lines.length);
        const cap = 20;
        const clip = (line: string) => line.length > 160 ? `${line.slice(0, 159)}…` : line;
        const shown = lines.slice(0, Math.min(laneLines, cap)).map(clip);
        if (laneLines > cap) shown.push(`+${laneLines - cap} more; run lanes --wait again`);
        shown.push(...lines.slice(laneLines).map(clip));
        if (shown.length) console.log(shown.join("\n"));
        try {
          store.markLanesSeen(laneWho, toMark.slice(0, cap));
        } catch (error) {
          if (!isSQLiteContention(error)) throw error;
        }
      }
    } finally {
      watcher.close();
      store.close();
    }
    await drainAndExit(code);
  }
  const result = await readRuns(laneWho.project, ids.length ? ids : undefined, 0, undefined, (all || !json) ? null : undefined);
  if (json) {
    console.log(JSON.stringify(result, null, 2));
  } else if (result.status === "ok") {
    const now = Date.now();
    const age = (started: string | null) => {
      if (!started) return "unknown";
      const seconds = Math.max(0, Math.floor((now - Date.parse(started)) / 1000));
      return seconds < 60 ? `${seconds}s` : seconds < 3600
        ? `${Math.floor(seconds / 60)}m` : `${Math.floor(seconds / 3600)}h`;
    };
    const active = (row: (typeof result.runs)[number]) => row.state !== "terminal";
    const needsAttention = (row: (typeof result.runs)[number]) =>
      row.status === "quarantined" ||
      row.status === "input_required" ||
      row.state === "input_required" ||
      row.landable === false ||
      Boolean(row.quarantine) ||
      (typeof row.status === "string" && row.status.includes("review"));
    const activeRows = result.runs.filter(active);
    const terminalRows = result.runs.filter((row) => !active(row));
    terminalRows.sort((a, b) => Date.parse(b.started_at ?? "") - Date.parse(a.started_at ?? ""));
    activeRows.sort((a, b) => Date.parse(b.started_at ?? "") - Date.parse(a.started_at ?? ""));

    let rows: typeof result.runs;
    let omitted = 0;
    if (ids.length || all) {
      rows = [...activeRows, ...terminalRows];
    } else {
      const shownTerminal = terminalRows.filter((row, index) => index < 5 || needsAttention(row));
      omitted = terminalRows.length - shownTerminal.length;
      rows = [...activeRows, ...shownTerminal];
    }

    if (!rows.length)
      console.log(`no lanes for project ${laneWho.project}` +
        (laneWho.cwd === laneWho.project ? "" : ` (cwd ${laneWho.cwd} resolves to it; --project reads another)`));
    for (const row of rows)
      console.log(`${row.status ?? row.state}  ${row.id}  ${row.route ?? "-"}  ${age(row.started_at)}${row.status === "quarantined" ? ` · ${quarantineAction(row)}` : ""}`);
    if (omitted > 0)
      console.log(`${omitted} more; use --all`);
  } else {
    console.log(JSON.stringify(result));
  }
  await drainAndExit(result.status === "ok" ? 0 : 1);
}
if (command === "status") {
  try {
    const wait = flag("wait-seconds");
    const rest = argv.slice(1).filter((value) => value !== "--json" && value !== "--runs");
    if (rest.length > 1 || rest.some((value) => value.startsWith("--"))) {
      throw new Error("usage: fabric status [id] [--wait-seconds N] [--json]");
    }
    if (rest[0] !== undefined || argv.includes("--runs") || wait !== undefined) {
      console.log(JSON.stringify(await fabricStatus(who.project, rest[0], wait === undefined ? 0 : Number(wait)), null, 2));
      await drainAndExit(0);
    }
  } catch (error) {
    console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
    await drainAndExit(2);
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
      await drainAndExit(2);
    }
    const runs = listRecordedRuns(who.project);
    if (json) {
      console.log(JSON.stringify({
        workspace: who.project,
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
    await drainAndExit(0);
  }
  if (subcommand === "kill") {
    const reference = rest[1];
    if (rest.length !== 2 || reference === undefined) {
      console.error("fabric: usage: fabric dispatch kill <run-id|run-dir> [--json]");
      await drainAndExit(2);
    }
    const { federatedLane } = await import("./hosts.js");
    const remote = await federatedLane("cancel", who.project, { id: reference! });
    if (remote) {
      console.log(JSON.stringify(remote, null, 2));
      await drainAndExit(remote.status === "rejected" ? 1 : 0);
    }
    const run = findRecordedRun(who.project, reference!.split("@")[0]!);
    if (run === undefined) {
      console.error(`fabric: no recorded dispatch run: ${reference}`);
      await drainAndExit(1);
    }
    const outcome = await terminateRecordedRun(run!);
    const state = outcome.reason === "still running"
      ? outcome.reason
      : outcome.signalled ? "signalled" : "not running";
    const output = json ? JSON.stringify(outcome, null, 2)
      : `${run!.run_id} ${state}${outcome.escalated ? " (escalated to SIGKILL)" : ""}` +
        `${outcome.reason !== undefined && outcome.reason !== state ? ` (${outcome.reason})` : ""}`;
    await new Promise<void>((resolveWrite) => process.stdout.write(`${output}\n`, "utf8", () => resolveWrite()));
    await drainAndExit(outcome.signalled && outcome.reason !== "still running" ? 0 : 1);
  }
  const controller = new AbortController();
  let interrupted: number | undefined;
  // The transfer has its own process group. Await its cancellation before exiting.
  const interrupt = (signal: NodeJS.Signals) => {
    interrupted ??= signal === "SIGINT" ? 130 : 143;
    controller.abort();
  };
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", interrupt);
  try {
    const options = subcommand?.startsWith("--") ? rest : rest.slice(1);
    const values = new Map<string, string>();
    const switches = new Set<string>();
    const allowed = new Set(["--adapter", "--alias", "--model", "--effort", "--account", "--mode", "--worktree", "--cwd", "--prompt-file", "--id", "--tasks",
      "--route", "--council", "--models", "--session", "--host", "--operation-id", "--resume", "--handoff", "--delivery-run-id"]);
    for (let index = 0; index < options.length; index += 1) {
      const option = options[index]!;
      if (["--wait", "--rotate", "--confidential", "--fresh"].includes(option)) {
        if (switches.has(option)) throw new Error(`${option} may be passed once`);
        switches.add(option);
        continue;
      }
      if (!option.startsWith("--")) throw new Error(`unexpected argument: ${option}`);
      if (!allowed.has(option)) throw new Error(`unknown option ${option}; choose ${[...allowed].join(", ")}, --rotate, --confidential, --fresh or --wait`);
      const value = options[++index];
      if (value === undefined || value.startsWith("--")) throw new Error(`${option} requires a value`);
      if (values.has(option)) throw new Error(`${option} may be passed once`);
      values.set(option, value);
    }
    const read = (name: string): string | undefined => values.get(`--${name}`);
    if (subcommand !== undefined && !subcommand.startsWith("--"))
      throw new Error(`unknown dispatch subcommand: ${subcommand}`);
    const route: RouteInput = {
      ...(read("delivery-run-id") === undefined ? {} : { delivery_run_id: read("delivery-run-id") }),
      ...(read("host") === undefined ? {} : { host: read("host") }),
      ...(read("operation-id") === undefined ? {} : { operation_id: read("operation-id") }),
      ...(read("adapter") === undefined ? {} : { adapter: read("adapter") }),
      ...(read("alias") === undefined ? {} : { alias: read("alias") }),
      ...(read("model") === undefined ? {} : { model: read("model") }),
      ...(read("effort") === undefined ? {} : { effort: read("effort") }),
      ...(read("account") === undefined ? {} : { account: read("account") }),
      ...(read("mode") === undefined ? {} : { mode: read("mode") as RouteInput["mode"] }),
      ...(read("worktree") === undefined ? {} : { worktree: read("worktree") }),
      ...(read("cwd") === undefined ? {} : { cwd: read("cwd") }),
      ...(read("route") === undefined ? {} : { route: read("route") }),
      ...(read("council") === undefined ? {} : { council: Number(read("council")) }),
      ...(read("models") === undefined ? {} : { models: read("models")!.split(",").map((item) => item.trim()).filter(Boolean) }),
      ...(switches.has("--rotate") ? { rotate: true } : {}),
      ...(switches.has("--confidential") ? { confidential: true } : {}),
    };
    if (route.council !== undefined && (!Number.isInteger(route.council) || route.council < 1 || route.council > 8))
      throw new Error("--council requires an integer from 1 to 8");
    const tasksFile = read("tasks");
    // This process exits once the wait ends; the run must not depend on it.
    hostOwnersInThemselves();
    let result: Record<string, unknown>;
    let sessionLine: string | undefined;
    if (switches.has("--fresh") && read("session") === undefined) throw new Error("--fresh requires --session");
    if (read("session") !== undefined) {
      if (tasksFile !== undefined) throw new Error("--session cannot be combined with --tasks");
      const store = new Store(databasePath());
      try {
        store.announce(who);
        const name = read("session")!;
        result = await sessionDispatch({
          ...route,
          prompt_file: read("prompt-file"),
          session: name,
          ...(switches.has("--fresh") ? { fresh: true } : {}),
          ...(read("id") === undefined ? {} : { task_id: read("id") }),
          wait_seconds: switches.has("--wait") ? 55 : 0,
        }, { ...who, registeredProjects: store.projects() }, store, controller.signal);
        const session = await reconcileSession(store, who, name);
        if (session) sessionLine = String(sessionView(session).digest);
        if (result.session_error) sessionLine = `${sessionLine ?? ""}\n! ${String(result.session_error)}`.trim();
      } finally {
        store.close();
      }
      result = { ...result, id: result.id ?? result.run_id };
    } else if (tasksFile !== undefined) {
      if (["prompt-file", "id"].some((key) => read(key) !== undefined))
        throw new Error("--tasks cannot be combined with prompt or id flags");
      const document = JSON.parse(readFileSync(resolve(who.cwd, tasksFile), "utf8")) as unknown;
      const batch = (Array.isArray(document) ? { tasks: document } : document) as BatchInput;
      if (!Array.isArray(batch.tasks)) throw new Error("--tasks file must contain a tasks array");
      result = await dispatchConfiguredBatch({ ...route, ...batch, wait_seconds: switches.has("--wait") ? 55 : 0 }, executionIdentity(), controller.signal);
    } else {
      const input: DispatchInput = {
        ...route,
        prompt_file: read("prompt-file"),
        wait_seconds: switches.has("--wait") ? 55 : 0,
        ...(read("id") === undefined ? {} : { task_id: read("id") }),
        ...(read("worktree") === undefined ? {} : { worktree: read("worktree") }),
        ...(read("cwd") === undefined ? {} : { cwd: read("cwd") }),
        ...(read("resume") === undefined ? {} : { resume: read("resume") }),
        ...(read("handoff") === undefined ? {} : { handoff: read("handoff") }),
      };
      const { resumeConfiguredProvider, handoffDispatch } = await import("./resume.js");
      result = input.resume ? await resumeConfiguredProvider(input, executionIdentity(), controller.signal)
        : input.handoff ? await handoffDispatch(input, executionIdentity(), controller.signal)
        : await dispatchConfiguredProvider(input, executionIdentity(), controller.signal);
    }
    if (typeof result.operation_id === "string") console.log(`operation_id: ${result.operation_id}`);
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
    if (result.placement_fallback || result.status === "launch_unknown") console.log(digest(result));
    if (sessionLine) console.log(sessionLine);
    await drainAndExit(interrupted ?? (result.status === "rejected" ? 1 : 0));
  } catch (error) {
    console.error(`fabric: ${error instanceof Error ? error.message : String(error)}`);
    await drainAndExit(interrupted ?? 2);
  }
}
if (command === "adapters" && argv.includes("--models")) {
  const rest = argv.slice(1);
  const value = (flag: string) => { const at = rest.indexOf(flag); return at < 0 ? undefined : rest[at + 1]; };
  const adapter = value("--models"), match = value("--match");
  const known = rest.filter((argument, index) => ["--models", "--match"].includes(argument) ||
    ["--models", "--match"].includes(rest[index - 1] ?? ""));
  if (adapter === undefined || adapter.startsWith("--") || match?.startsWith("--") || known.length !== rest.length) {
    console.error("fabric: usage: fabric adapters --models ADAPTER [--match TEXT]");
    await drainAndExit(2);
  }
  const { liveModels } = await import("./catalogue.js");
  console.log((await liveModels(adapter!, { match })).digest);
  await drainAndExit(0);
}
if (command === "adapters") {
  const unknown = argv.slice(1).filter((argument) => argument !== "--json");
  if (unknown.length > 0) {
    console.error("fabric: usage: fabric adapters [--json] | fabric adapters --models ADAPTER [--match TEXT]");
    await drainAndExit(2);
  }
  const { catalogueSnapshot } = await import("./catalogue.js");
  const snapshot = catalogueSnapshot();
  if (argv.includes("--json")) {
    console.log(JSON.stringify(snapshot, null, 2));
  } else if (snapshot.adapters.length === 0) {
    console.error("fabric: adapter catalogue unavailable (no product checkout found)");
    await drainAndExit(1);
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
  await drainAndExit(0);
}
if (command === "status" || command === "doctor") {
  const unknown = argv.slice(1).filter((argument) => argument !== "--json");
  if (unknown.length > 0) {
    console.error(`fabric: usage: fabric ${command} [--json]`);
    await drainAndExit(2);
  }
  const result = inspectDatabase(databasePath(), who.project, command);
  console.log(JSON.stringify(result, null, 2));
  await drainAndExit(result.status === "error" ? 1 : 0);
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

  case "session": {
    const [, action, name, ...extra] = argv;
    if (extra.length || !(action === "list" ? name === undefined : ["inspect", "forget"].includes(action ?? "") && name))
      throw new Error("usage: fabric session list | inspect <name> | forget <name>");
    if (action === "list") {
      const rows = [];
      for (const row of store.sessions(who.project)) {
        const current = await reconcileSession(store, who, row.name);
        if (current) rows.push(sessionView(current));
      }
      show({ sessions: rows });
      break;
    }
    const key = sessionName(name);
    const row = await reconcileSession(store, who, key);
    if (!row) throw new Error(`no session ${key}`);
    if (action === "inspect") show(sessionView(row));
    else if (row.turnStatus === null || !store.forgetSession(who, key))
      throw new Error(`session ${key} is busy with run ${row.turnRunId ?? "launch"}; wait or cancel it, then forget`);
    else show({ forgotten: key, run_id: row.runId ?? row.turnRunId });
    break;
  }

  case "work-claims":
    if (argv.length !== 1) throw new Error("usage: fabric work-claims");
    show({ work_claims: store.workClaims(who.project), landing_lease: store.landingLease(who.project) });
    break;

  case "landing-wait": {
    const timeout = flag("timeout") ?? "900";
    if (argv.length !== 1 || !/^\d+$/u.test(timeout)) throw new Error("usage: fabric landing-wait [--timeout N]");
    const waited = await store.waitLanding(who.project, Number(timeout));
    show({ changed: waited.changed, landing_lease: waited.lease });
    if (!waited.changed) process.exitCode = 124;
    break;
  }

  case "landing-push": {
    const session = argv[1], generation = Number(argv[2]), branch = argv[3];
    if (argv.length !== 4 || !session || !Number.isSafeInteger(generation) || generation < 1 || !branch || !validBranch(branch))
      throw new UsageError("usage: fabric landing-push <session> <generation> <branch> [--label <seat>] [--remote R]");
    const lease = store.landingLease(who.project);
    if (!lease) throw new Error("stale landing lease");
    store.verifyLanding(who, session, generation, lease.expectedSha);
    const gitOptions = { cwd: who.cwd, env: withoutGitRedirects(process.env) };
    const remote = execFileSync("git", ["ls-remote", "--heads", landingRemote, `refs/heads/${branch}`],
      { ...gitOptions, encoding: "utf8" }).trim().split(/\s+/u)[0];
    if (remote !== lease.expectedSha) throw new Error("remote integration SHA changed; acquire a new landing lease");
    const head = execFileSync("git", ["rev-parse", "HEAD"], { ...gitOptions, encoding: "utf8" }).trim();
    execFileSync("git", ["merge-base", "--is-ancestor", remote, head], gitOptions);
    const root = productRoot(process.env);
    execFileSync(await pythonOwner(root, process.env),
      [resolve(root, "skills/orchestrate/scripts/lane_protection.py"),
        "--common-dir", resolve(who.project, ".git"), "--tip", head, "--base", lease.expectedSha],
      { ...gitOptions, env: pythonOwnerEnvironment(process.env), stdio: ["ignore", "ignore", "inherit"], timeout: 30_000 });
    const pushed = store.withLandingPush(who, session, generation, remote, () =>
      execFileSync("git", ["push", `--force-with-lease=refs/heads/${branch}:${remote}`,
        landingRemote, `${head}:refs/heads/${branch}`],
      { ...gitOptions, stdio: landingJson ? ["inherit", 2, "inherit"] : "inherit", timeout: 120_000 }));
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
      const result=await statusRows(who.project,ids.length ? ids : undefined, 0, "all", undefined, "brief", true, 20, false, true);
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
      const snapshot = await readEvents(who.project, cursor, store.inbox(who, { peek: true, limit: 100 }));
      if (snapshot.status !== "ok") throw new Error(String(snapshot.error));
      cursor = snapshot.cursor;
      for (const event of snapshot.events) console.log(JSON.stringify(event));
      if (untilIdle && snapshot.events.length === 0) {
        const runs = await readRuns(who.project);
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
  process.exitCode = error instanceof UsageError ? 2 : 1;
} finally {
  store?.close();
}
