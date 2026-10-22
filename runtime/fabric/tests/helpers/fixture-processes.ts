import { execFileSync } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";

export interface FixtureProcess {
  pid: number;
  pgid: number;
  command: string;
}

/** Live processes other than this one whose command line names `root`. */
export function processesUnder(root: string): FixtureProcess[] {
  const listing = execFileSync("ps", ["-axo", "pid=,pgid=,command="], {
    encoding: "utf8",
    env: { ...process.env, LC_ALL: "C", LANG: "C" },
    maxBuffer: 16 * 1024 * 1024,
  });
  const found: FixtureProcess[] = [];
  for (const line of listing.split("\n")) {
    const match = /^\s*(\d+)\s+(\d+)\s+(.*)$/u.exec(line);
    if (!match || Number(match[1]) === process.pid || !match[3]!.includes(root)) continue;
    found.push({ pid: Number(match[1]), pgid: Number(match[2]), command: match[3]! });
  }
  return found;
}

/** A process group id is a positive integer; anything else means unknown. */
export function parseProcessGroup(raw: string): number | undefined {
  const text = raw.trim();
  if (!/^[0-9]+$/u.test(text)) return undefined;
  const value = Number(text);
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

function probeOwnProcessGroup(): string {
  try {
    return execFileSync("ps", ["-o", "pgid=", "-p", String(process.pid)], { encoding: "utf8" });
  } catch { return ""; }
}

function kill(pid: number, signal: NodeJS.Signals): void {
  try { process.kill(pid, signal); } catch { /* already gone */ }
}

/**
 * Wait briefly for fixture processes under `root` to exit on their own, then
 * kill what is left, process groups included. Returns what had to be killed:
 * a non-empty answer means a test let a child outlive it.
 */
export async function reapFixtureProcesses(root: string, graceMs = 2_000, probe: () => string = probeOwnProcessGroup): Promise<FixtureProcess[]> {
  const deadline = Date.now() + graceMs;
  let remaining = processesUnder(root);
  while (remaining.length && Date.now() < deadline) {
    await delay(50);
    remaining = processesUnder(root);
  }
  // A non-detached leak shares the runner's group; signalling that group would kill the runner and its workers.
  // If the runner's own group is unknown, signal no group at all.
  const own = parseProcessGroup(probe());
  for (const item of remaining) {
    if (own !== undefined && item.pgid > 1 && item.pgid !== own) kill(-item.pgid, "SIGKILL");
    kill(item.pid, "SIGKILL");
  }
  return remaining;
}

export function describeLeaks(leaked: FixtureProcess[]): string {
  return `fixture processes outlived their test: ${leaked.map((item) => `${item.pid} ${item.command.slice(0, 160)}`).join("; ")}`;
}
