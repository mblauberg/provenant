import { createHash } from "node:crypto";
import { mkdir, realpath } from "node:fs/promises";
import { isAbsolute, join } from "node:path";

import type { JsonValue, ProviderActionId } from "@local/agent-fabric-protocol";

import type {
  HerdrActionEvidence,
  HerdrActionRecord,
} from "../integrations/herdr-fabric-ports.js";
import type {
  HerdrDaemonIntegrationConfiguration,
  HerdrDaemonRuntime,
} from "../integrations/herdr-daemon-integration.js";

export type HerdrDaemonProcessConfiguration =
  | Readonly<{ enabled: false }>
  | Readonly<{
      enabled: true;
      executable: string;
      expectedProtocol: number;
      consoleExecutable: string;
      consoleExecutableDigest: string;
      presencePollIntervalMs?: number;
      observerExecutable?: string;
      observerExecutableDigest?: string;
      observerSocketPath?: string;
      observerCapabilityFile?: string;
      observerCursorDirectory?: string;
    }>;

export function parseHerdrDaemonProcessConfiguration(serialized: string | undefined): HerdrDaemonProcessConfiguration {
  if (serialized === undefined) return { enabled: false };
  const value: unknown = JSON.parse(serialized);
  if (!isRecord(value) || typeof value.enabled !== "boolean") {
    throw new TypeError("daemon Herdr configuration must be a closed object");
  }
  if (!value.enabled) {
    if (!exactKeys(value, ["enabled"])) throw new TypeError("disabled daemon Herdr configuration is not closed");
    return { enabled: false };
  }
  const observerFields = [
    "observerCapabilityFile",
    "observerCursorDirectory",
    "observerExecutable",
    "observerExecutableDigest",
    "observerSocketPath",
  ] as const;
  const observerCount = observerFields.filter((field) => value[field] !== undefined).length;
  if (observerCount !== 0 && observerCount !== observerFields.length) {
    throw new TypeError("daemon Herdr observer configuration must be complete or absent");
  }
  const expected = [
    "consoleExecutable",
    "consoleExecutableDigest",
    "enabled",
    "executable",
    "expectedProtocol",
    ...(value.presencePollIntervalMs === undefined ? [] : ["presencePollIntervalMs"]),
    ...(observerCount === 0 ? [] : observerFields),
  ];
  if (!exactKeys(value, expected)) throw new TypeError("enabled daemon Herdr configuration is not closed");
  for (const field of ["executable", "consoleExecutable"] as const) {
    if (typeof value[field] !== "string" || !isAbsolute(value[field]) || value[field].includes("\0")) {
      throw new TypeError(`daemon Herdr ${field} must be an absolute path`);
    }
  }
  for (const field of ["consoleExecutableDigest"] as const) {
    if (typeof value[field] !== "string" || !/^sha256:[0-9a-f]{64}$/u.test(value[field])) {
      throw new TypeError(`daemon Herdr ${field} is invalid`);
    }
  }
  if (!Number.isSafeInteger(value.expectedProtocol) || Number(value.expectedProtocol) < 1) {
    throw new TypeError("daemon Herdr expectedProtocol is invalid");
  }
  if (
    value.presencePollIntervalMs !== undefined &&
    (typeof value.presencePollIntervalMs !== "number" || !Number.isSafeInteger(value.presencePollIntervalMs) ||
      value.presencePollIntervalMs < 250 || value.presencePollIntervalMs > 60_000)
  ) throw new TypeError("daemon Herdr presence poll interval is outside 250..60000ms");
  let observer: Record<string, string> = {};
  if (observerCount !== 0) {
    const digest = value.observerExecutableDigest;
    if (typeof digest !== "string" || !/^sha256:[0-9a-f]{64}$/u.test(digest)) {
      throw new TypeError("daemon Herdr observer executable digest is invalid");
    }
    observer = { observerExecutableDigest: digest };
    for (const field of observerFields.filter((entry) => entry !== "observerExecutableDigest")) {
      const entry = value[field];
      if (typeof entry !== "string" || !isAbsolute(entry) || entry.includes("\0")) {
        throw new TypeError(`daemon Herdr ${field} must be an absolute path`);
      }
      observer[field] = entry;
    }
  }
  return {
    enabled: true,
    executable: value.executable,
    expectedProtocol: Number(value.expectedProtocol),
    consoleExecutable: value.consoleExecutable,
    consoleExecutableDigest: value.consoleExecutableDigest,
    ...(value.presencePollIntervalMs === undefined ? {} : { presencePollIntervalMs: Number(value.presencePollIntervalMs) }),
    ...observer,
  } as HerdrDaemonProcessConfiguration;
}

