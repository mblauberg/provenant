import { join } from "node:path";
import { readFile, rm, writeFile } from "node:fs/promises";

import { afterEach, describe, expect, it, vi } from "vitest";
import { parse } from "yaml";

import { resolveAdapterInvocationCli } from "../../../src/cli/adapter-invocation.ts";
import { createResolvedStage4Compatibility } from "../../support/stage4-pi-agy-testkit.ts";
import { createCursorKiroCompatibilityFixture } from "../../support/stage4-cursor-kiro-testkit.ts";
import { runSourceCli } from "../../support/cli-process.ts";

type Fixture = Awaited<ReturnType<typeof createResolvedStage4Compatibility>> |
  Awaited<ReturnType<typeof createCursorKiroCompatibilityFixture>>;

const fixtures: Fixture[] = [];

afterEach(async () => {
  await Promise.all(fixtures.splice(0).map((fixture) => rm(fixture.directory, { recursive: true, force: true })));
});

async function fixtureExecutable(fixture: Fixture, adapterId: string): Promise<string> {
  const document: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
  if (
    typeof document !== "object" || document === null ||
    !("adapters" in document) || typeof document.adapters !== "object" || document.adapters === null
  ) {
    throw new TypeError("fixture compatibility document has no adapters");
  }
  const adapter: unknown = Reflect.get(document.adapters, adapterId);
  if (
    typeof adapter !== "object" || adapter === null ||
    !("implementation" in adapter) || typeof adapter.implementation !== "object" || adapter.implementation === null ||
    !("executable" in adapter.implementation) || typeof adapter.implementation.executable !== "string"
  ) {
    throw new TypeError(`fixture has no executable for ${adapterId}`);
  }
  return adapter.implementation.executable;
}

