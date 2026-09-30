// Test-only preload: fail the launcher at the point between recording a
// resumed turn and spawning its owner. PROVENANT_SPAWN_FAULT=throw raises;
// =kill ends the launcher as a crash would.
import childProcess from "node:child_process";
import { syncBuiltinESMExports } from "node:module";

const fault = process.env.PROVENANT_SPAWN_FAULT;
const original = childProcess.spawn;
childProcess.spawn = function (command, args, ...rest) {
  if (fault && Array.isArray(args) && args.includes("--resume")) {
    if (fault === "kill") process.kill(process.pid, "SIGKILL");
    throw new Error("spawn refused by fault injection");
  }
  return original.call(this, command, args, ...rest);
};
syncBuiltinESMExports();
