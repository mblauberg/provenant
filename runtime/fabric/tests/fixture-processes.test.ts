import { spawn } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";
import { parseProcessGroup, processesUnder, reapFixtureProcesses } from "./helpers/fixture-processes.js";
import { scratchRoot } from "./helpers/temp-root.js";

const roots: string[] = [];
afterEach(async () => {
  for (const root of roots.splice(0)) {
    await reapFixtureProcesses(root, 0);
    rmSync(root, { recursive: true, force: true });
  }
});

function sleeper(root: string, options: { detached?: boolean } = {}) {
  const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60000)", join(root, "run")], { stdio: "ignore", ...options });
  child.unref();
  return child.pid!;
}

it("finds and kills a fixture child that outlived its test, with its group", async () => {
  const root = scratchRoot("fabric-leak-");
  roots.push(root);
  mkdirSync(join(root, "run"));
  const pid = sleeper(root, { detached: true });
  expect(processesUnder(root).map((item) => item.pid)).toContain(pid);

  const leaked = await reapFixtureProcesses(root, 100);
  expect(leaked.map((item) => item.pid)).toEqual([pid]);
  expect(processesUnder(root)).toEqual([]);
});

it("reports nothing when every child has already exited", async () => {
  const root = scratchRoot("fabric-leak-");
  roots.push(root);
  const child = spawn(process.execPath, ["-e", "", join(root, "run")], { stdio: "ignore" });
  await new Promise((done) => child.once("close", done));
  expect(await reapFixtureProcesses(root, 100)).toEqual([]);
});

it("kills a non-detached leak by pid without signalling the runner's own group", async () => {
  const root = scratchRoot("fabric-leak-");
  roots.push(root);
  const pid = sleeper(root);
  const leaked = await reapFixtureProcesses(root, 100);
  expect(leaked.map((item) => item.pid)).toEqual([pid]);
  expect(processesUnder(root)).toEqual([]);
  // Still alive to assert this: the runner's group was not signalled.
  expect(() => process.kill(process.pid, 0)).not.toThrow();
});

it.each(["", "   \n", "abc", "0", "-5", "12x", "1.5", "NaN"])("treats %j as an unknown process group", (raw) => {
  expect(parseProcessGroup(raw)).toBeUndefined();
});

it("parses a real process group", () => {
  expect(parseProcessGroup(" 4242\n")).toBe(4242);
});

it.each(["", "garbage", "0"])("signals no group when the runner's group probe answers %j", async (answer) => {
  const root = scratchRoot("fabric-leak-");
  roots.push(root);
  const pid = sleeper(root, { detached: true });
  const kill = vi.spyOn(process, "kill");
  try {
    const leaked = await reapFixtureProcesses(root, 100, () => answer);
    expect(leaked.map((item) => item.pid)).toEqual([pid]);
    expect(kill.mock.calls.filter(([target]) => (target as number) <= 0)).toEqual([]);
  } finally {
    kill.mockRestore();
  }
  expect(processesUnder(root)).toEqual([]);
});
