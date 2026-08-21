import { setTimeout as delay } from "node:timers/promises";

import { identify } from "../src/identity.js";
import { Store } from "../src/store.js";

const [databasePath, projectCwd, role, label, startAtText] = process.argv.slice(2) as [
  string, string, "agent" | "team", string, string,
];
const creator = identify({
  AGENT_FABRIC_SEAT: "codex",
  AGENT_FABRIC_LABEL: "namespace-creator",
}, projectCwd);
const candidate = identify({
  AGENT_FABRIC_SEAT: "codex",
  AGENT_FABRIC_LABEL: label,
}, projectCwd);

const store = new Store(databasePath);
try {
  const startAt = Number(startAtText);
  if (Date.now() < startAt) await delay(startAt - Date.now());
  if (role === "agent") store.announce(candidate);
  else store.createTeam(creator, label, [creator.agentId]);
  console.log(JSON.stringify({ role, won: true }));
} catch (error) {
  console.log(JSON.stringify({
    role,
    won: false,
    error: error instanceof Error ? error.message : String(error),
  }));
} finally {
  store.close();
}
