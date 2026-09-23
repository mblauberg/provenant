import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const shim = fileURLToPath(new URL("../../../skills/orchestrate/scripts/bin/ps", import.meta.url));

/** Preserve the system ps result when available; use the seatbelt-safe PATH shim otherwise. */
export function psOutput(args, env = process.env) {
  const options = {
    encoding: "utf8", timeout: 5_000, stdio: ["ignore", "pipe", "ignore"], env,
  };
  try {
    return execFileSync("/bin/ps", args, options);
  } catch {
    const fallbackEnv = existsSync(shim)
      ? { ...env, PATH: `${dirname(shim)}:${env.PATH ?? ""}` }
      : env;
    return execFileSync("ps", args, { ...options, env: fallbackEnv });
  }
}
