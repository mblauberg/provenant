/**
 * Two agents, two processes' worth of identity, one file. Proves the round trip
 * the old design could not complete: no daemon, no trust, no seat, no bootstrap.
 */
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

import { identify } from "./src/identity.js";
import { Store } from "./src/store.js";

const path = resolve(tmpdir(), `fabric-smoke-${String(process.pid)}/fabric.sqlite3`);
rmSync(resolve(path, ".."), { recursive: true, force: true });

const claude = identify({ AGENT_FABRIC_SEAT: "claude" }, process.cwd());
const codex = identify({ AGENT_FABRIC_SEAT: "codex" }, process.cwd());

const a = new Store(path);
const b = new Store(path);
a.announce(claude);
b.announce(codex);

console.log("agents:", a.agents(claude.project).map((agent) => agent.agentId).join(", "));

const sent = a.send(claude, "codex", "Review the login diff and tell me what breaks.", {
  kind: "request",
});
console.log("sent:", sent.messageId, "->", sent.recipients.join(", "));

const received = b.inbox(codex);
console.log("codex inbox:", received.length, "message(s)");
console.log("  from", received[0]?.from, ":", received[0]?.body);

b.send(codex, "claude", "The session cookie is never cleared on logout.", {
  kind: "response",
  replyTo: received[0]?.messageId,
});
b.acknowledge(codex, received[0]!.messageId, received[0]!.claimId!);
const back = a.inbox(claude);
console.log("claude inbox:", back.length, "reply");
console.log("  from", back[0]?.from, ":", back[0]?.body);
console.log("  same conversation:", back[0]?.conversationId === sent.messageId);
a.acknowledge(claude, back[0]!.messageId, back[0]!.claimId!);

console.log("inbox is now empty:", a.inbox(claude).length === 0);

a.createTeam(claude, "reviewers", ["claude", "codex"]);
const team = a.send(claude, "reviewers", "Standup in five.");
console.log("team send reached:", team.recipients.join(", "));

const task = a.createTask(claude, "Fix the logout cookie", { owner: "codex" });
a.updateTask(claude, task.taskId, "done", "shipped");
console.log("task:", JSON.stringify(a.tasks(claude.project)[0]));

console.log("activity log:");
for (const row of a.activity(claude.project).reverse()) {
  console.log(`  ${row.agentId} ${row.kind}: ${row.detail}`);
}

a.close();
b.close();
rmSync(resolve(path, ".."), { recursive: true, force: true });
