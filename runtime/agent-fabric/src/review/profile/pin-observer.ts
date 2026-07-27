/**
 * Pin observation through the repository's existing capability producers.
 *
 * `skills/orchestrate/scripts/claude_capabilities.py` and
 * `skills/orchestrate/scripts/codex_capabilities.py` are the only discovery
 * mechanisms used here. This module runs them, applies the same snapshot trust
 * rules `scripts/model_route.py` applies before believing one, and caches the
 * result so callers can choose either a cache-only read or the existing
 * cache-with-live-refresh behaviour.
 *
 * Failure is never drift. A producer that exits non-zero, times out, is absent
 * or returns an untrusted snapshot yields `unobservable`, which the comparison
 * reports as `unknown`.
 */

import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import type { ModelFamily } from "./index.js";
import type { PinRouteObservation, PinRouteObserver } from "./pin-drift.js";

const execFileAsync = promisify(execFile);

/** One canary per family per window, however often doctor runs. */
export const PIN_OBSERVATION_FRESHNESS_MS = 6 * 60 * 60 * 1000;
export const PIN_OBSERVATION_TIMEOUT_MS = 45_000;
export const PIN_OBSERVATION_CACHE_FILE = "review-profile-pin-observations.json";

/** Efforts `claude_capabilities.py` accepts; anything else would make the producer reject the run. */
const CANARY_EFFORTS = new Set(["low", "medium", "high", "xhigh", "max"]);
const DEFAULT_CANARY_EFFORT = "high";

const PRODUCERS: Partial<Record<ModelFamily, { script: string; source: string }>> = {
  anthropic: { script: "claude_capabilities.py", source: "claude subscription canary" },
  openai: { script: "codex_capabilities.py", source: "codex debug models" },
};

type CacheEntry = { observedModel: string | null; observedAtMs: number };
type Cache = { schemaVersion: 1; entries: Record<string, CacheEntry> };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * The trust rules `scripts/model_route.py` applies to a capability snapshot,
 * minus its 300s routing window: freshness here is the caller's cache window,
 * because a pin report is not a dispatch decision.
 */
function trustedModels(
  raw: unknown,
  expectedSource: string,
  providerFamily: ModelFamily,
): Map<string, string> | null {
  if (!isRecord(raw) || raw.schema_version !== 1 || raw.source !== expectedSource) return null;
  if (typeof raw.observed_at !== "string" || !/[Zz]|[+-]\d{2}:\d{2}$/u.test(raw.observed_at)) return null;
  if (!Number.isFinite(Date.parse(raw.observed_at))) return null;
  if (providerFamily === "anthropic") {
    const provenance = raw.provenance;
    if (!isRecord(provenance) || provenance.kind !== "subscription_runtime_canary"
      || provenance.auth_method !== "claude.ai"
      || typeof provenance.subscription_type !== "string" || provenance.subscription_type.length === 0) return null;
  }
  if (!isRecord(raw.models)) return null;
  const models = new Map<string, string>();
  for (const [key, item] of Object.entries(raw.models)) {
    if (!isRecord(item) || typeof item.resolved_model !== "string" || item.resolved_model.length === 0) return null;
    const normalised = key.toLowerCase();
    if (models.has(normalised)) return null;
    models.set(normalised, item.resolved_model);
  }
  return models.size === 0 ? null : models;
}

async function readCache(path: string): Promise<Cache> {
  try {
    const parsed: unknown = JSON.parse(await readFile(path, "utf8"));
    if (isRecord(parsed) && parsed.schemaVersion === 1 && isRecord(parsed.entries)) {
      const entries: Record<string, CacheEntry> = {};
      for (const [key, value] of Object.entries(parsed.entries)) {
        if (!isRecord(value) || typeof value.observedAtMs !== "number") continue;
        if (value.observedModel !== null && typeof value.observedModel !== "string") continue;
        entries[key] = { observedModel: value.observedModel, observedAtMs: value.observedAtMs };
      }
      return { schemaVersion: 1, entries };
    }
  } catch {}
  return { schemaVersion: 1, entries: {} };
}

async function writeCache(path: string, cache: Cache): Promise<void> {
  const staging = `${path}.${String(process.pid)}.tmp`;
  try {
    await writeFile(staging, `${JSON.stringify(cache, null, 2)}\n`, { mode: 0o600 });
    await rename(staging, path);
  } catch {
    await rm(staging, { force: true });
  }
}

