import { execFileSync } from "node:child_process";
import { homedir } from "node:os";
import { resolve } from "node:path";

/**
 * Who am I, and which project am I in?
 *
 * Both answers are derived from the process, never provisioned. The old design
 * issued a capability token for this and needed a daemon to validate it, a seat
 * store to hold it, a trust record to authorise it and a bootstrap ceremony to
 * mint it, all of which could be forged by any process running as the same user
 * anyway. The environment already knows the answer.
 */
export interface Identity {
  readonly project: string;
  readonly agentId: string;
  readonly provider: string;
}

/**
 * The git toplevel, falling back to the directory itself.
 *
 * A directory that is not a repository is a perfectly good project; refusing to
 * work outside a repository was one of the reasons the tool could not be used in
 * arbitrary directories.
 */
export function projectRoot(cwd: string = process.cwd()): string {
  try {
    const top = execFileSync("git", ["rev-parse", "--show-toplevel"], {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    if (top.length > 0) return top;
  } catch {
    // Not a repository, or git is absent. The directory stands on its own.
  }
  return resolve(cwd);
}

export function identify(env: NodeJS.ProcessEnv = process.env, cwd?: string): Identity {
  const provider = env.AGENT_FABRIC_SEAT ?? env.AGENT_FABRIC_CLIENT_LABEL ?? "agent";
  // A label distinguishes several agents of the same provider in one project.
  // Without one they share an inbox, which is the right default for a solo run.
  const agentId = env.AGENT_FABRIC_LABEL ?? provider;
  return { project: projectRoot(cwd), agentId, provider };
}

export function databasePath(env: NodeJS.ProcessEnv = process.env): string {
  const stateDirectory = env.AGENT_FABRIC_STATE_DIRECTORY ??
    resolve(homedir(), ".local/state/agent-harness/fabric");
  return resolve(stateDirectory, "fabric.sqlite3");
}
