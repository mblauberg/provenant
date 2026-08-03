import { readFile, rm, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

import { afterEach, describe, expect, it } from "vitest";
import { parse, stringify } from "yaml";

import { loadAdapterModelConstraints, assessAdapterModelPolicy } from "../../src/adapters/model-selection.ts";
import { createCursorKiroCompatibilityFixture } from "../support/stage4-cursor-kiro-testkit.ts";

const fixtures: Array<{ directory: string; compatibilityPath: string }> = [];

afterEach(async () => {
  await Promise.all(fixtures.splice(0).map((fixture) => rm(fixture.directory, { recursive: true, force: true })));
});

describe("adapter model matching conformance", () => {
  it("matches Python-style wildcards case-insensitively", () => {
    expect(assessAdapterModelPolicy({
      modelFamily: "cursor-composer",
      modelId: "COMPOSER-2-HIGH",
      allowedFamilies: ["cursor-composer"],
      allowedModelPatterns: ["composer-*-high"],
      requiresExplicitModel: true,
    })).toEqual({ allowed: true, reason: "allowed" });
  });

  it("permits account-default dispatch only when an explicit model is not required", () => {
    // ChatGPT-subscription Codex rejects explicit model ids; an absent id is
    // an account-default dispatch and skips the pattern gate (#190).
    expect(assessAdapterModelPolicy({
      modelFamily: "openai",
      modelId: null,
      allowedFamilies: ["openai"],
      allowedModelPatterns: ["gpt-*", "codex*"],
      requiresExplicitModel: false,
    })).toEqual({ allowed: true, reason: "allowed" });
    expect(assessAdapterModelPolicy({
      modelFamily: "openai",
      modelId: null,
      allowedFamilies: ["openai"],
      allowedModelPatterns: ["gpt-*", "codex*"],
      requiresExplicitModel: true,
    })).toEqual({ allowed: false, reason: "model-required" });
    // The family gate still applies to account-default dispatch.
    expect(assessAdapterModelPolicy({
      modelFamily: "anthropic",
      modelId: null,
      allowedFamilies: ["openai"],
      allowedModelPatterns: ["gpt-*"],
      requiresExplicitModel: false,
    })).toEqual({ allowed: false, reason: "family-forbidden" });
    // Account-default is exclusive: ANY explicit id fails closed, even one
    // matching the allow-list, because the runtime rejects explicit ids.
    expect(assessAdapterModelPolicy({
      modelFamily: "openai",
      modelId: "gpt-5.6-sol",
      allowedFamilies: ["openai"],
      allowedModelPatterns: ["gpt-*"],
      requiresExplicitModel: false,
    })).toEqual({ allowed: false, reason: "model-forbidden" });
    expect(assessAdapterModelPolicy({
      modelFamily: "openai",
      modelId: "grok-4",
      allowedFamilies: ["openai"],
      allowedModelPatterns: ["gpt-*"],
      requiresExplicitModel: false,
    })).toEqual({ allowed: false, reason: "model-forbidden" });
  });

  it("never bridges the open-weight family through an absent model", () => {
    expect(assessAdapterModelPolicy({
      modelFamily: "zhipu",
      modelId: null,
      allowedFamilies: ["open-weight"],
      allowedModelPatterns: ["glm-*"],
      requiresExplicitModel: false,
    })).toEqual({ allowed: false, reason: "family-forbidden" });
  });

  it("bridges concrete open-weight families only through an explicit matching pattern", () => {
    expect(assessAdapterModelPolicy({
      modelFamily: "zhipu",
      modelId: "GLM-5",
      allowedFamilies: ["open-weight"],
      allowedModelPatterns: ["glm-*"],
      requiresExplicitModel: true,
    })).toEqual({ allowed: true, reason: "allowed" });
    expect(assessAdapterModelPolicy({
      modelFamily: "google",
      modelId: "gemini-3.1-pro",
      allowedFamilies: ["open-weight"],
      requiresExplicitModel: true,
    })).toEqual({ allowed: false, reason: "family-forbidden" });
  });

  it("derives the OpenCode install root from its resolved executable", async () => {
    const fixture = await createCursorKiroCompatibilityFixture();
    fixtures.push(fixture);
    const document = parse(await readFile(fixture.compatibilityPath, "utf8")) as {
      activation_policy: Record<string, unknown>;
      adapters: Record<string, { implementation: { executable: string; provider_install_root?: string } }>;
    };
    const executable = document.adapters["opencode-acp"]!.implementation.executable;
    document.adapters["opencode-acp"]!.implementation.provider_install_root = "${EXECUTABLE_ROOT}";
    document.activation_policy.executable_resolution_version = 2;
    await writeFile(fixture.compatibilityPath, stringify(document));

    const constraints = await loadAdapterModelConstraints({
      compatibilityPath: fixture.compatibilityPath,
      schemaPath: fixture.schemaPath,
      adapterId: "opencode-acp",
    });

    expect(constraints.providerInstallRoot).toBe(dirname(executable));
  });
});
