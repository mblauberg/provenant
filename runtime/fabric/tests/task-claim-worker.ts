import { setTimeout as delay } from "node:timers/promises";

import { identify } from "../src/identity.js";
import { Store } from "../src/store.js";

const [databasePath, projectCwd, indexText, startAtText] = process.argv.slice(2) as [
  string,
  string,
  string,
  string,
];
const who = identify({
  AGENT_FABRIC_SEAT: "codex",
  AGENT_FABRIC_LABEL: `claimant-${indexText}`,
}, projectCwd);

let store: Store | undefined;
try {
  store = new Store(databasePath);
  store.announce(who);
  const startAt = Number(startAtText);
  if (Date.now() < startAt) await delay(startAt - Date.now());
  try {
    const task = store.claimTask(who, "shared-task");
    console.log(JSON.stringify({ won: true, owner: task.owner }));
  } catch (error) {
    console.log(JSON.stringify({
      won: false,
      reason: error instanceof Error ? error.message : String(error),
    }));
  }
} finally {
  store?.close();
}
