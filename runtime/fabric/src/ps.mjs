import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const shim = fileURLToPath(new URL("../../../skills/orchestrate/scripts/bin/ps", import.meta.url));
/** Errors that mean /bin/ps could not start at all (seatbelt refuses setuid exec with EPERM). */
const UNSTARTABLE = new Set(["EPERM", "EACCES", "ENOENT"]);

/** Use the system ps; only when it cannot start, run the bundled libproc shim by absolute path. */
export function psOutput(args, env = process.env) {
  const options = {
    encoding: "utf8", timeout: 5_000, stdio: ["ignore", "pipe", "ignore"], env,
  };
  try {
    return execFileSync("/bin/ps", args, options);
  } catch (error) {
    if (!UNSTARTABLE.has(error?.code) || !existsSync(shim)) throw error;
    return execFileSync(shim, args, { ...options, env: { ...env, PATH: env.PATH || "/usr/bin:/bin" } });
  }
}