describe("adapter invocation resolver CLI", () => {
  it("builds an Agy invocation from provider-specific options", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(configPath, "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: [agy]\n");
    const verifyProvider = vi.fn(async () => ({} as never));

    const invocation = await resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--mode", "plan",
      "--model", "Gemini 3.5 Flash (High)",
      "--prompt", "Review this change",
      "--resume-reference", "conversation-42",
      "--log-file", "/tmp/agy-invocation.log",
      "--cwd", "/fixture/worktree",
      "--timeout-ms", "90000",
    ], {
      verifyProvider,
    });

    expect(verifyProvider).toHaveBeenCalledWith(expect.objectContaining({ adapterId: "agy" }));
    expect(invocation).toEqual({
      executable: await fixtureExecutable(fixture, "agy"),
      args: [
        "--sandbox",
        "--log-file", "/tmp/agy-invocation.log",
        "--mode", "plan",
        "--model", "Gemini 3.5 Flash (High)",
        "--conversation", "conversation-42",
        "--print-timeout", "90s",
        "--print", "Review this change",
      ],
      cwd: "/fixture/worktree",
      timeoutMs: 90_000,
    });
  });

  it("builds a Cursor invocation from provider-specific options", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(
      configPath,
      "schemaVersion: 1\nallowedAdapters: [cursor-agent]\nactiveAdapters: [cursor-agent]\n",
    );
    const verifyProvider = vi.fn(async () => ({} as never));

    const invocation = await resolveAdapterInvocationCli([
      "--adapter", "cursor",
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--mode", "ask",
      "--model", "cursor-grok-4.5-high",
      "--prompt", "Review this change",
      "--resume-reference", "thread-42",
      "--cwd", "/fixture/worktree",
      "--timeout-ms", "45000",
    ], {
      verifyProvider,
    });

    expect(verifyProvider).toHaveBeenCalledWith(expect.objectContaining({ adapterId: "cursor-agent" }));
    expect(invocation).toEqual({
      executable: await fixtureExecutable(fixture, "cursor-agent"),
      args: [
        "--print",
        "--output-format", "stream-json",
        "--sandbox", "enabled",
        "--trust",
        "--mode", "ask",
        "--model", "cursor-grok-4.5-high",
        "--workspace", "/fixture/worktree",
        "--resume", "thread-42",
        "Review this change",
      ],
      cwd: "/fixture/worktree",
      timeoutMs: 45_000,
    });
  });

  it("builds a Kiro invocation without accepting a mode", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(
      configPath,
      "schemaVersion: 1\nallowedAdapters: [kiro-acp]\nactiveAdapters: [kiro-acp]\n",
    );
    const verifyProvider = vi.fn(async () => ({} as never));

    const invocation = await resolveAdapterInvocationCli([
      "--adapter", "kiro",
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--model", "qwen3-coder",
      "--agent-engine", "v2",
      "--cwd", "/fixture/worktree",
      "--timeout-ms", "30000",
    ], {
      verifyProvider,
    });

    expect(verifyProvider).toHaveBeenCalledWith(expect.objectContaining({ adapterId: "kiro-acp" }));
    expect(invocation).toEqual({
      executable: await fixtureExecutable(fixture, "kiro-acp"),
      args: ["acp", "--agent-engine", "v2", "--model", "qwen3-coder"],
      cwd: "/fixture/worktree",
      timeoutMs: 30_000,
    });
  });

  it("builds a Pi RPC launch from only common launch options", async () => {
    const fixture = await createResolvedStage4Compatibility("pi-rpc");
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(
      configPath,
      "schemaVersion: 1\nallowedAdapters: [pi-rpc]\nactiveAdapters: [pi-rpc]\n",
    );
    const verifyProvider = vi.fn(async () => ({} as never));

    const invocation = await resolveAdapterInvocationCli([
      "--adapter", "pi",
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--cwd", "/fixture/worktree",
      "--timeout-ms", "15000",
    ], {
      verifyProvider,
    });

    expect(verifyProvider).toHaveBeenCalledWith(expect.objectContaining({ adapterId: "pi-rpc" }));
    expect(invocation).toEqual({
      executable: await fixtureExecutable(fixture, "pi-rpc"),
      args: ["--mode", "rpc"],
      cwd: "/fixture/worktree",
      timeoutMs: 15_000,
    });
  });

  it.each([
    ["agy", ["--mode", "plan", "--model", "gemini", "--prompt", "review", "--agent-engine", "v2"], "--agent-engine"],
    ["cursor", ["--mode", "plan", "--model", "composer", "--prompt", "review", "--log-file", "agy.log"], "--log-file"],
    ["kiro", ["--model", "qwen", "--agent-engine", "v2", "--mode", "plan"], "--mode"],
    ["pi", ["--model", "qwen"], "--model"],
  ])("refuses fields outside the %s invocation shape", async (provider, providerArguments, unknownOption) => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", provider,
      ...providerArguments,
    ])).rejects.toThrow(`adapter invocation for ${provider} received unknown option: ${unknownOption}`);
  });

  it("refuses an unknown provider", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "unknown",
    ])).rejects.toThrow("unknown provider: unknown");
  });

  it("requires a provider selection", async () => {
    await expect(resolveAdapterInvocationCli([])).rejects.toThrow(
      "adapter invocation requires --adapter",
    );
  });

  it("names the Copilot adapter exception", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "copilot",
    ])).rejects.toThrow("copilot is not supported; it has no adapter");
  });

  it("routes adapter invocation through the main CLI", async () => {
    const result = await runSourceCli([
      "adapter", "invocation", "--adapter", "copilot",
    ]);

    expect(result.exitCode).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("copilot is not supported; it has no adapter");
  });

  it.each([
    ["agy", ["--model", "gemini", "--prompt", "review"], "--mode"],
    ["agy", ["--mode", "plan", "--prompt", "review"], "--model"],
    ["agy", ["--mode", "plan", "--model", "gemini"], "--prompt"],
    ["cursor", ["--model", "composer", "--prompt", "review"], "--mode"],
    ["cursor", ["--mode", "ask", "--prompt", "review"], "--model"],
    ["kiro", ["--agent-engine", "v2"], "--model"],
    ["kiro", ["--model", "qwen"], "--agent-engine"],
  ])("refuses a missing required %s field before compatibility I/O", async (provider, providerArguments, field) => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", provider,
      ...providerArguments,
    ])).rejects.toThrow(`adapter invocation for ${provider} requires ${field}`);
  });

  it.each([
    ["agy", ["--mode", "plan", "--model", "   ", "--prompt", "review"], "--model"],
    ["agy", ["--mode", "plan", "--model", "gemini", "--prompt", " \t "], "--prompt"],
    ["cursor", ["--mode", "ask", "--model", "\t", "--prompt", "review"], "--model"],
    ["kiro", ["--model", "  ", "--agent-engine", "v2"], "--model"],
  ])("refuses a blank required %s field before compatibility I/O", async (provider, providerArguments, field) => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", provider,
      ...providerArguments,
    ])).rejects.toThrow(`adapter invocation for ${provider} requires ${field}`);
  });

  it.each([
    ["agy", ["--mode", "ask", "--model", "gemini", "--prompt", "review"], "mode", "ask"],
    ["cursor", ["--mode", "accept-edits", "--model", "composer", "--prompt", "review"], "mode", "accept-edits"],
    ["kiro", ["--model", "qwen", "--agent-engine", "v3"], "agent-engine", "v3"],
  ])("refuses an invalid %s enum value", async (provider, providerArguments, field, value) => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", provider,
      ...providerArguments,
    ])).rejects.toThrow(`adapter invocation for ${provider} received invalid ${field} value: ${value}`);
  });

  it("refuses a non-positive timeout before compatibility I/O", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "pi",
      "--timeout-ms", "0",
    ])).rejects.toThrow("adapter invocation for pi received invalid timeout-ms value: 0");
  });
});
