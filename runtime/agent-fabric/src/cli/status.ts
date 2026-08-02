import Database from "better-sqlite3";
import { existsSync } from "node:fs";
import { lstat, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import {
  MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
  assertRequiredResultShapeFeatures,
} from "@local/agent-fabric-protocol";

import { verifyAdapterCompatibility, type AdapterExecutableFailure } from "../adapters/compatibility.js";
import { isPrimaryAdapter } from "../adapters/primary-adapters.js";
import { verifyProviderConformance } from "../adapters/provider-conformance.js";
import {
  providerIdentityAssuranceForPolicy,
  verifyProviderExecutableIdentity,
} from "../adapters/provider-identity.js";
import { ADAPTER_INTERFACE_PROBE_INCOMPLETE, probeProviderInterface } from "../adapters/provider-interface.js";
import { loadAdapterModelConstraints } from "../adapters/model-selection.js";
import { loadFabricConfig } from "../config/index.js";
import { withPrivateDatabaseClone } from "../core/migrations.js";
import type { ReviewProfileCatalogue } from "../review/profile/index.js";
import {
  evaluateReviewProfilePinDrift,
  readPinObservations,
  reviewProfilePinOutcome,
  type PinRouteObserver,
  type ReviewProfilePinReport,
} from "../review/profile/pin-drift.js";
import { createCapabilityPinObserver } from "../review/profile/pin-observer.js";
import {
  verifyDeployedReviewProfileCatalogue,
  type DeployedReviewProfileCatalogueReport,
} from "../review/profile/deployed-catalogue.js";
import { assertDatabaseIntegrity } from "../persistence/invariants.js";
import { BootstrapElection, FLOCK_ELECTION_LOCK_PORT } from "../daemon/bootstrap-election.js";
import { connectFabricDaemon } from "../daemon/client.js";
import { privateDiscoveryPaths, readPrivateDiscovery } from "../daemon/private-discovery.js";
import { preflightProtocolBuild } from "../daemon/protocol-build-preflight.js";
import {
  mcpBootstrapRenewalCommand,
  mcpRosterRenewalCommand,
  openRosterReadPort,
  seatExpiryWarningDue,
} from "./mcp-roster-renewal.js";
import type { FabricPaths } from "./paths.js";
import { fabricCliCommand, resolveFabricRoots } from "../domain/fabric-roots.js";
import { hasPairedInstanceRoot } from "./instance-root-pairing.js";
import { MCP_SEATS, resolveSeatPaths, type SeatMetadata } from "./seat-store.js";
import { projectConfigPathAtExactRoot, resolveProjectBoundary } from "./project-boundary.js";
import { trustedWorkspaceRoots } from "./workspace-trust.js";
import {
  daemonState,
  errorCode,
  errorDetail,
  mergeOptionalAdapterFailures,
  optionalAdapterFailureDetail,
  verifyConfiguredTsxLoaders,
  type StatusDependencies,
} from "./status-support.js";

type Check = {
  id: string;
  status: "pass" | "idle" | "fail";
  code: string;
  detail: string;
  /** The condition this check asserts, named so a state can say why it holds. */
  precondition: string;
};
type ProviderIdentityResult = Awaited<ReturnType<typeof verifyProviderExecutableIdentity>>;
type ProviderInterfaceResult = Awaited<ReturnType<typeof probeProviderInterface>>;
type ProviderObservation = { adapterId: string; requiredIdentity: string; identity?: ProviderIdentityResult; providerInterface?: ProviderInterfaceResult; identityError?: unknown; interfaceError?: unknown };
type ProviderIdentityState = "clean" | "drifted" | "unknown";
type ProviderIdentityObservation = { adapterId: string; state: ProviderIdentityState; detail: string };
type DateStaleness = { field: "catalog_date" | "observed_on"; date: string | null; ageDays: number | null; stale: boolean | null };
type DoctorDaemonConnection = Pick<
  Awaited<ReturnType<typeof connectFabricDaemon>>,
  "initializeResult" | "probeBootstrapContract" | "close"
>;
type DoctorDependencies = {
  verifyProvider?: typeof verifyProviderConformance;
  verifyProviderIdentity?: typeof verifyProviderExecutableIdentity;
  probeProviderInterface?: typeof probeProviderInterface;
  observeReviewProfilePin?: PinRouteObserver;
  providerProbeTimeoutMs?: number;
  preflightProtocolBuild?: typeof preflightProtocolBuild;
  now?: () => number;
  connectDaemon?: (input: {
    socketPath: string;
    capability: string;
  }) => Promise<DoctorDaemonConnection>;
  inspectDaemonSocket?: (path: string) => Promise<{ isSocket(): boolean; uid: number }>;
};

type DoctorDaemonState =
  | { status: "live"; code: "DAEMON_LIVE"; detail: string; pid: number; socketPath: string }
  | { status: "idle"; code: "DAEMON_ON_DEMAND_IDLE"; detail: string; pid: null; socketPath: null }
  | { status: "failed"; code: string; detail: string; pid: number | null; socketPath: string | null };

/**
 * Causal lifecycle vocabulary. `current` and `idle` are the two healthy modes;
 * a failing precondition is `recovering` when the next ordinary bootstrap
 * reconciles it under existing authority, and `blocked` otherwise. Unknown
 * codes fail closed to `blocked`: this lifecycle never claims that a condition
 * it does not recognise will repair itself.
 */
export type DoctorLifecycleState = "current" | "idle" | "recovering" | "blocked";

const PRECONDITIONS: Readonly<Record<string, string>> = {
  "protocol-build": "the local protocol dist is present and current for its build inputs",
  configuration: "the trusted Fabric configuration loads and names its adapters",
  "wrapper-loader": "every configured adapter wrapper loader is installed",
  "adapter-compatibility": "adapter activation metadata validates against the compatibility schema",
  "provider-conformance": "each configured provider executable answers its conformance probe",
  "provider-identity": "each primary provider passes runtime identity and interface conformance checks",
  "pin-staleness": "model catalogue and review-profile observation dates are within the advisory threshold",
  "state-directory": "the state directory is a private non-symlink directory",
  "runtime-directory": "the runtime directory is a private non-symlink directory",
  "database-integrity": "a byte-stable copy of the Fabric database is current-schema and passes its invariants",
  "daemon-socket": "daemon discovery, election, process, socket and bootstrap contract agree",
};

/**
 * The only failures an ordinary bootstrap actually converges, each traced to
 * the code path that converges it:
 *
 * - `BOOTSTRAP_IN_PROGRESS` and `DAEMON_SHUTDOWN_IN_PROGRESS`: another owner
 *   holds the transition and is completing it; no work is required here.
 * - `DAEMON_PROCESS_UNAVAILABLE`: active discovery whose PID is dead, the one
 *   shape `reconcileUnreachablePrivateDaemon` marks terminal and re-elects.
 * - `DAEMON_PROCESS_CRASHED` and `DAEMON_PROCESS_UNCLEAN_STOP`: terminal
 *   discovery carrying evidence that matches its ready receipt, which the
 *   ordinary spawn path replaces.
 * - `DATABASE_INSPECTION_UNSTABLE`: the bounded read-only inspection did not
 *   observe a stable source set; a later inspection may converge.
 *
 * Everything else is `blocked` and reported as needing a decision rather than
 * a retry. That deliberately excludes absent discovery, a stale socket and an
 * unreachable socket under a live PID: reconciliation returns false for all
 * three, so bootstrap raises `BOOTSTRAP_RECONCILIATION_REQUIRED` or
 * `BOOTSTRAP_READY_UNREACHABLE` and a zero-touch caller would retry forever.
 * Unrecognised codes fail closed to `blocked` for the same reason.
 */
const RECOVERABLE_CODES: ReadonlySet<string> = new Set([
  "BOOTSTRAP_IN_PROGRESS",
  "DAEMON_SHUTDOWN_IN_PROGRESS",
  "DAEMON_PROCESS_UNAVAILABLE",
  "DAEMON_PROCESS_CRASHED",
  "DAEMON_PROCESS_UNCLEAN_STOP",
  "DATABASE_INSPECTION_UNSTABLE",
]);

const PROVIDER_PROBE_TIMEOUT_MS = 16_000;
const STALENESS_THRESHOLD_DAYS = 30;
const PROFILE_PIN_REPAIR_COMMAND = "npm run profile:pin";
const DAY_MS = 24 * 60 * 60 * 1000;
const CACHED_REVIEW_PROFILE_PIN_PRECONDITION =
  "each certifying-profile model pin in the automated comparison set matches a provider capability result cached within the last six hours";
const LIVE_REVIEW_PROFILE_PIN_PRECONDITION =
  "each certifying-profile model pin in the automated comparison set matches alias resolution checked by a live provider capability probe";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  const value = index === -1 ? undefined : arguments_[index + 1];
  if (index !== -1 && (value === undefined || value.startsWith("--"))) throw new Error(`${name} requires a value`);
  return value;
}

