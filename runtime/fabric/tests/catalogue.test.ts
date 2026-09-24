import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, it } from "vitest";

import { catalogueSnapshot } from "../src/catalogue.js";

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
