// A stand-in graph owner that dies at a chosen point of the gate handshake.
import { spawn } from "node:child_process";

import { launchGate } from "../src/graph/gates.js";

const [mode, token, dir] = process.argv.slice(2) as ["no-go" | "buffered-go", string, string];
const launch = launchGate(token, dir);
// Another child started after the wrapper: were the pipe inheritable, it would keep the wrapper waiting.
const bystander = spawn("sleep", ["30"], { stdio: "ignore", detached: true });
process.stdout.write(`${JSON.stringify({ wrapper: launch.pid, bystander: bystander.pid })}\n`, () => {
  if (mode === "no-go") process.kill(process.pid, "SIGKILL");
  launch.child.stdin!.end(`${JSON.stringify({ token, argv: ["sh", "-c", "echo ran > marker"], cwd: dir,
    out: `${dir}/out.txt`, err: `${dir}/err.txt`, exit: `${dir}/exit.json` })}\n`, () => process.kill(process.pid, "SIGKILL"));
});
