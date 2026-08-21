import { setTimeout as delay } from "node:timers/promises";

import { identify } from "../src/identity.js";
import { Store } from "../src/store.js";

const [databasePath, projectCwd, startAtText] = process.argv.slice(2) as [string, string, string];
const who = identify({
  AGENT_FABRIC_SEAT: "codex",
  AGENT_FABRIC_LABEL: "shared-reader",
}, projectCwd);

let store: Store | undefined;
try {
  store = new Store(databasePath);
  store.announce(who);
  const startAt = Number(startAtText);
  if (Date.now() < startAt) await delay(startAt - Date.now());
  const claims = store.inbox(who, { limit: 1 });
  console.log(JSON.stringify({
    claims: claims.map((claim) => ({ messageId: claim.messageId, claimId: claim.claimId })),
  }));
} catch (error) {
  console.log(JSON.stringify({
    error: error instanceof Error ? error.stack ?? error.message : String(error),
  }));
  process.exitCode = 1;
} finally {
  store?.close();
}