async function runProducer(
  providerFamily: ModelFamily,
  agentsHome: string,
  candidate: string,
  requestedEffort: string | null,
  timeoutMs: number,
): Promise<Map<string, string> | { failure: string }> {
  const producer = PRODUCERS[providerFamily];
  if (producer === undefined) return { failure: `no capability producer for ${providerFamily}` };
  const script = join(agentsHome, "skills", "orchestrate", "scripts", producer.script);
  const options = { encoding: "utf8" as const, timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024 };
  let raw: unknown;
  if (providerFamily === "anthropic") {
    const effort = requestedEffort !== null && CANARY_EFFORTS.has(requestedEffort)
      ? requestedEffort
      : DEFAULT_CANARY_EFFORT;
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-pin-canary-"));
    const out = join(directory, "capabilities.json");
    try {
      await execFileAsync(script, ["--out", out, "--alias", candidate, "--effort", effort], options);
      raw = JSON.parse(await readFile(out, "utf8"));
    } catch (error: unknown) {
      return { failure: errorDetail(error) };
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  } else {
    try {
      const { stdout } = await execFileAsync(script, [], options);
      raw = JSON.parse(stdout);
    } catch (error: unknown) {
      return { failure: errorDetail(error) };
    }
  }
  const models = trustedModels(raw, producer.source, providerFamily);
  return models ?? { failure: `${producer.script} returned an untrusted capability snapshot` };
}

/**
 * Observe what a route alias currently resolves to, reusing the repository's
 * capability producers and a cached freshness window.
 */
export function createCapabilityPinObserver(options: {
  readonly agentsHome: string;
  /** Absent means never cache: the refresh command always wants a live probe. */
  readonly cacheDirectory?: string;
  /** Return unknown on a fresh-cache miss without invoking a capability producer. */
  readonly cacheOnly?: boolean;
  /** Bypass a fresh cache for reading, invoke the producer and refresh the cache. */
  readonly forceLive?: boolean;
  readonly freshnessMs?: number;
  readonly timeoutMs?: number;
  readonly now?: () => number;
}): PinRouteObserver {
  const freshnessMs = options.freshnessMs ?? PIN_OBSERVATION_FRESHNESS_MS;
  const timeoutMs = options.timeoutMs ?? PIN_OBSERVATION_TIMEOUT_MS;
  const now = options.now ?? Date.now;
  return async ({ providerFamily, routeAlias, routeCandidates, requestedEffort }): Promise<PinRouteObservation> => {
    const candidate = routeCandidates[0];
    if (candidate === undefined) {
      return { status: "unobservable", detail: `alias ${routeAlias} names no route candidate` };
    }
    const key = `${providerFamily}/${routeAlias}/${candidate}`;
    const cachePath = options.cacheDirectory === undefined
      ? undefined
      : join(options.cacheDirectory, PIN_OBSERVATION_CACHE_FILE);
    if (cachePath !== undefined && options.forceLive !== true) {
      const cached = (await readCache(cachePath)).entries[key];
      const ageMs = cached === undefined ? undefined : now() - cached.observedAtMs;
      if (cached !== undefined && ageMs !== undefined && ageMs >= 0 && ageMs < freshnessMs) {
        const age = Math.floor(ageMs / 60_000);
        return cached.observedModel === null
          ? { status: "retired", detail: `cached ${String(age)}m ago: provider no longer offers ${candidate}` }
          : { status: "observed", model: cached.observedModel, detail: `cached ${String(age)}m ago` };
      }
    }
    if (options.cacheOnly === true) {
      return {
        status: "unobservable",
        detail: "no provider capability result cached within the last six hours; live provider capability probe was not run",
      };
    }
    const result = await runProducer(providerFamily, options.agentsHome, candidate, requestedEffort, timeoutMs);
    if (!(result instanceof Map)) {
      return { status: "unobservable", detail: `${providerFamily} capability probe did not complete: ${result.failure}` };
    }
    const observedModel = result.get(candidate.toLowerCase()) ?? null;
    if (cachePath !== undefined && options.cacheDirectory !== undefined) {
      await mkdir(options.cacheDirectory, { recursive: true, mode: 0o700 });
      const cache = await readCache(cachePath);
      cache.entries[key] = { observedModel, observedAtMs: now() };
      await writeCache(cachePath, cache);
    }
    return observedModel === null
      ? { status: "retired", detail: `${providerFamily} capability snapshot no longer offers ${candidate}` }
      : { status: "observed", model: observedModel, detail: `observed live via ${PRODUCERS[providerFamily]!.script}` };
  };
}