function checkCode(id: string, outcome: "OK" | "FAILED"): string {
  return `${id.replaceAll("-", "_").toUpperCase()}_${outcome}`;
}

function generationIdentityMatches(
  owner: { actionId: string; electionGeneration: number; daemonInstanceGeneration: number; socketPath: string },
  ready: { actionId: string; electionGeneration: number; daemonInstanceGeneration: number; socketPath: string },
): boolean {
  return ready.actionId === owner.actionId
    && ready.electionGeneration === owner.electionGeneration
    && ready.daemonInstanceGeneration === owner.daemonInstanceGeneration
    && ready.socketPath === owner.socketPath;
}

/**
 * Diagnostics compose configuration exactly as daemon startup does.
 *
 * `config` is the product's shipped layer and `localConfig` is the instance's
 * narrowing layer, present only when the instance root was explicitly selected,
 * the instance ships a layer and the operator has not pinned a single file
 * with `--trusted-config` (ADR 0019). Reading one layer instead would let
 * `status` and `doctor` report a widened policy the daemon refuses to start on,
 * or fail on a split instance that is perfectly valid because it holds no
 * product-owned file at all.
 */
export function resolveStatusPaths(arguments_: string[]): { productRoot: string; instanceRoot: string; config: string; localConfig: string | undefined; compatibility: string; compatibilitySchema: string; modelRouting: string; reviewProfile: string } {
  const agentsHomeFlag = option(arguments_, "--agents-home");
  const productRootFlag = option(arguments_, "--product-root");
  const instanceRootFlag = option(arguments_, "--instance-root");
  const rootOptions = {
    agentsHomeFlag,
    productRootFlag,
    instanceRootFlag,
  };
  const { productRoot, instanceRoot } = resolveFabricRoots(rootOptions);
  const pinnedConfig = option(arguments_, "--trusted-config");
  const config = resolve(pinnedConfig ?? join(productRoot, "config", "agent-fabric.yaml"));
  const instanceConfig = resolve(join(instanceRoot, "config", "agent-fabric.yaml"));
  return {
    productRoot,
    instanceRoot,
    config,
    localConfig:
      pinnedConfig === undefined
        && hasPairedInstanceRoot({ productRoot, instanceRoot })
        && instanceConfig !== config
        && existsSync(instanceConfig)
        ? instanceConfig
        : undefined,
    compatibility: resolve(option(arguments_, "--compatibility") ?? join(productRoot, "config", "adapter-compatibility.yaml")),
    compatibilitySchema: resolve(option(arguments_, "--compatibility-schema") ?? join(productRoot, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json")),
    modelRouting: join(instanceRoot, "config", "model-routing.json"),
    // Review profiles are product-shipped (ADR 0019). Resolving them under the
    // instance made `status` and `doctor` fail deterministically on a correct
    // split install, which simply does not hold this product-owned file.
    reviewProfile: resolve(option(arguments_, "--review-profile")
      ?? join(productRoot, "config", "review-profiles", "certifying-review-four-slot-v1.json")),
  };
}

async function bounded<T>(operation: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  return await new Promise<T>((resolvePromise, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} timed out after ${String(timeoutMs)}ms`)), timeoutMs);
    operation.then(
      (value) => { clearTimeout(timer); resolvePromise(value); },
      (error: unknown) => { clearTimeout(timer); reject(error); },
    );
  });
}

function probeFailure(error: unknown): { state: "drifted" | "unknown"; detail: string } {
  const messages: string[] = [];
  const codes: string[] = [];
  const seen = new Set<Error>();
  let current = error;
  while (current instanceof Error && !seen.has(current)) {
    seen.add(current);
    messages.push(current.message);
    if ("code" in current && typeof current.code === "string") codes.push(current.code);
    current = current.cause;
  }
  const mismatch = codes.some((code) =>
    ["ADAPTER_IDENTITY_MISMATCH", "ADAPTER_INTERFACE_MISMATCH", "ADAPTER_PATH_UNSAFE"].includes(code));
  const incomplete = codes.includes(ADAPTER_INTERFACE_PROBE_INCOMPLETE);
  return {
    state: mismatch && !incomplete ? "drifted" : "unknown",
    detail: [...new Set(messages)].join(": ") || errorDetail(error),
  };
}

function primaryProviderState(observation: ProviderObservation): ProviderIdentityObservation {
  const drift: string[] = [];
  const unknown: string[] = [];
  for (const [probe, error] of [["identity", observation.identityError], ["interface", observation.interfaceError]] as const) {
    if (error === undefined) continue;
    const failure = probeFailure(error);
    (failure.state === "drifted" ? drift : unknown).push(`${probe} probe ${failure.state === "drifted" ? "mismatch" : "unavailable"}: ${failure.detail}`);
  }
  if (observation.identity !== undefined) {
    const expectedAssurance = providerIdentityAssuranceForPolicy(observation.requiredIdentity);
    if (expectedAssurance !== undefined && observation.identity.assurance !== expectedAssurance) {
      drift.push(
        `provider_identity ${observation.requiredIdentity} observed assurance ${observation.identity.assurance}; expected ${expectedAssurance}`,
      );
    }
  } else if (observation.identityError === undefined) unknown.push("identity probe did not complete");
  if (observation.providerInterface === undefined && observation.interfaceError === undefined) unknown.push("interface probe did not complete");
  if (drift.length > 0) {
    return { adapterId: observation.adapterId, state: "drifted", detail: drift.join("; ") };
  }
  if (unknown.length > 0) return { adapterId: observation.adapterId, state: "unknown", detail: unknown.join("; ") };
  return {
    adapterId: observation.adapterId,
    state: "clean",
    detail: `identity assurance ${observation.identity!.assurance}; interface ${observation.providerInterface!.probe} conformant`,
  };
}

function dateStaleness(field: DateStaleness["field"], date: unknown, now: number): DateStaleness {
  if (typeof date !== "string" || !/^\d{4}-\d{2}-\d{2}$/u.test(date)) {
    return { field, date: null, ageDays: null, stale: null };
  }
  const timestamp = Date.parse(`${date}T00:00:00Z`);
  if (!Number.isFinite(timestamp)) return { field, date, ageDays: null, stale: null };
  const ageDays = Math.floor((now - timestamp) / DAY_MS);
  return { field, date, ageDays, stale: ageDays > STALENESS_THRESHOLD_DAYS };
}

async function readDoctorMetadata(modelRoutingPath: string): Promise<{ catalogDate: unknown; modelRouting: unknown }> {
  let catalogDate: unknown;
  let modelRouting: unknown;
  try {
    modelRouting = JSON.parse(await readFile(modelRoutingPath, "utf8"));
    if (isRecord(modelRouting)) catalogDate = modelRouting.catalog_date;
  } catch {}
  return { catalogDate, modelRouting };
}

/**
 * Report the certifying profile's pinned identities against current alias
 * resolution. A pre-record install remains diagnosable and is explicitly
 * unverified; once a deployment record exists, its exact deployed document is
 * digest-bound before any pin report is derived.
 */
async function reviewProfilePins(
  agentsHome: string,
  reviewProfilePath: string,
  modelRouting: unknown,
  observe: PinRouteObserver,
): Promise<ReviewProfilePinReport & {
  catalogueDeployment: DeployedReviewProfileCatalogueReport;
}> {
  const deployment = await verifyDeployedReviewProfileCatalogue({
    agentsHome,
    profilePath: reviewProfilePath,
  });
  const catalogue = deployment.catalogue;
  const catalogueDeployment: DeployedReviewProfileCatalogueReport = deployment.status === "verified"
    ? {
        status: deployment.status,
        profile: deployment.profile,
        record: deployment.record,
        digest: deployment.digest,
        repairCommand: deployment.repairCommand,
      }
    : deployment;
  if (!isRecord(catalogue) || !Array.isArray(catalogue.chairProfiles)) {
    return { compared: [], attested: [], catalogueDeployment };
  }
  const report = await evaluateReviewProfilePinDrift({
    catalogue: catalogue as unknown as ReviewProfileCatalogue,
    observations: readPinObservations(catalogue),
    routing: modelRouting,
    observe,
  });
  return { ...report, catalogueDeployment };
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

async function seatStatus(
  paths: FabricPaths,
  project: string,
  productRoot: string,
): Promise<Array<Record<string, unknown>>> {
  const registered = new Map<string, SeatMetadata>();
  for (const seat of MCP_SEATS) {
    try {
      const location = await resolveSeatPaths({ stateDirectory: paths.stateDirectory, project, seat });
      const metadata: unknown = JSON.parse(await readFile(location.metadataPath, "utf8"));
      if (typeof metadata !== "object" || metadata === null) throw new Error("metadata is invalid");
      registered.set(seat, metadata as SeatMetadata);
    } catch {
      // A missing or invalid seat is reported after the complete roster scan so
      // the remedy can distinguish zero-state bootstrap from an absent peer.
    }
  }
  const chairExists = [...registered.values()].some(({ role }) => role === "chair");
  const renewalPeerSeat = [...registered.entries()]
    .find(([, metadata]) => metadata.role === "peer")?.[0] as (typeof MCP_SEATS)[number] | undefined;
  const bootstrapChairSeat = [...registered.entries()]
    .find(([, metadata]) => metadata.role === "chair" && metadata.originKind !== "provisioned")
    ?.[0] as (typeof MCP_SEATS)[number] | undefined;
  const provisionedChair = [...registered.entries()]
    .find(([, metadata]) => metadata.role === "chair" && metadata.originKind === "provisioned");
  const provisionedChairSeat = provisionedChair?.[0] as (typeof MCP_SEATS)[number] | undefined;
  // One read port serves the chair authority ceiling and every seat's warning
  // check within this status composition; the finally closes it once the
  // roster rows are built.
  const rosterPort = openRosterReadPort(paths.databasePath);
  try {
    const chairAuthorityExpiresAt = provisionedChair === undefined
      ? null
      : rosterPort.chairAuthorityExpiresAt({
          runId: provisionedChair[1].runId,
          chairAgentId: provisionedChair[1].chairAgentId,
        });
    return MCP_SEATS.map((seat) => {
      const value = registered.get(seat);
      if (value !== undefined) {
        const remainingMs = Date.parse(value.expiresAt) - Date.now();
        return {
          seat,
          agentId: value.agentId,
          role: value.role,
          originKind: value.originKind ?? "legacy-bootstrap",
          runId: value.runId,
          expiresAt: value.expiresAt,
          active: remainingMs > 0,
          registered: true,
          // The warning window derives from the roster's own lifetime so it can
          // distinguish plenty-of-time from act-now, and an expired seat keeps
          // its remedy because expiry is recoverable, not a dead end (#526).
          ...(value.originKind === "provisioned" &&
            seatExpiryWarningDue({
              port: rosterPort,
              generation: value.generation,
              expiresAt: value.expiresAt,
            }) &&
            renewalPeerSeat !== undefined
            ? {
                remedy: bootstrapChairSeat === undefined
                  ? (() => {
                      const renewal = mcpRosterRenewalCommand({
                        project,
                        peerSeat: renewalPeerSeat,
                        currentExpiresAt: value.expiresAt,
                        chairAuthorityExpiresAt,
                        productRoot,
                      });
                      return renewal ??
                        `the provisioned roster cannot be renewed; use ${
                          mcpBootstrapRenewalCommand(project, provisionedChairSeat ?? "codex", productRoot)
                        }`;
                    })()
                  : mcpBootstrapRenewalCommand(project, bootstrapChairSeat, productRoot),
              }
            : {}),
        };
      }
      return chairExists
        ? {
            seat,
            registered: false,
            active: false,
            reason: "PEER_SEAT_NOT_PROVISIONED",
            remedy: `${fabricCliCommand({ productRootFlag: productRoot })} mcp peer-provision --project ${shellQuote(project)} --seat ${seat}`,
          }
        : {
            seat,
            registered: false,
            active: false,
            reason: "PROJECT_NOT_BOOTSTRAPPED",
            remedy: `cd ${shellQuote(project)} && ${fabricCliCommand({ productRootFlag: productRoot })} bootstrap --seat codex`,
          };
    });
  } finally {
    rosterPort.close();
  }
}

export async function fabricStatus(
  arguments_: string[],
  paths: FabricPaths,
  dependencies: StatusDependencies = {},
): Promise<Record<string, unknown>> {
  const selected = resolveStatusPaths(arguments_);
  const project = resolve(option(arguments_, "--project") ?? process.cwd());
  const boundary = await resolveProjectBoundary(project);
  const projectConfigPath = boundary.evidence.kind === "refused"
    ? undefined
    : projectConfigPathAtExactRoot(boundary.selectedProjectRoot);
  // ${AGENTS_HOME} expands against the product root (#528); the layering is the
  // same composition daemon startup performs, so a widening instance file is
  // refused here exactly as the daemon would refuse it.
  const config = await loadFabricConfig({
    globalPath: selected.config,
    ...(selected.localConfig === undefined ? {} : { localPath: selected.localConfig }),
    ...(projectConfigPath === undefined ? {} : { projectPath: projectConfigPath }),
    agentsHome: selected.productRoot,
  });
  const roots = [...new Set([...config.workspaceRoots, ...await trustedWorkspaceRoots({ stateDirectory: paths.stateDirectory, executionProfile: config.executionProfile ?? "headless" })])].sort();
  const daemon = await daemonState(paths, dependencies);
  const compatibility = await verifyAdapterCompatibility({
    compatibilityPath: selected.compatibility,
    schemaPath: selected.compatibilitySchema,
    adapterIds: config.adapterIds,
    requireEnabled: true,
    allowUnavailableOptional: true,
    resolveExecutables: false,
  });
  return {
    schemaVersion: 1,
    daemon,
    executionProfile: config.executionProfile ?? "headless",
    configuredAdapters: config.adapterIds,
    activeAdapters: daemon.activeAdapters,
    optionalAdapters: compatibility.unavailableOptionalAdapters,
    trustedWorkspaceRoots: roots,
    project: { path: project, seats: await seatStatus(paths, project, selected.productRoot) },
  };
}

function precondition(id: string): string {
  return PRECONDITIONS[id] ?? id;
}

async function check(id: string, operation: () => string | undefined | Promise<string | undefined>): Promise<Check> {
  try {
    const detail = await operation();
    return { id, status: "pass", code: checkCode(id, "OK"), detail: detail === undefined || detail.length === 0 ? "ok" : detail, precondition: precondition(id) };
  } catch (error: unknown) {
    return { id, status: "fail", code: errorCode(error, checkCode(id, "FAILED")), detail: errorDetail(error), precondition: precondition(id) };
  }
}

async function socketIsAbsent(socketPath: string): Promise<boolean> {
  try {
    await lstat(socketPath);
    return false;
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") return true;
    throw error;
  }
}

async function doctorDaemonState(
  paths: FabricPaths,
  dependencies: DoctorDependencies,
): Promise<DoctorDaemonState> {
  try {
    const runtimeInfo = await lstat(paths.runtimeDirectory);
    if (!runtimeInfo.isDirectory() || runtimeInfo.isSymbolicLink()) {
      throw new Error(`${paths.runtimeDirectory} must be a non-symlink directory`);
    }
    return await new BootstrapElection({ runtimeDirectory: paths.runtimeDirectory }).inspectCurrentReadOnlyWith(async (election) => {
      if (election.status === "active") {
        return {
          status: "failed" as const,
          code: "BOOTSTRAP_IN_PROGRESS",
          detail: "bootstrap election is active",
          pid: null,
          socketPath: null,
        };
      }
      const shutdownPath = join(paths.runtimeDirectory, "daemon-shutdown.lock");
      const shutdown = await socketIsAbsent(shutdownPath)
        ? undefined
        : await FLOCK_ELECTION_LOCK_PORT.probe(shutdownPath);
      if (shutdown?.status === "held") {
        return {
          status: "failed" as const,
          code: "DAEMON_SHUTDOWN_IN_PROGRESS",
          detail: "daemon shutdown transition is active",
          pid: null,
          socketPath: null,
        };
      }
      try {
      const discovery = await readPrivateDiscovery(privateDiscoveryPaths(paths.runtimeDirectory), paths.socketPath);
    if (discovery.status === "absent" || discovery.status === "terminal") {
      if (discovery.status === "terminal" && discovery.owner.state === "crashed") {
        return {
          status: "failed",
          code: "DAEMON_PROCESS_CRASHED",
          detail: `daemon generation crashed (exit=${String(discovery.owner.exitCode)} signal=${String(discovery.owner.signal)})`,
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      if (discovery.status === "terminal" && discovery.owner.state !== "stopped") {
        return {
          status: "failed",
          code: "DAEMON_DISCOVERY_INVALID",
          detail: `terminal daemon discovery state ${String(discovery.owner.state)} is not a clean stop`,
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      if (
        discovery.status === "terminal"
        && (discovery.owner.exitCode !== 0 || discovery.owner.signal !== null)
      ) {
        return {
          status: "failed",
          code: "DAEMON_PROCESS_UNCLEAN_STOP",
          detail: `daemon stopped uncleanly (exit=${String(discovery.owner.exitCode)} signal=${String(discovery.owner.signal)})`,
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      if (discovery.status === "absent" && election.status !== "absent") {
        if (election.status === "terminal") {
          return {
            status: "failed",
            code: election.receipt.code,
            detail: election.receipt.message,
            pid: null,
            socketPath: null,
          };
        }
        return {
          status: "failed",
          code: "DAEMON_DISCOVERY_MISSING",
          detail: "bootstrap completed but no generation-bound daemon discovery is available",
          pid: null,
          socketPath: null,
        };
      }
      if (!await socketIsAbsent(paths.socketPath)) {
        return {
          status: "failed",
          code: "DAEMON_SOCKET_STALE",
          detail: "daemon socket exists without an active generation-bound owner",
          pid: discovery.status === "terminal" ? discovery.owner.pid : null,
          socketPath: paths.socketPath,
        };
      }
      if (
        discovery.status === "terminal"
        && (election.status !== "ready" || !generationIdentityMatches(discovery.owner, election.receipt))
      ) {
        return {
          status: "failed",
          code: "DAEMON_ELECTION_INCONSISTENT",
          detail: "terminal daemon discovery has no matching successful bootstrap election",
          pid: discovery.owner.pid,
          socketPath: null,
        };
      }
      return {
        status: "idle",
        code: "DAEMON_ON_DEMAND_IDLE",
        detail: discovery.status === "terminal" ? "on-demand daemon stopped cleanly" : "on-demand daemon has not been started",
        pid: null,
        socketPath: null,
      };
    }
    if (discovery.status === "ambiguous") {
      return {
        status: "failed",
        code: "DAEMON_DISCOVERY_AMBIGUOUS",
        detail: discovery.message,
        pid: discovery.owner?.pid ?? discovery.receipt?.pid ?? null,
        socketPath: paths.socketPath,
      };
    }
    if (
      election.status !== "ready" ||
      !generationIdentityMatches(discovery.owner, election.receipt)
    ) {
      return {
        status: "failed",
        code: "DAEMON_ELECTION_INCONSISTENT",
        detail: "active daemon discovery does not match the successful bootstrap election",
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    try {
      process.kill(discovery.receipt.pid, 0);
    } catch (error: unknown) {
      return {
        status: "failed",
        code: "DAEMON_PROCESS_UNAVAILABLE",
        detail: errorDetail(error),
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    let info;
    try {
      info = await (dependencies.inspectDaemonSocket ?? lstat)(discovery.receipt.socketPath);
    } catch (error: unknown) {
      return {
        status: "failed",
        code: "DAEMON_SOCKET_UNAVAILABLE",
        detail: errorDetail(error),
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    if (!info.isSocket() || info.uid !== process.getuid?.()) {
      return {
        status: "failed",
        code: "DAEMON_SOCKET_UNSAFE",
        detail: "daemon socket is not owned by the current user",
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    try {
      const client = await (dependencies.connectDaemon ?? connectFabricDaemon)({
        socketPath: discovery.receipt.socketPath,
        capability: discovery.receipt.bootstrapCapability,
      });
      try {
        // Negotiation pins the credential result contract without another RPC.
        // The probe only confirms bootstrap-scope dispatch and connection liveness.
        assertRequiredResultShapeFeatures(
          client.initializeResult.capabilities,
          [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
        );
        await client.probeBootstrapContract();
      } finally {
        await client.close();
      }
    } catch (error: unknown) {
      const errorCodeValue = errorCode(error, "DAEMON_HANDSHAKE_FAILED");
      const code = ["PROTOCOL_INCOMPATIBLE", "DAEMON_PROTOCOL_MISMATCH", "DAEMON_PROTOCOL_UNSUPPORTED"].includes(errorCodeValue)
        ? "DAEMON_PROTOCOL_INCOMPATIBLE"
        : "DAEMON_HANDSHAKE_FAILED";
      return {
        status: "failed",
        code,
        detail: code === "DAEMON_PROTOCOL_INCOMPATIBLE"
          ? `${errorDetail(error)}; restart the incumbent through its owning Fabric lifecycle, then retry provenant doctor`
          : errorDetail(error),
        pid: discovery.receipt.pid,
        socketPath: discovery.receipt.socketPath,
      };
    }
    return {
      status: "live",
      code: "DAEMON_LIVE",
      detail: "daemon discovery, process, socket, negotiation and bootstrap contract probe are healthy",
      pid: discovery.receipt.pid,
      socketPath: discovery.receipt.socketPath,
    };
      } finally {
        if (shutdown?.status === "acquired") await shutdown.handle.release();
      }
    });
  } catch (error: unknown) {
    return {
      status: "failed",
      code: errorCode(error, "DAEMON_DISCOVERY_FAILED"),
      detail: errorDetail(error),
      pid: null,
      socketPath: null,
    };
  }
}

export async function fabricDoctor(
  arguments_: string[],
  paths: FabricPaths,
  dependencies: DoctorDependencies = {},
): Promise<Record<string, unknown>> {
  const selected = resolveStatusPaths(arguments_);
  const project = resolve(option(arguments_, "--project") ?? process.cwd());
  const boundary = await resolveProjectBoundary(project);
  const projectConfigPath = boundary.evidence.kind === "refused"
    ? undefined
    : projectConfigPathAtExactRoot(boundary.selectedProjectRoot);
  const consumeProviderQuota = arguments_.includes("--consume-provider-quota");
  let adapterIds: string[] = [];
  let adapterCommands: string[][] = [];
  let compatibilityVerification: Awaited<ReturnType<typeof verifyAdapterCompatibility>> | undefined;
  const optionalAdapterFailures: AdapterExecutableFailure[] = [];
  const providerObservations: ProviderObservation[] = [];
  const checks: Check[] = [];
  checks.push(await check("protocol-build", async () => {
    if (process.env.AGENT_FABRIC_PROTOCOL_BUILD_VERDICT === "stale") {
      const repair = process.env.AGENT_FABRIC_PROTOCOL_BUILD_REPAIR;
      throw Object.assign(
        new Error(
          repair === undefined || repair.length === 0
            ? "the launcher observed a stale protocol dist but did not provide a repair command"
            : `the launcher observed a stale protocol dist; repair: ${repair}`,
        ),
        { code: "AGENT_FABRIC_PROTOCOL_BUILD_STALE" },
      );
    }
    await (dependencies.preflightProtocolBuild ?? preflightProtocolBuild)();
    return "protocol dist is present and current for its build inputs";
  }));
  checks.push(await check("configuration", async () => {
    // Same composition as daemon startup, so this check answers the question an
    // operator is actually asking: would the daemon accept this configuration.
    const config = await loadFabricConfig({
      globalPath: selected.config,
      ...(selected.localConfig === undefined ? {} : { localPath: selected.localConfig }),
      ...(projectConfigPath === undefined ? {} : { projectPath: projectConfigPath }),
      agentsHome: selected.productRoot,
    });
    adapterIds = config.adapterIds;
    adapterCommands = adapterIds.map((adapterId) => config.adapterCommands[adapterId] ?? []);
  }));
  checks.push(await check("wrapper-loader", async () => {
    return await verifyConfiguredTsxLoaders(adapterCommands, selected.productRoot);
  }));
  checks.push(await check("adapter-compatibility", async () => {
    compatibilityVerification = await verifyAdapterCompatibility({ compatibilityPath: selected.compatibility, schemaPath: selected.compatibilitySchema, adapterIds, requireEnabled: true, allowUnavailableOptional: true });
    optionalAdapterFailures.push(...compatibilityVerification.unavailableOptionalAdapters);
    return compatibilityVerification.wrapperProvenance
      .map((item) => `${item.adapterId}=${item.repositoryCommit}:${item.wrapperPath}`)
      .join(" ");
  }));
  checks.push(await check("provider-conformance", async () => {
    const verification = compatibilityVerification ?? await verifyAdapterCompatibility({ compatibilityPath: selected.compatibility, schemaPath: selected.compatibilitySchema, adapterIds, requireEnabled: true, allowUnavailableOptional: true });
    const observations = [];
    for (const adapterId of adapterIds) {
      const executable = verification.resolvedExecutables[adapterId];
      if (executable === undefined) {
        if (isPrimaryAdapter(adapterId)) throw new Error(`provider executable is missing: ${adapterId}`);
        optionalAdapterFailures.push({ adapterId, executable: "<missing>", reasons: [`provider executable is missing: ${adapterId}`] });
        continue;
      }
      try {
        const policy = await loadAdapterModelConstraints({
          compatibilityPath: selected.compatibility,
          schemaPath: selected.compatibilitySchema,
          adapterId,
        });
      if (policy.providerIdentity === undefined) continue;
      const input = {
        adapterId,
        executable,
        ...(policy.cursorInstallRoot === undefined ? {} : { cursorInstallRoot: policy.cursorInstallRoot }),
        ...(policy.providerInstallRoot === undefined ? {} : { providerInstallRoot: policy.providerInstallRoot }),
      };
      if (isPrimaryAdapter(adapterId)) {
        const timeoutMs = dependencies.providerProbeTimeoutMs ?? PROVIDER_PROBE_TIMEOUT_MS;
        const [identity, providerInterface] = await Promise.allSettled([
          bounded(Promise.resolve().then(async () => await (dependencies.verifyProviderIdentity ?? verifyProviderExecutableIdentity)(input)), timeoutMs, `${adapterId} provider identity probe`),
          bounded(Promise.resolve().then(async () => await (dependencies.probeProviderInterface ?? probeProviderInterface)(input)), timeoutMs, `${adapterId} provider interface probe`),
        ]);
        providerObservations.push({
          adapterId,
          requiredIdentity: policy.providerIdentity,
          ...(identity.status === "fulfilled" ? { identity: identity.value } : { identityError: identity.reason }),
          ...(providerInterface.status === "fulfilled"
            ? { providerInterface: providerInterface.value }
            : { interfaceError: providerInterface.reason }),
        });
        if (identity.status === "fulfilled" && providerInterface.status === "fulfilled") {
          observations.push(`${adapterId}=${providerInterface.value.version}:${identity.value.sha256}:${identity.value.assurance}`);
        }
        continue;
      }
      const observation = await (dependencies.verifyProvider ?? verifyProviderConformance)(input);
      observations.push(`${adapterId}=${observation.interface.version}:${observation.identity.sha256}:${observation.identity.assurance}`);
      } catch (error: unknown) {
        if (isPrimaryAdapter(adapterId)) throw error;
        optionalAdapterFailures.push({ adapterId, executable, reasons: [errorDetail(error)] });
      }
    }
    return observations.join(" ");
  }));
  const mergedOptionalAdapterFailures = mergeOptionalAdapterFailures(optionalAdapterFailures);
  if (mergedOptionalAdapterFailures.length > 0) {
    const providerCheck = checks.find((item) => item.id === "provider-conformance");
    if (providerCheck !== undefined && providerCheck.status === "pass") {
      providerCheck.status = "idle";
      providerCheck.code = "OPTIONAL_ADAPTERS_DEGRADED";
      providerCheck.detail = [
        providerCheck.detail,
        optionalAdapterFailureDetail(mergedOptionalAdapterFailures),
      ].filter((item) => item.length > 0).join(" ");
    }
  }
  const metadata = await readDoctorMetadata(selected.modelRouting);
  const providerIdentity = providerObservations.map((observation) =>
    primaryProviderState(observation));
  const drifted = providerIdentity.some((item) => item.state === "drifted");
  const unknown = providerIdentity.some((item) => item.state === "unknown");
  checks.push({
    id: "provider-identity",
    status: drifted ? "fail" : unknown ? "idle" : "pass",
    code: drifted ? "PROVIDER_IDENTITY_DRIFT" : unknown ? "PROVIDER_IDENTITY_UNKNOWN" : "PROVIDER_IDENTITY_OK",
    detail: providerIdentity.map((item) => `${item.adapterId}=${item.state}: ${item.detail}`).join(" "),
    precondition: precondition("provider-identity"),
  });
  const pinReport = await reviewProfilePins(
    // The catalogue is product-shipped and `selected.reviewProfile` resolves
    // under the product root, so the root this path is validated against must
    // be the product root too. Passing the instance root made doctor reject its
    // own resolved path with ARTIFACT_PATH_FORBIDDEN on a split install.
    selected.productRoot,
    selected.reviewProfile,
    metadata.modelRouting,
    consumeProviderQuota
      ? dependencies.observeReviewProfilePin ?? createCapabilityPinObserver({
          agentsHome: selected.productRoot,
          cacheDirectory: paths.stateDirectory,
          forceLive: true,
          ...(dependencies.now === undefined ? {} : { now: dependencies.now }),
        })
      : createCapabilityPinObserver({
          agentsHome: selected.productRoot,
          cacheDirectory: paths.stateDirectory,
          cacheOnly: true,
          ...(dependencies.now === undefined ? {} : { now: dependencies.now }),
        }),
  );
  const pinOutcome = reviewProfilePinOutcome(pinReport);
  checks.push({
    id: "review-profile-pins",
    precondition: consumeProviderQuota
      ? LIVE_REVIEW_PROFILE_PIN_PRECONDITION
      : CACHED_REVIEW_PROFILE_PIN_PRECONDITION,
    ...pinOutcome,
    detail: [
      ...pinReport.compared.map((pin) =>
        `${pin.providerFamily}/${pin.model}=${pin.state}: ${pin.detail}`),
      ...pinReport.attested.map((pin) =>
        `${pin.providerFamily}/${pin.model}=attested observed_on=${pin.observedOn}: ${pin.detail}`),
      ...(pinOutcome.status === "fail" ? [`repair with ${PROFILE_PIN_REPAIR_COMMAND}`] : []),
    ].join(" ") || "no certifying profile pins in the comparison set",
  });
  const now = (dependencies.now ?? Date.now)();
  const observedDates = [...pinReport.compared.map((pin) => pin.observedOn), ...pinReport.attested.map((pin) => pin.observedOn)]
    .filter((value): value is string => value !== null)
    .sort();
  const staleness = {
    modelRouting: dateStaleness("catalog_date", metadata.catalogDate, now),
    reviewProfile: dateStaleness("observed_on", observedDates[0], now),
  };
  checks.push({
    id: "source-staleness",
    status: "pass",
    code: "SOURCE_STALENESS_ADVISORY",
    detail: [
      `catalog_date=${staleness.modelRouting.date ?? "unknown"} age_days=${String(staleness.modelRouting.ageDays)}`,
      `review_profile_observed_on=${staleness.reviewProfile.date ?? "unknown"} age_days=${String(staleness.reviewProfile.ageDays)}`,
      `threshold_days=${String(STALENESS_THRESHOLD_DAYS)}`,
    ].join(" "),
    precondition: precondition("pin-staleness"),
  });
  for (const [id, path, expectedKind] of [
    ["state-directory", paths.stateDirectory, "directory"],
    ["runtime-directory", paths.runtimeDirectory, "directory"],
  ] as const) {
    checks.push(await check(id, async () => {
      const info = await lstat(path);
      if (info.isSymbolicLink() || (expectedKind === "directory" && !info.isDirectory()) || (info.mode & 0o077) !== 0) throw new Error(`${path} must be a private non-symlink directory`);
    }));
  }
  checks.push(await check("database-integrity", async () => {
    withPrivateDatabaseClone(paths.databasePath, (snapshotPath) => {
      const database = new Database(snapshotPath, { fileMustExist: true });
      try { assertDatabaseIntegrity(database); } finally { database.close(); }
    });
  }));
  const daemon = await doctorDaemonState(paths, dependencies);
  checks.push({
    id: "daemon-socket",
    status: daemon.status === "live" ? "pass" : daemon.status === "idle" ? "idle" : "fail",
    code: daemon.code,
    detail: daemon.detail,
    precondition: precondition("daemon-socket"),
  });
  const failed = checks.find((item) => item.status === "fail");
  const healthy = failed === undefined;
  const state: DoctorLifecycleState = healthy
    ? daemon.status === "live" ? "current" : "idle"
    : RECOVERABLE_CODES.has(failed.code) ? "recovering" : "blocked";
  const daemonCheck = checks.find((item) => item.id === "daemon-socket");
  // The deciding check, worst first. An unknown precondition is surfaced ahead
  // of the daemon summary even when the lifecycle is healthy, so `cause` never
  // reports a satisfied precondition while a declared one is unresolved.
  const holds = failed
    ?? checks.find((item) => item.status === "idle" && item.id !== "daemon-socket")
    ?? daemonCheck;
  return {
    schemaVersion: 1,
    healthy,
    state,
    code: failed?.code ?? (mergedOptionalAdapterFailures.length > 0 ? "OPTIONAL_ADAPTERS_DEGRADED" : daemon.code),
    // Why the state holds, not merely which state it is: the exact check whose
    // precondition decided it, and whether this lifecycle may repair it.
    // `satisfied` reports that check's own precondition and is independent of
    // `healthy`: a healthy idle lifecycle has no daemon by design, and an
    // unknown provider probe stays advisory (#406) without being called
    // satisfied. Only a `pass` check reports a satisfied precondition.
    cause: {
      checkId: holds?.id ?? "daemon-socket",
      precondition: holds?.precondition ?? precondition("daemon-socket"),
      satisfied: holds?.status === "pass",
      code: holds?.code ?? daemon.code,
      detail: holds?.detail ?? daemon.detail,
      recoverable: !healthy && state === "recovering",
    },
    wrapperIntercepted: [],
    daemon: {
      status: daemon.status,
      pid: daemon.pid,
      socketPath: daemon.socketPath,
    },
    optionalAdapters: mergedOptionalAdapterFailures,
    providerIdentity: { adapters: providerIdentity },
    reviewProfilePins: { repairCommand: PROFILE_PIN_REPAIR_COMMAND, ...pinReport },
    staleness: {
      advisory: true,
      thresholdDays: STALENESS_THRESHOLD_DAYS,
      modelRouting: staleness.modelRouting,
      reviewProfile: staleness.reviewProfile,
    },
    checks,
  };
}
