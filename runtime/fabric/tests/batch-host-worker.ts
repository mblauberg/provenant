import { dispatchConfiguredBatch } from "../src/execution.js";

const [workspace, prompt] = process.argv.slice(2) as [string, string];

const identity = {
  project: workspace,
  cwd: workspace,
  agentId: "dispatch-host",
  provider: "codex",
};

const started = await dispatchConfiguredBatch(
  { adapter: "codex", tasks: [{ id: "host-batch-task", prompt }], wait_seconds: 0 },
  identity,
  new AbortController().signal,
);
console.log(JSON.stringify(started));
setInterval(() => undefined, 1000);
