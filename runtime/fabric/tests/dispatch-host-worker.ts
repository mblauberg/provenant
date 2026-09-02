// A stand-in MCP host: it starts one detached dispatch, reports the run, then
// waits to be killed so a test can assert what survives its death.
import { dispatchConfiguredProvider } from "../src/execution.js";

const [workspace, prompt] = process.argv.slice(2) as [string, string];

const identity = {
  project: workspace,
  cwd: workspace,
  agentId: "dispatch-host",
  provider: "codex",
};

const started = await dispatchConfiguredProvider(
  { adapter: "codex", prompt, task_id: "host-task", wait_seconds: 0 },
  identity,
  new AbortController().signal,
);
console.log(JSON.stringify(started));
setInterval(() => undefined, 1000);
