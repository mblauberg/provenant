import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, it } from "vitest";

import { catalogueSnapshot, liveModels } from "../src/catalogue.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

it("ignores a blank product root instead of running a cwd-relative script", () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-blank-root-"));
  const oldCwd = process.cwd();
  try {
    mkdirSync(join(root, "scripts"));
    writeFileSync(join(root, "scripts", "model_route.py"),
      `from pathlib import Path\nPath(${JSON.stringify(join(root, "executed"))}).write_text("yes")\nprint("{}")\n`);
    process.chdir(root);
    const snapshot = catalogueSnapshot(undefined, {
      AGENT_FABRIC_PRODUCT_ROOT: "", AGENT_FABRIC_INSTANCE_ROOT: root,
    });
    expect(snapshot.sources).toContain(join(repositoryRoot, "config", "model-routing.json"));
    expect(existsSync(join(root, "executed"))).toBe(false);
  } finally { process.chdir(oldCwd); rmSync(root, { recursive: true, force: true }); }
});

it("caches a failed snapshot for the same source stamp", () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-failed-snapshot-"));
  try {
    const env = { AGENT_FABRIC_INSTANCE_ROOT: root, AGENT_FABRIC_STATE_ROOT: root };
    const first = catalogueSnapshot(root, env);
    expect(first.drift).not.toEqual([]);
    expect(catalogueSnapshot(root, env)).toBe(first);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

it("lists an adapter's live models compactly, groups large families and caches the probe", async () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-live-models-"));
  try {
    const bin = join(root, "bin");
    mkdirSync(bin);
    const listing = ["opencode-go/deepseek-v4.1-flash", "opencode-go/kimi-k3",
      ...Array.from({ length: 30 }, (_, index) => `openrouter/vendor/model-${index}`)].join("\\n");
    const script = join(bin, "opencode");
    writeFileSync(script, `#!/bin/sh\ncase "$1" in --version) echo 1.2.3;; models) printf '${listing}\\n';; *) :;; esac\n`);
    chmodSync(script, 0o755);
    const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, AGENT_FABRIC_STATE_ROOT: root };
    const first = await liveModels("opencode", { root: repositoryRoot, env });
    expect(first.digest.split("\n")).toEqual([
      "opencode: 32 live models",
      "opencode-go/ (2): deepseek-v4.1-flash kimi-k3",
      "openrouter/ (30): pass match to list",
      "dispatch any as model \"opencode/<id>\"; an uncatalogued id runs with a note",
    ]);
    writeFileSync(script, "#!/bin/sh\ncase \"$1\" in --version) echo 1.2.3;; *) exit 1;; esac\n");
    const filtered = await liveModels("opencode", { root: repositoryRoot, env, match: "model-2" });
    expect(filtered.digest).toContain("opencode: 11 of 32 live models match model-2 (cached)");
    expect(filtered.digest).toContain("openrouter/vendor/model-2 openrouter/vendor/model-20 ");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

it("sends Claude to native subagents and names adapters without a live list", async () => {
  const env = { ...process.env, AGENT_FABRIC_STATE_ROOT: mkdtempSync(join(tmpdir(), "fabric-live-none-")) };
  expect((await liveModels("claude", { root: repositoryRoot, env })).digest)
    .toMatch(/^claude: no live list; run Claude models as native subagents \(Agent tool\)\ncatalogued: /u);
  expect((await liveModels("copilot", { root: repositoryRoot, env })).digest).toMatch(/^copilot: no live list\ncatalogued: /u);
  expect((await liveModels("nope", { root: repositoryRoot, env })).digest).toMatch(/^nope: unknown adapter; known: agy, claude, /u);
});

it("keeps a huge live list within the output budget and says what it left out", async () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-live-budget-"));
  try {
    const bin = join(root, "bin");
    mkdirSync(bin);
    const listing = Array.from({ length: 1200 }, (_, index) => `provider${index}/model-${index}`).join("\\n");
    const script = join(bin, "opencode");
    writeFileSync(script, `#!/bin/sh\ncase "$1" in --version) echo 1.2.3;; models) printf '${listing}\\n';; *) :;; esac\n`);
    chmodSync(script, 0o755);
    const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, AGENT_FABRIC_STATE_ROOT: root };
    const { digest } = await liveModels("opencode", { root: repositoryRoot, env });
    expect(digest.length).toBeLessThanOrEqual(4096);
    expect(digest).toMatch(/\n\d+ more groups \(\d+ models\) omitted; pass match to narrow\n/u);
    expect(digest.split("\n").at(-1)).toContain('dispatch any as model "opencode/<id>"');
  } finally { rmSync(root, { recursive: true, force: true }); }
});
