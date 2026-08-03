import { setTimeout as delay } from "node:timers/promises";

import { identify } from "../src/identity.js";
import { Store } from "../src/store.js";

const [databasePath, indexText, operationsText, workerCountText, projectCwd, startAtText] = process.argv.slice(2);
const index = Number(indexText);
const operations = Number(operationsText);
const workerCount = Number(workerCountText);
const startAt = Number(startAtText);
const who = identify({
  AGENT_FABRIC_SEAT: "concurrency-worker",
  AGENT_FABRIC_LABEL: `worker-${index}`,
}, projectCwd);

let store: Store | undefined;
let sent = 0;
let delivered = 0;
const failures: string[] = [];

try {
  store = new Store(databasePath);
  store.announce(who);
  if (Date.now() < startAt) await delay(startAt - Date.now());
  const recipient = `worker-${(index + 1) % workerCount}`;
  for (let operation = 0; operation < operations; operation += 1) {
    store.send(who, recipient, `worker-${index}-message-${operation}`);
    sent += 1;
    delivered += store.inbox(who, { limit: operations }).length;
  }
  const deadline = Date.now() + 30_000;
  while (delivered < operations && Date.now() < deadline) {
    delivered += store.inbox(who, { limit: operations }).length;
    if (delivered < operations) await delay(10);
  }
  if (delivered !== operations) {
    throw new Error(`expected ${operations} deliveries, observed ${delivered}`);
  }
} catch (error) {
  failures.push(error instanceof Error ? error.stack ?? error.message : String(error));
} finally {
  try {
    store?.close();
  } catch (error) {
    failures.push(error instanceof Error ? error.stack ?? error.message : String(error));
  }
}

console.log(JSON.stringify({ index, sent, delivered, failures }));
process.exitCode = failures.length === 0 ? 0 : 1;
