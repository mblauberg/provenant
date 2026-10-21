// Vitest setup: keep fixture workspaces out of any enclosing git repository.
// A dispatched agent runs with TMPDIR inside its attempt directory under the
// live product checkout's .agent-run, so git (and runRoot) would resolve every
// mkdtemp workspace to that live checkout and write fixture runs into it.
import { execFileSync } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

function insideGitWorkTree(dir: string): boolean {
  try {
    execFileSync("git", ["rev-parse", "--git-dir"], { cwd: dir, stdio: "ignore" });
    return true;
  } catch { return false; }
}

const scratchRoots: string[] = [];

// Ordinary fixtures must never consult the developer's host federation config.
process.env.PROVENANT_HOST_LOCAL_ONLY = "1";

if (insideGitWorkTree(tmpdir())) {
  process.env.TMPDIR = realpathSync(mkdtempSync(join("/tmp", "fabric-test-tmp-")));
  scratchRoots.push(process.env.TMPDIR);
}

// An inherited live state location is always replaced; tests that set their own win.
const stateScratch = mkdtempSync(join(tmpdir(), "fabric-test-state-"));
scratchRoots.push(stateScratch);
process.env.AGENT_FABRIC_STATE_DIRECTORY = join(stateScratch, "state");
process.env.AGENT_FABRIC_STATE_ROOT = join(stateScratch, "root");

process.on("exit", () => {
  for (const root of scratchRoots) rmSync(root, { recursive: true, force: true });
});
