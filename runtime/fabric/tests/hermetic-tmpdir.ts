// Vitest setup: keep fixture workspaces out of any enclosing git repository.
// A dispatched agent runs with TMPDIR inside its attempt directory under the
// live product checkout's .agent-run, so git (and runRoot) would resolve every
// mkdtemp workspace to that live checkout and write fixture runs into it.
import { execFileSync } from "node:child_process";
import { mkdtempSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

function insideGitWorkTree(dir: string): boolean {
  try {
    execFileSync("git", ["rev-parse", "--git-dir"], { cwd: dir, stdio: "ignore" });
    return true;
  } catch { return false; }
}

if (insideGitWorkTree(tmpdir())) {
  process.env.TMPDIR = realpathSync(mkdtempSync(join("/tmp", "fabric-test-tmp-")));
}

// Tests that do not name a state location must never read the live project store.
const stateScratch = mkdtempSync(join(tmpdir(), "fabric-test-state-"));
process.env.AGENT_FABRIC_STATE_DIRECTORY ??= join(stateScratch, "state");
process.env.AGENT_FABRIC_STATE_ROOT ??= join(stateScratch, "root");