export function composeHerdrDaemonIntegration(
  configuration: HerdrDaemonProcessConfiguration,
  stateDirectory: string,
): HerdrDaemonIntegrationConfiguration {
  if (!configuration.enabled) return { mode: "disabled" };
  return {
    mode: "enabled",
    createIntegration: async (input) => {
      const sessionDirectory = join(
        stateDirectory,
        "herdr",
        createHash("sha256").update(input.projectSessionId).digest("hex"),
      );
      await mkdir(sessionDirectory, { recursive: true, mode: 0o700 });
      if (await realpath(sessionDirectory) !== sessionDirectory) {
        throw new TypeError("daemon Herdr session state directory is non-canonical");
      }
      const module = await loadHerdrModule();
      const integration = await module.createProductionHerdrIntegration({
        executable: configuration.executable,
        expectedProtocol: configuration.expectedProtocol,
        stateDirectory: sessionDirectory,
        projectId: input.projectId,
        projectSessionId: input.projectSessionId,
        canonicalProjectRoot: input.canonicalProjectRoot,
        consoleExecutable: configuration.consoleExecutable,
        consoleExecutableDigest: configuration.consoleExecutableDigest,
        ...(configuration.observerExecutable === undefined ? {} : {
          observerExecutable: configuration.observerExecutable,
          observerExecutableDigest: configuration.observerExecutableDigest,
          observerSocketPath: configuration.observerSocketPath,
          observerCapabilityFile: configuration.observerCapabilityFile,
          observerCursorDirectory: configuration.observerCursorDirectory,
        }),
        fabricJournal: input.fabricJournal,
        fabricDirectSteer: input.fabricDirectSteer,
      });
      return {
        restoreControlBinding: async (intent) => {
          if (!isRecord(intent)) throw new TypeError("daemon Herdr control binding is malformed");
          if (intent.kind === "console.ensure-pane") {
            await integration.boundary.restoreConsolePresenceRegistration(intent);
            return;
          }
          if (intent.kind !== "agent.ensure-pane" || !isRecord(intent.identity)) {
            throw new TypeError("daemon Herdr control binding is outside the restorable family");
          }
          integration.boundary.restorePresenceRegistration(intent);
          await integration.adapter.reconcilePresence(intent.identity as JsonValue);
        },
        execute: async (actionId, intent) => await integration.adapter.execute(actionId, intent),
        lookupAction: async (actionId) => await integration.boundary.lookupAction(actionId),
        reconcilePresence: async (intent) => {
          if (!isRecord(intent) || intent.kind !== "agent.ensure-pane" || !isRecord(intent.identity)) {
            throw new TypeError("daemon Herdr presence registration is malformed");
          }
          integration.boundary.restorePresenceRegistration(intent);
          return await integration.adapter.reconcilePresence(intent.identity as JsonValue);
        },
      } satisfies HerdrDaemonRuntime;
    },
  };
}

export function herdrPresencePollInterval(configuration: HerdrDaemonProcessConfiguration): number | null {
  return configuration.enabled ? configuration.presencePollIntervalMs ?? 1_000 : null;
}

type LoadedHerdrIntegration = {
  adapter: {
    execute(actionId: ProviderActionId, intent: JsonValue): Promise<HerdrActionRecord>;
    reconcilePresence(identity: JsonValue): Promise<JsonValue>;
  };
  boundary: {
    lookupAction(actionId: ProviderActionId): Promise<HerdrActionEvidence>;
    restorePresenceRegistration(intent: JsonValue): void;
    restoreConsolePresenceRegistration(intent: JsonValue): Promise<void>;
  };
};

type HerdrModule = {
  createProductionHerdrIntegration(options: unknown): Promise<LoadedHerdrIntegration>;
};

async function loadHerdrModule(): Promise<HerdrModule> {
  const sourceMode = import.meta.url.endsWith(".ts");
  const moduleUrl = new URL(
    sourceMode ? "../../../agent-fabric-herdr/src/index.ts" : "../../../agent-fabric-herdr/dist/index.js",
    import.meta.url,
  );
  const value: unknown = await import(moduleUrl.href);
  if (!isRecord(value) || typeof value.createProductionHerdrIntegration !== "function") {
    throw new TypeError("optional Herdr production package is unavailable");
  }
  return value as HerdrModule;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  return actual.length === sortedExpected.length && actual.every((field, index) => field === sortedExpected[index]);
}
