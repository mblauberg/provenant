/** Interpreter selection and environment for Python-owned harness operations. */
import { execFile, execFileSync } from "node:child_process";
import { accessSync, constants, lstatSync, statSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";
import { promisify } from "node:util";
import { withoutGitRedirects } from "./identity.js";

const execFileAsync = promisify(execFile);

export function pythonOwnerEnvironment(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return { ...withoutGitRedirects(env), PYTHONNOUSERSITE: "1" };
}

function pythonOwnerCommand(root: string, env: NodeJS.ProcessEnv) {
  const helper = join(root, "scripts/lib/harness-python.sh");
  const metadata = lstatSync(helper);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`Python owner selector must be a regular local file: ${helper}`);
  }
  return {
    args: ["-c", 'source "$1"; run_stdlib -I -c "import sys; print(sys.executable)"', "provenant-python-owner", helper],
    options: { cwd: root, env: pythonOwnerEnvironment(env), encoding: "utf8" as const, timeout: 10_000, maxBuffer: 64 * 1024 },
  };
}

function selectedPython(stdout: string): string {
  const selected = resolve(stdout.trim());
  const metadata = statSync(selected);
  accessSync(selected, constants.X_OK);
  if (!isAbsolute(selected) || !metadata.isFile()) throw new Error("selector returned a non-file");
  return selected;
}

function pythonOwnerError(error: unknown): Error {
  return new Error(`Python execution owner is unavailable: ${error instanceof Error ? error.message : String(error)}`, { cause: error });
}

export async function pythonOwner(root: string, env: NodeJS.ProcessEnv): Promise<string> {
  const { args, options } = pythonOwnerCommand(root, env);
  try { return selectedPython((await execFileAsync("/bin/bash", args, options)).stdout); }
  catch (error) { throw pythonOwnerError(error); }
}

/** Synchronous validation uses the same interpreter selector as dispatch. */
export function pythonOwnerSync(root: string, env: NodeJS.ProcessEnv): string {
  const { args, options } = pythonOwnerCommand(root, env);
  try { return selectedPython(execFileSync("/bin/bash", args, options)); }
  catch (error) { throw pythonOwnerError(error); }
}

