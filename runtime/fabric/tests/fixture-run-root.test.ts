import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, rmdirSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { assertFixtureRunRoot } from "../src/execution.js";

describe("assertFixtureRunRoot", () => {
  const temp = "/private/var/folders/xx/T";

  it("refuses a fixture model run rooted in a project .agent-run", () => {
    expect(() => assertFixtureRunRoot("/Users/example/Repos/project/.agent-run", [{ model: "fixture" }], temp))
      .toThrow(/fixture run refused.*issue #1002/u);
  });

  it("refuses a fixture cli_version run rooted in a project .agent-run", () => {
    expect(() => assertFixtureRunRoot("/Users/example/Repos/project/.agent-run", [{ cli_version: "fixture" }], temp))
      .toThrow(/fixture run refused/u);
  });

  it("refuses a fixture run whose temp dir itself sits inside a .agent-run tree", () => {
    expect(() => assertFixtureRunRoot(
      "/a/.agent-run/att/tmp/ws/.agent-run",
      [{ model: "fixture" }],
      "/a/.agent-run/att/tmp",
    )).toThrow(/fixture run refused/u);
  });

  it("resolves a symlinked run root before checking the temp boundary", () => {
    const tempRoot = mkdtempSync(join(tmpdir(), "fabric-fixture-link-"));
    const projectRunParent = join(process.cwd(), ".agent-run");
    const projectRunParentExisted = existsSync(projectRunParent);
    const projectRunRoot = join(projectRunParent, `fixture-link-${Date.now()}`);
    try {
      mkdirSync(projectRunRoot, { recursive: true });
      const link = join(tempRoot, "project-run");
      symlinkSync(projectRunRoot, link);
      expect(() => assertFixtureRunRoot(join(link, "runs"), [{ model: "fixture" }], tempRoot))
        .toThrow(/acceptable temp root/u);
    } finally {
      rmSync(projectRunRoot, { recursive: true, force: true });
      if (!projectRunParentExisted) {
        try { rmdirSync(projectRunParent); } catch { /* another test may have populated it */ }
      }
      rmSync(tempRoot, { recursive: true, force: true });
    }
  });

  it("allows a fixture run under the OS temp dir", () => {
    expect(() => assertFixtureRunRoot(`${temp}/fabric-lean-1/workspace/.agent-run`, [{ model: "fixture" }], temp))
      .not.toThrow();
  });

  it("allows a fixture run under the fixed system temp path", () => {
    expect(() => assertFixtureRunRoot(`${realpathSync("/tmp")}/fabric-lean-1/workspace/.agent-run`, [{ model: "fixture" }], temp))
      .not.toThrow();
  });

  it("allows a fixture run under FABRIC_TEST_TMPDIR", () => {
    const saved = process.env.FABRIC_TEST_TMPDIR;
    const explicit = realpathSync(mkdtempSync(join(tmpdir(), "fabric-explicit-root-")));
    process.env.FABRIC_TEST_TMPDIR = explicit;
    try {
      expect(() => assertFixtureRunRoot(`${explicit}/fabric-lean-1/workspace/.agent-run`, [{ model: "fixture" }], temp))
        .not.toThrow();
    } finally {
      if (saved === undefined) delete process.env.FABRIC_TEST_TMPDIR;
      else process.env.FABRIC_TEST_TMPDIR = saved;
      rmSync(explicit, { recursive: true, force: true });
    }
  });

  it("allows a fixture run inside a project's throwaway .agent-run/scratch tree", () => {
    expect(() => assertFixtureRunRoot(
      "/repo/.agent-run/scratch/hosts-lanes-1/workshop/Repos/project/.agent-run",
      [{ model: "fixture" }],
      temp,
    )).not.toThrow();
  });

  it("refuses a fixture run rooted at the scratch directory itself", () => {
    expect(() => assertFixtureRunRoot("/repo/.agent-run/scratch", [{ model: "fixture" }], temp))
      .toThrow(/fixture run refused/u);
  });

  it("allows real runs anywhere", () => {
    expect(() => assertFixtureRunRoot("/Users/example/Repos/project/.agent-run", [{ model: "gpt-5" }], temp))
      .not.toThrow();
  });
});
