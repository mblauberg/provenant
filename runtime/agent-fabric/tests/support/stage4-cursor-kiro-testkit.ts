import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { stringify } from "yaml";

import * as publicApi from "../../src/index.ts";
import { runAdapterConformance } from "../../src/index.ts";

import { commitFixtureRepository, writeWrapperPackageScaffold } from "./fixture-repository.ts";

export type Stage4AdapterId = "cursor-agent" | "kiro-acp" | "opencode-acp";
export type PublicFunction = (...arguments_: unknown[]) => unknown;

const fixtureAdapter = fileURLToPath(new URL("./stage4-cursor-kiro-fake-adapter.ts", import.meta.url));

export function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

export function requireStage4PublicFunction(name: string): PublicFunction {
  const value: unknown = Reflect.get(publicApi, name);
  if (typeof value !== "function") {
    throw new Error(`public agent-fabric API ${name} is not implemented`);
  }
  return (...arguments_: unknown[]) => Reflect.apply(value, undefined, arguments_);
}

function compatibilityEntry(input: {
  adapterId: Stage4AdapterId;
  wrapperPath: string;
}): Record<string, unknown> {
  const cursor = input.adapterId === "cursor-agent";
  const openCode = input.adapterId === "opencode-acp";
  return {
    enabled: true,
    delivery_stage: 4,
    implementation: {
      kind: "fixture-process",
      executable: fixtureAdapter,
      provider_identity: cursor ? "cursor-partial-signed-helpers" : openCode ? "owner-controlled-install-root" : "apple-designated",
      ...(input.adapterId === "cursor-agent" ? { cursor_install_root: dirname(fixtureAdapter) } : {}),
      ...(openCode ? { provider_install_root: dirname(fixtureAdapter) } : {}),
      wrapper_entrypoint: input.wrapperPath,
    },
    contract: {
      protocol: cursor ? "cursor-fixture-jsonl" : "fixture-acp",
    },
    runtime_range: { platforms: [process.platform] },
    model_family_constraints: cursor
      ? {
          allowed: ["cursor-composer", "xai"],
          allowed_model_patterns: ["composer-*", "cursor-grok-*"],
          requires_explicit_model: true,
        }
      : openCode
        ? {
            allowed: ["generic-open"],
            allowed_model_patterns: ["opencode/*"],
            requires_explicit_model: true,
            route_role: "optional-free-account-worker",
          }
        : {
          allowed: ["open-weight"],
          allowed_model_patterns: ["deepseek-*", "glm-*", "minimax-*", "qwen*"],
          requires_explicit_model: true,
          route_role: "open-model-worker",
        },
    official_source_url: "https://example.invalid/stage4-fixture",
  };
}

export async function createCursorKiroCompatibilityFixture(): Promise<{
  directory: string;
  compatibilityPath: string;
  schemaPath: string;
}> {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-stage4-cursor-kiro-"));
  const wrapperPath = join(directory, "fixture-wrapper.js");
  const wrapper = "export const fixtureWrapper = true;\n";
  await writeFile(wrapperPath, wrapper, { mode: 0o600 });
  await writeWrapperPackageScaffold(directory);
  await commitFixtureRepository(directory);
  const adapters = Object.fromEntries(
    (["cursor-agent", "kiro-acp", "opencode-acp"] as const).map((adapterId) => [
      adapterId,
      compatibilityEntry({
        adapterId,
        wrapperPath,
      }),
    ]),
  );
  const compatibilityPath = join(directory, "adapter-compatibility.yaml");
  await writeFile(
    compatibilityPath,
    stringify({
      schema_version: 1,
      activation_policy: { real_adapters_require_separate_gate: true, default_enabled: false },
      adapters,
    }),
  );
  return {
    directory,
    compatibilityPath,
    schemaPath: repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
  };
}

export async function runStage4Fixture(input: {
  adapterId: Stage4AdapterId;
  model: string;
  modelFamily: string;
  journalPath: string;
}): Promise<Awaited<ReturnType<typeof runAdapterConformance>>> {
  return await runAdapterConformance({
    command: [process.execPath, "--import", "tsx", fixtureAdapter, input.adapterId],
    environment: { STAGE4_FAKE_ADAPTER_JOURNAL: input.journalPath },
    action: {
      actionId: `${input.adapterId}:conformance:1`,
      operation: "send_turn",
      payload: { model: input.model, modelFamily: input.modelFamily, prompt: "fixture-only" },
    },
  });
}
