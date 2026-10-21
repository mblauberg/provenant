import { realpathSync } from "node:fs";
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

  it("allows a fixture run under the OS temp dir", () => {
    expect(() => assertFixtureRunRoot(`${temp}/fabric-lean-1/workspace/.agent-run`, [{ model: "fixture" }], temp))
      .not.toThrow();
  });

  it("allows a fixture run under the fixed system temp path", () => {
    expect(() => assertFixtureRunRoot(`${realpathSync("/tmp")}/fabric-lean-1/workspace/.agent-run`, [{ model: "fixture" }], temp))
      .not.toThrow();
  });

  it("allows real runs anywhere", () => {
    expect(() => assertFixtureRunRoot("/Users/example/Repos/project/.agent-run", [{ model: "gpt-5" }], temp))
      .not.toThrow();
  });
});
