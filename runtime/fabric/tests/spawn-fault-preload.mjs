// Test-only preload for launcher faults.
// PROVENANT_SPAWN_FAULT fails the launcher between recording a resumed turn and
// spawning its owner: =throw raises; =kill ends the launcher as a crash would.
// PROVENANT_OWNER_RECORD_FAULT=1 makes publishing the owner record fail.
import childProcess from "node:child_process";
import fs from "node:fs";
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
const rename = fs.renameSync;
fs.renameSync = function (from, to, ...rest) {
  if (process.env.PROVENANT_OWNER_RECORD_FAULT && String(to).endsWith("dispatch-owner.json"))
    throw Object.assign(new Error("owner record refused by fault injection"), { code: "EACCES" });
  return rename.call(this, from, to, ...rest);
};
syncBuiltinESMExports();
