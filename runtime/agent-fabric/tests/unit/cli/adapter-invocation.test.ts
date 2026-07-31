import { join } from "node:path";
import { readFile, realpath, rm, writeFile } from "node:fs/promises";

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

  it.each([
    ["with common launch options", ["--cwd", "/fixture/worktree", "--timeout-ms", "15000"]],
    ["without optional launch options", []],
  ])("refuses Pi %s because it has no provider conformance policy", async (_description, options) => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "pi",
      ...options,
    ])).rejects.toThrow(
      "provider pi is registered but has no provider conformance policy and is disabled in the adapter compatibility config",
    );
  });

  it("builds the minimal Agy invocation without optional fields", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(configPath, "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: [agy]\n");

    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--agents-home", fixture.directory,
      "--product-root", fixture.directory,
      "--instance-root", fixture.directory,
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--mode", "accept-edits",
      "--model", "gemini",
      "--prompt", "review",
    ], {
      verifyProvider: vi.fn(async () => ({} as never)),
    })).resolves.toStrictEqual({
      executable: await fixtureExecutable(fixture, "agy"),
      args: [
        "--sandbox",
        "--mode", "accept-edits",
        "--model", "gemini",
        "--print-timeout", "1800s",
        "--print", "review",
      ],
    });
  });

  it("uses the default provider conformance path for a supported Agy provider", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(configPath, "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: [agy]\n");
    const executable = await realpath(await fixtureExecutable(fixture, "agy"));

    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--agents-home", fixture.directory,
      "--product-root", fixture.directory,
      "--instance-root", fixture.directory,
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--mode", "plan",
      "--model", "gemini",
      "--prompt", "review",
    ])).rejects.toThrow(`provider signature is invalid: ${executable}`);
  });

  it("builds the minimal Cursor invocation without optional fields", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(
      configPath,
      "schemaVersion: 1\nallowedAdapters: [cursor-agent]\nactiveAdapters: [cursor-agent]\n",
    );

    await expect(resolveAdapterInvocationCli([
      "--adapter", "cursor",
      "--agents-home", fixture.directory,
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--mode", "plan",
      "--model", "composer",
      "--prompt", "review",
    ], {
      verifyProvider: vi.fn(async () => ({} as never)),
    })).resolves.toStrictEqual({
      executable: await fixtureExecutable(fixture, "cursor-agent"),
      args: [
        "--print",
        "--output-format", "stream-json",
        "--sandbox", "enabled",
        "--trust",
        "--mode", "plan",
        "--model", "composer",
        "review",
      ],
    });
  });

  it("builds the minimal Kiro invocation without optional fields", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(
      configPath,
      "schemaVersion: 1\nallowedAdapters: [kiro-acp]\nactiveAdapters: [kiro-acp]\n",
    );

    await expect(resolveAdapterInvocationCli([
      "--adapter", "kiro",
      "--agents-home", fixture.directory,
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
      "--model", "qwen3-coder",
      "--agent-engine", "v2",
    ], {
      verifyProvider: vi.fn(async () => ({} as never)),
    })).resolves.toStrictEqual({
      executable: await fixtureExecutable(fixture, "kiro-acp"),
      args: ["acp", "--agent-engine", "v2", "--model", "qwen3-coder"],
    });
  });

  it.each([
    ["agy", ["--mode", "plan", "--model", "gemini", "--prompt", "review", "--agent-engine", "v2"], "--agent-engine"],
    ["cursor", ["--mode", "plan", "--model", "composer", "--prompt", "review", "--log-file", "agy.log"], "--log-file"],
    ["kiro", ["--model", "qwen", "--agent-engine", "v2", "--mode", "plan"], "--mode"],
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

  it("explains when a registered provider has no invocation payload", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "opencode-acp",
    ])).rejects.toThrow("provider opencode-acp is registered but has no invocation payload");
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

  it("emits a successful JSON payload through the main CLI dispatch", async () => {
    const payload = {
      executable: "/fixture/agy",
      args: ["--sandbox", "--mode", "plan", "--model", "gemini", "--print-timeout", "1800s", "--print", "review"],
    };
    vi.doMock("../../../src/cli/adapter-invocation.ts", () => ({
      resolveAdapterInvocationCli: vi.fn(async () => payload),
    }));
    vi.resetModules();
    const stdout = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    const previousArgv = process.argv;
    const previousExitCode = process.exitCode;
    process.argv = [process.execPath, "main.ts", "adapter", "invocation", "--adapter", "agy"];
    process.exitCode = undefined;
    try {
      await import("../../../src/cli/main.ts");
      expect(stdout.mock.calls.map(([chunk]) => String(chunk)).join("")).toBe(`${JSON.stringify(payload)}\n`);
    } finally {
      process.argv = previousArgv;
      process.exitCode = previousExitCode;
      stdout.mockRestore();
      vi.doUnmock("../../../src/cli/adapter-invocation.ts");
      vi.resetModules();
    }
  });

  it("enumerates provider options in the main CLI usage", async () => {
    const result = await runSourceCli([]);

    expect(result.exitCode).toBe(1);
    expect(result.stderr).toContain(
      "adapter invocation --adapter agy|cursor|kiro [agy: --mode plan|accept-edits --model MODEL --prompt TEXT",
    );
    expect(result.stderr).toContain("cursor: --mode plan|ask --model MODEL --prompt TEXT");
    expect(result.stderr).toContain("kiro: --model MODEL --agent-engine v2");
  });

  it.each([
    ["agy", ["--model", "gemini", "--prompt", "review"], "--mode"],
    ["agy", ["--mode", "plan", "--prompt", "review"], "--model"],
    ["agy", ["--mode", "plan", "--model", "gemini"], "--prompt"],
    ["cursor", ["--model", "composer", "--prompt", "review"], "--mode"],
    ["cursor", ["--mode", "ask", "--prompt", "review"], "--model"],
    ["cursor", ["--mode", "ask", "--model", "composer"], "--prompt"],
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
    ["kiro", ["--model", "qwen", "--agent-engine", " \t "], "--agent-engine"],
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
      "--adapter", "agy",
      "--mode", "plan",
      "--model", "gemini",
      "--prompt", "review",
      "--timeout-ms", "0",
    ])).rejects.toThrow("adapter invocation for agy received invalid timeout-ms value: 0");
  });

  it.each(["1.5", "not-a-number"])("refuses a non-integer timeout value: %s", async (value) => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--mode", "plan",
      "--model", "gemini",
      "--prompt", "review",
      "--timeout-ms", value,
    ])).rejects.toThrow(`adapter invocation for agy received invalid timeout-ms value: ${value}`);
  });

  it("refuses a timeout above Node's timer maximum", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--mode", "plan",
      "--model", "gemini",
      "--prompt", "review",
      "--timeout-ms", "2147483648",
    ])).rejects.toThrow(
      "adapter invocation for agy received invalid timeout-ms value: 2147483648; maximum is 2147483647ms",
    );
  });

  it("rejects a trailing option flag", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--cwd",
    ])).rejects.toThrow("adapter invocation requires a value for --cwd");
  });

  it("rejects an empty option value", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--cwd", "",
    ])).rejects.toThrow("adapter invocation requires a value for --cwd");
  });

  it("rejects an option value beginning with a dash", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--cwd", "--not-a-path",
    ])).rejects.toThrow("adapter invocation requires a value for --cwd");
  });

  it("rejects a duplicated option", async () => {
    await expect(resolveAdapterInvocationCli([
      "--adapter", "agy",
      "--cwd", "/one",
      "--cwd", "/two",
    ])).rejects.toThrow("adapter invocation received duplicate option: --cwd");
  });

});
