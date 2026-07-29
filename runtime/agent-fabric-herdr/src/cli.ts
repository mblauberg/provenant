import { constants } from "node:fs";
import { open, realpath } from "node:fs/promises";
import { isAbsolute } from "node:path";

import type {
  FabricActionJournalPort,
  FabricDirectSteerPort,
} from "./contracts.js";
import {
  createProductionHerdrIntegration,
  type ProductionHerdrIntegrationOptions,
} from "./production.js";

export type HerdrCliIo = Readonly<{
  stdin: NodeJS.ReadableStream;
  stdout: NodeJS.WritableStream;
  stderr: NodeJS.WritableStream;
}>;

const USAGE = "usage: agent-fabric-herdr doctor --config PATH\n";

type HerdrCliConfig = Omit<ProductionHerdrIntegrationOptions, "fabricJournal" | "fabricDirectSteer" | "clock"> & {
  schemaVersion: 1;
};

export async function runHerdrCli(arguments_: readonly string[], io: HerdrCliIo): Promise<number> {
  if (arguments_.length === 0 || arguments_[0] === "--help" || arguments_[0] === "-h") {
    io.stdout.write(USAGE);
    return 0;
  }
  const command = arguments_[0];
  if (command !== "doctor") {
    io.stderr.write(USAGE);
    return 2;
  }
  let parsed: ParsedArguments;
  try {
    parsed = parseArguments(arguments_.slice(1));
  } catch (error: unknown) {
    io.stderr.write(`${error instanceof Error ? error.message : "invalid arguments"}\n`);
    return 2;
  }
  try {
    const config = await readConfig(parsed.config);
    const integration = await createProductionHerdrIntegration({
      ...config,
      fabricJournal: inactiveJournal(),
      fabricDirectSteer: inactiveDirectSteer(),
    });
    const conformance = await integration.boundary.probe();
    io.stdout.write(`${JSON.stringify({
      status: "available",
      version: conformance.version,
      protocol: conformance.protocol,
      projectId: config.projectId,
      projectSessionId: config.projectSessionId,
      authority: "fabric",
      paneTruth: false,
    })}\n`);
    return 0;
  } catch {
    io.stderr.write("Herdr configuration or bounded operation failed safely\n");
    return 1;
  }
}

type ParsedArguments = { config: string };

function parseArguments(arguments_: readonly string[]): ParsedArguments {
  const config = arguments_.length === 2 && arguments_[0] === "--config"
    ? arguments_[1]
    : undefined;
  if (config === undefined || !isAbsolute(config) || config.includes("\0")) {
    throw new TypeError("--config must name an absolute file");
  }
  return { config };
}

async function readConfig(path: string): Promise<HerdrCliConfig> {
  const canonical = await realpath(path).catch(() => null);
  if (canonical === null || canonical !== path) throw new TypeError("Herdr config is missing or non-canonical");
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const info = await handle.stat();
    if (!info.isFile() || info.size < 1 || info.size > 64 * 1024) throw new TypeError("Herdr config is not a bounded regular file");
    const value: unknown = JSON.parse((await handle.readFile()).toString("utf8"));
    if (!isRecord(value)) throw new TypeError("Herdr config must be an object");
    const observerFields = [
      "observerExecutable",
      "observerExecutableDigest",
      "observerSocketPath",
      "observerCapabilityFile",
      "observerCursorDirectory",
    ] as const;
    const optional = observerFields.every((field) => value[field] === undefined) ? [] : observerFields;
    const keys = [
      "canonicalProjectRoot", "consoleExecutable", "consoleExecutableDigest", "executable",
      "expectedProtocol", "projectId", "projectSessionId",
      "schemaVersion", "stateDirectory", ...optional,
    ];
    if (!exactKeys(value, keys) || value.schemaVersion !== 1 ||
        typeof value.executable !== "string" || !Number.isSafeInteger(value.expectedProtocol) ||
        typeof value.stateDirectory !== "string" || typeof value.projectId !== "string" ||
        typeof value.projectSessionId !== "string" || typeof value.canonicalProjectRoot !== "string" ||
        typeof value.consoleExecutable !== "string" || typeof value.consoleExecutableDigest !== "string" ||
        (value.observerExecutable !== undefined && typeof value.observerExecutable !== "string") ||
        (value.observerExecutableDigest !== undefined && typeof value.observerExecutableDigest !== "string") ||
        (value.observerSocketPath !== undefined && typeof value.observerSocketPath !== "string") ||
        (value.observerCapabilityFile !== undefined && typeof value.observerCapabilityFile !== "string") ||
        (value.observerCursorDirectory !== undefined && typeof value.observerCursorDirectory !== "string")) {
      throw new TypeError("Herdr config has an invalid closed shape");
    }
    return value as HerdrCliConfig;
  } finally {
    await handle.close();
  }
}

function inactiveJournal(): FabricActionJournalPort {
  return {
    readAction: async () => null,
    markDispatched: async () => { throw new TypeError("diagnostic CLI has no Fabric journal mutation"); },
    completeAction: async () => { throw new TypeError("diagnostic CLI has no Fabric journal mutation"); },
    markAmbiguous: async () => { throw new TypeError("diagnostic CLI has no Fabric journal mutation"); },
  };
}

function inactiveDirectSteer(): FabricDirectSteerPort {
  return {
    validateSteerReference: async () => ({ status: "rejected", code: "unknown-reference", reason: "diagnostic CLI has no Fabric steering authority" }),
    prepareDirectSteerAction: async () => { throw new TypeError("diagnostic CLI cannot prepare a Fabric action"); },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const sorted = [...expected].sort();
  return actual.length === sorted.length && actual.every((key, index) => key === sorted[index]);
}
