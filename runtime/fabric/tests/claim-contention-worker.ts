import { identify } from "../src/identity.js";
import { Store } from "../src/store.js";

const [databasePath, projectCwd, claimTtlText] = process.argv.slice(2) as [string, string, string];
const who = identify({
  AGENT_FABRIC_SEAT: "codex",
  AGENT_FABRIC_LABEL: "contended-reader",
}, projectCwd);

const store = new Store(databasePath);
try {
  store.announce(who);
  process.stdout.write("ready\n");
  await new Promise<void>((resolve) => process.stdin.once("data", () => resolve()));
  const message = store.inbox(who, { limit: 1, claimTtlMs: Number(claimTtlText) })[0];
  process.stdout.write(`${JSON.stringify(message)}\n`);
} finally {
  store.close();
}
