import { setTimeout as delay } from "node:timers/promises";

import { identify } from "../src/identity.js";
import { Store } from "../src/store.js";

const [databasePath, projectCwd, indexText, startAtText] = process.argv.slice(2) as [
  string, string, string, string,
];
const index = Number(indexText);
const who = identify({
  AGENT_FABRIC_SEAT: "codex",
  AGENT_FABRIC_LABEL: `cold-start-${index}`,
}, projectCwd);

let store: Store | undefined;
try {
  const startAt = Number(startAtText);
  if (Date.now() < startAt) await delay(startAt - Date.now());
  store = new Store(databasePath);
  store.announce(who);
  console.log(JSON.stringify({ index, ok: true }));
} catch (error) {
  console.log(JSON.stringify({
    index,
    ok: false,
    error: error instanceof Error ? error.message : String(error),
  }));
} finally {
  store?.close();
}
