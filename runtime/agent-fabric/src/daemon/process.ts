import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import {
  chmodSync,
  constants as fsConstants,
  copyFileSync,
  existsSync,
  mkdirSync,
  rmdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createConnection, createServer, type Socket } from "node:net";
import { join } from "node:path";

import {
  FABRIC_OPERATIONS,
  PROTOCOL_FEATURES,
  PROTOCOL_LIMITS,
} from "@local/agent-fabric-protocol";

import { openFabric } from "../core/open-fabric.js";
import {
  openLocalLifecycleReceiptAuthority,
  type LocalLifecycleReceiptAuthority,
} from "../lifecycle/local-receipt-authority.js";
import {
  bindCurrentMcpSeatsInput,
  bootstrapMcpSeatInput,
  FABRIC_DAEMON_VERSION,
  daemonInitializeParams,
  daemonInitializeResult,
  dispatchClientMethod,
  FABRIC_PROTOCOL_VERSION,
  issueLocalOperatorSessionCapabilityInput,
  isDaemonRequest,
  isRecord,
  openLocalOperatorConsoleCapabilityInput,
  openLocalOperatorTakeoverCapabilityInput,
  provisionLocalOperatorInput,
  rotateLocalOperatorPrincipalInput,
  type DaemonRequest,
} from "./protocol.js";
import { parseDaemonAdapters } from "./composition.js";
import {
  composeHerdrDaemonIntegration,
  herdrPresencePollInterval,
  parseHerdrDaemonProcessConfiguration,
} from "./herdr-composition.js";
import { BootstrapElection, FLOCK_ELECTION_LOCK_PORT, type ElectionLockHandle } from "./bootstrap-election.js";
import { GuardedIdleStopController, type IdleStopResult, type QuiesceToken } from "../lifecycle/global-liveness.js";
import { IdleShutdownScheduler } from "./idle-shutdown-scheduler.js";
import { ResultDeadlineScheduler } from "./result-deadline-scheduler.js";
import { finalizeDaemonShutdown } from "./shutdown-finalizer.js";
import { acquireDaemonLocks, releaseDaemonLocks, writeDaemonLockReceipt } from "./client.js";
import {
  markPrivateDiscoveryTerminal,
  privateDiscoveryPaths,
  readPrivateDiscovery,
} from "./private-discovery.js";
import { routeDaemonConnection } from "./connection-router.js";
import { servePublicProtocolConnection } from "./public-protocol.js";
import { BoundedNdjsonReader, BoundedNdjsonWriter, FABRIC_PROTOCOL_LIMITS } from "../transport/bounded-ndjson.js";
import { trustedWorkspaceIdentity } from "../cli/workspace-trust.js";
import {
  createOptionalGitHubHostedChecksAdapter,
  type OptionalGitHubHostedChecksConfiguration,
} from "../operator/github-hosted-checks.js";
import type { TrustedGitConfiguration } from "../operator/trusted-git-registry.js";
import { inspectFabricDatabase } from "../core/migrations.js";
import {
  closeRecoverableUnixListener,
  openRecoverableUnixListener,
  RecoverableServingAdmissionFence,
} from "./recoverable-serving-socket.js";

class DaemonProtocolError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "DaemonProtocolError";
    this.code = code;
  }
}

async function reportPreReadyFailure(error: unknown, fallbackCode: string): Promise<never> {
  const code = typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : fallbackCode;
  const message = error instanceof Error ? error.message : String(error);
  const preserved = typeof error === "object" && error !== null && "preserved" in error && error.preserved === true;
  await new Promise<void>((resolve) => process.stdout.write(
    `${JSON.stringify({ ready: false, error: { code, message, ...(preserved ? { preserved: true } : {}) } })}\n`,
    () => resolve(),
  ));
  process.exit(1);
}

function removeOwnedEmptyDirectory(path: string, existedBeforeBootstrap: boolean): void {
  if (existedBeforeBootstrap) return;
  try {
    rmdirSync(path);
  } catch (error: unknown) {
    if (!(error instanceof Error && "code" in error && (error.code === "ENOENT" || error.code === "ENOTEMPTY"))) {
      throw error;
    }
  }
}

function secretEqual(left: string, right: string): boolean {
  const leftDigest = createHash("sha256").update(left).digest();
  const rightDigest = createHash("sha256").update(right).digest();
  return timingSafeEqual(leftDigest, rightDigest);
}

function localAuthenticatedSubjectHash(key: string): `sha256:${string}` {
  if (typeof process.getuid !== "function") {
    throw new DaemonProtocolError(
      "LOCAL_SUBJECT_UNAVAILABLE",
      "local operator provisioning requires an authenticated Unix process owner",
    );
  }
  return `sha256:${createHmac("sha256", key)
    .update(`agent-fabric.local-operator.v1\0uid:${String(process.getuid())}`)
    .digest("hex")}`;
}

const databasePath = process.env.AGENT_FABRIC_DATABASE_PATH;
const productRoot = process.env.AGENT_FABRIC_PRODUCT_ROOT;
const socketPath = process.env.AGENT_FABRIC_SOCKET_PATH;
const stateDirectory = process.env.AGENT_FABRIC_STATE_DIRECTORY;
const runtimeDirectory = process.env.AGENT_FABRIC_RUNTIME_DIRECTORY;
const bootstrapCapability = process.env.AGENT_FABRIC_BOOTSTRAP_CAPABILITY;
const bootstrapMode = process.env.AGENT_FABRIC_BOOTSTRAP_MODE;
const bootstrapActionId = process.env.AGENT_FABRIC_BOOTSTRAP_ACTION_ID;
const bootstrapElectionGeneration = Number(process.env.AGENT_FABRIC_BOOTSTRAP_ELECTION_GENERATION);
const bootstrapCustody = process.env.AGENT_FABRIC_BOOTSTRAP_CUSTODY;
const daemonLockPathsValue: unknown = JSON.parse(process.env.AGENT_FABRIC_DAEMON_LOCK_PATHS_JSON ?? "[]");
const daemonLockPaths = Array.isArray(daemonLockPathsValue) && daemonLockPathsValue.every((value) => typeof value === "string")
  ? daemonLockPathsValue
  : undefined;
const capabilityKey = process.env.AGENT_FABRIC_CAPABILITY_KEY;
const executionProfile = process.env.AGENT_FABRIC_EXECUTION_PROFILE;
const lifecycleReceiptAuthorityId = process.env.AGENT_FABRIC_LIFECYCLE_RECEIPT_AUTHORITY_ID;
const maximumConcurrentProviderTurnsValue = process.env.AGENT_FABRIC_MAXIMUM_CONCURRENT_PROVIDER_TURNS ?? "8";
const maximumConcurrentProviderTurns = Number(maximumConcurrentProviderTurnsValue);
const workspaceRootsValue: unknown = JSON.parse(process.env.AGENT_FABRIC_WORKSPACE_ROOTS_JSON ?? "[]");
const workspaceRoots = Array.isArray(workspaceRootsValue) && workspaceRootsValue.every((value) => typeof value === "string")
  ? workspaceRootsValue
  : undefined;
const githubHostedChecksValue: unknown = JSON.parse(
  process.env.AGENT_FABRIC_GITHUB_HOSTED_CHECKS_JSON ?? '{"enabled":false}',
);
const trustedGitValue: unknown = JSON.parse(process.env.AGENT_FABRIC_TRUSTED_GIT_JSON ?? "{}");
if (!isRecord(trustedGitValue)) throw new Error("trusted Git daemon composition must be an object");
const trustedGitConfiguration = trustedGitValue as TrustedGitConfiguration;
const herdrProcessConfiguration = parseHerdrDaemonProcessConfiguration(
  process.env.AGENT_FABRIC_HERDR_JSON,
);
const daemonInstanceGenerationValue = process.env.AGENT_FABRIC_DAEMON_INSTANCE_GENERATION;
const daemonInstanceGeneration = Number(daemonInstanceGenerationValue);

if (
  databasePath === undefined ||
  productRoot === undefined ||
  socketPath === undefined ||
  stateDirectory === undefined ||
  runtimeDirectory === undefined ||
  bootstrapCapability === undefined
  || (bootstrapMode !== "production-election" && bootstrapMode !== "test-forced-process-locks")
  || bootstrapCustody !== "parent-pipe-v1"
  || (bootstrapMode === "production-election" && (
    bootstrapActionId === undefined ||
    bootstrapActionId.trim().length === 0 ||
    !Number.isSafeInteger(bootstrapElectionGeneration) ||
    bootstrapElectionGeneration < 1
  ))
  || (bootstrapMode === "test-forced-process-locks" && (
    daemonLockPaths === undefined || daemonLockPaths.length !== 2
  ))
  || capabilityKey === undefined
  || executionProfile === undefined
  || !Number.isInteger(maximumConcurrentProviderTurns)
  || maximumConcurrentProviderTurns < 1
  || workspaceRoots === undefined
  || workspaceRoots.length === 0
  || !Number.isSafeInteger(daemonInstanceGeneration)
  || daemonInstanceGeneration < 1
) {
  throw new Error("agent fabric daemon environment is incomplete");
}

let bootstrapCustodyReleased = false;
let bootstrapCustodyMessage = "";
let bootstrapSocketOwned = false;
const releaseBootstrapCustody = "release\n";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk: string) => {
  if (bootstrapCustodyReleased) return;
  bootstrapCustodyMessage += chunk;
  if (!releaseBootstrapCustody.startsWith(bootstrapCustodyMessage)) process.exit(1);
  if (bootstrapCustodyMessage === releaseBootstrapCustody) bootstrapCustodyReleased = true;
});
const failClosedOnBootstrapCustodyLoss = (): void => {
  if (!bootstrapCustodyReleased) process.exit(1);
};
process.stdin.on("end", failClosedOnBootstrapCustodyLoss);
process.stdin.on("error", failClosedOnBootstrapCustodyLoss);
process.stdin.resume();
process.once("exit", () => {
  if (!bootstrapCustodyReleased && bootstrapSocketOwned) rmSync(socketPath, { force: true });
});

if (bootstrapMode === "production-election") {
  try {
    inspectFabricDatabase(databasePath);
  } catch (error: unknown) {
    await reportPreReadyFailure(error, "DAEMON_DATABASE_PREFLIGHT_FAILED");
  }
}

const localSubjectHash = localAuthenticatedSubjectHash(capabilityKey);
const trustedStateDirectory = stateDirectory;
const trustedExecutionProfile = executionProfile;

async function withTrustedLocalSubject<T extends {
  canonicalRoot: string;
  trustRecordDigest: string;
}>(input: T): Promise<T & { authenticatedSubjectHash: string }> {
  let trusted: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>;
  try {
    trusted = await trustedWorkspaceIdentity({
      stateDirectory: trustedStateDirectory,
      canonicalRoot: input.canonicalRoot,
      executionProfile: trustedExecutionProfile,
    });
  } catch {
    throw new DaemonProtocolError(
      "WORKSPACE_TRUST_INVALID",
      "local operator workspace trust could not be revalidated",
    );
  }
  if (
    trusted.canonicalRoot !== input.canonicalRoot ||
    trusted.trustRecordDigest !== input.trustRecordDigest
  ) {
    throw new DaemonProtocolError(
      "TRUST_RECORD_CHANGED",
      "local operator workspace trust binding changed",
    );
  }
  return { ...input, authenticatedSubjectHash: localSubjectHash };
}

const stateDirectoryExistedBeforeBootstrap = existsSync(stateDirectory);
const runtimeDirectoryExistedBeforeBootstrap = existsSync(runtimeDirectory);
mkdirSync(stateDirectory, { recursive: true, mode: 0o700 });
mkdirSync(runtimeDirectory, { recursive: true, mode: 0o700 });
chmodSync(stateDirectory, 0o700);
chmodSync(runtimeDirectory, 0o700);
const cutoverRaceFixture = process.env.NODE_ENV === "test"
  ? process.env.AGENT_FABRIC_TEST_CUTOVER_RACE_FIXTURE_PATH
  : undefined;
let malformedBootstrapResultRemaining = process.env.NODE_ENV === "test"
  ? Number(process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT ?? "0")
  : 0;
if (!Number.isSafeInteger(malformedBootstrapResultRemaining) || malformedBootstrapResultRemaining < 0) {
  malformedBootstrapResultRemaining = 0;
}
if (cutoverRaceFixture !== undefined) {
  copyFileSync(cutoverRaceFixture, databasePath, fsConstants.COPYFILE_EXCL);
}
let daemonLocks: Awaited<ReturnType<typeof acquireDaemonLocks>> = [];
if (bootstrapMode === "test-forced-process-locks") {
  try {
    daemonLocks = await acquireDaemonLocks(daemonLockPaths as string[]);
    for (const lock of daemonLocks) {
      await writeDaemonLockReceipt(lock.path, { pid: process.pid, token: lock.token, socketPath });
    }
  } catch (error: unknown) {
    const code = typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
      ? error.code
      : "DAEMON_LOCKED";
    const message = error instanceof Error ? error.message : String(error);
    await new Promise<void>((resolve) => process.stdout.write(`${JSON.stringify({ ready: false, error: { code, message } })}\n`, () => resolve()));
    process.exit(1);
    throw error;
  }
  try {
    inspectFabricDatabase(databasePath);
  } catch (error: unknown) {
    await releaseDaemonLocks(daemonLocks).catch(() => undefined);
    daemonLocks = [];
    await reportPreReadyFailure(error, "DAEMON_DATABASE_PREFLIGHT_FAILED");
  }
}
const daemonAdapters = parseDaemonAdapters(process.env.AGENT_FABRIC_ADAPTERS_JSON);
const gitHostedChecks = await createOptionalGitHubHostedChecksAdapter(
  githubHostedChecksValue as OptionalGitHubHostedChecksConfiguration,
);
const herdrIntegration = composeHerdrDaemonIntegration(herdrProcessConfiguration, stateDirectory);
const initializeResult = daemonInitializeResult(Object.keys(daemonAdapters));
type PendingDaemonStop = Readonly<{
  custodyId: string;
  resultCorrelationDigest: string;
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  principalGeneration: number;
  commandId: string;
  operation: "daemon-stop";
  token: QuiesceToken;
}>;
const pendingDaemonStops = new Map<string, PendingDaemonStop>();
let scheduleIdleStop: () => void = () => undefined;
let closeBackgroundWorkers: () => void = () => undefined;
let lifecycleReceiptAuthority: LocalLifecycleReceiptAuthority | undefined;
const fabric = await (async () => {
  let opened: Awaited<ReturnType<typeof openFabric>> | undefined;
  try {
    lifecycleReceiptAuthority = lifecycleReceiptAuthorityId === undefined
      ? undefined
      : openLocalLifecycleReceiptAuthority({
          stateDirectory,
          expectedAuthorityId: lifecycleReceiptAuthorityId,
        });
    opened = await openFabric({
      databasePath,
      productRoot,
      fabricSocketPath: socketPath,
      capabilityKey,
      executionProfile,
      maximumConcurrentProviderTurns,
      workspaceRoots,
      adapters: daemonAdapters,
      ...(gitHostedChecks === undefined ? {} : { gitHostedChecks }),
      trustedGitConfiguration,
      herdr: herdrIntegration,
      ...(lifecycleReceiptAuthority === undefined ? {} : { lifecycleReceiptAuthority }),
      daemonStopPort: {
        request: async (request) => {
          const existing = pendingDaemonStops.get(request.custodyId);
          if (
            existing !== undefined &&
            (existing.resultCorrelationDigest !== request.resultCorrelationDigest ||
              existing.operatorId !== request.operatorId ||
              existing.projectId !== request.projectId ||
              existing.projectSessionId !== request.projectSessionId ||
              existing.principalGeneration !== request.principalGeneration ||
              existing.commandId !== request.commandId ||
              existing.operation !== request.operation ||
              existing.token.daemonInstanceGeneration !== request.token.daemonInstanceGeneration ||
              existing.token.observedGlobalStateRevision !== request.token.observedGlobalStateRevision)
          ) return "busy";
          pendingDaemonStops.set(request.custodyId, request);
          return "scheduled";
        },
      },
    });
    opened.recoverDaemonRuntimeEpoch({
      instanceGeneration: daemonInstanceGeneration,
      instanceId: `${bootstrapActionId ?? "forced-process"}:${String(process.pid)}`,
    });
    await opened.recoverStartupState();
    return opened;
  } catch (error: unknown) {
    await opened?.close().catch(() => undefined);
    lifecycleReceiptAuthority?.close();
    lifecycleReceiptAuthority = undefined;
    await releaseDaemonLocks(daemonLocks).catch(() => undefined);
    removeOwnedEmptyDirectory(runtimeDirectory, runtimeDirectoryExistedBeforeBootstrap);
    removeOwnedEmptyDirectory(stateDirectory, stateDirectoryExistedBeforeBootstrap);
    return await reportPreReadyFailure(error, "DAEMON_DATABASE_BOOTSTRAP_FAILED");
  }
})();
rmSync(socketPath, { force: true });
const sockets = new Set<Socket>();
const servingAdmission = new RecoverableServingAdmissionFence();
let completeQueuedDaemonStop: (commandId: string) => void = () => undefined;
let totalInFlight = 0;
const inFlightDrainers = new Set<() => void>();
const releaseInFlight = (): void => {
  totalInFlight -= 1;
  if (totalInFlight !== 0) return;
  for (const resolvePromise of inFlightDrainers) resolvePromise();
  inFlightDrainers.clear();
};
const waitForInFlight = async (): Promise<void> => {
  if (totalInFlight === 0) return;
  await new Promise<void>((resolvePromise) => inFlightDrainers.add(resolvePromise));
};
const servePrivateControlConnection = (socket: Socket): void => {
  const writer = new BoundedNdjsonWriter(socket, {
    maximumFrameBytes: FABRIC_PROTOCOL_LIMITS.maximumFrameBytes,
    maximumPendingWrites: FABRIC_PROTOCOL_LIMITS.maximumInFlightPerConnection,
  });
  let connectionInFlight = 0;
  let initializedCapability: string | undefined;
  let initializationComplete = false;

  const respond = async (
    id: string,
    operation: () => Promise<unknown>,
    onSuccess?: () => void,
  ): Promise<void> => {
    try {
      const result = await operation();
      await writer.write({ id, result: result === undefined ? null : result });
      onSuccess?.();
    } catch (error: unknown) {
      const name = error instanceof Error ? error.name : "Error";
      const message = error instanceof Error ? error.message : String(error);
      const code =
        typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
          ? error.code
          : "DAEMON_REQUEST_FAILED";
      await writer.write({ id, error: { name, code, message } }).catch(() => socket.destroy());
    } finally {
      connectionInFlight -= 1;
      releaseInFlight();
    }
  };

  const dispatch = async (request: DaemonRequest): Promise<unknown> => {
    if (request.method === "initialize") {
      if (initializedCapability !== undefined) {
        throw new DaemonProtocolError("DAEMON_ALREADY_INITIALIZED", "daemon connection is already initialized");
      }
      const initialize = daemonInitializeParams(request.params);
      if (initialize.protocolVersion !== FABRIC_PROTOCOL_VERSION) {
        throw new DaemonProtocolError(
          "DAEMON_PROTOCOL_UNSUPPORTED",
          `daemon protocol ${String(initialize.protocolVersion)} is unsupported`,
        );
      }
      initializedCapability = request.capability;
      return initializeResult;
    }
    if (!initializationComplete || initializedCapability === undefined) {
      throw new DaemonProtocolError("DAEMON_NOT_INITIALIZED", "initialize must succeed before daemon commands");
    }
    if (!secretEqual(request.capability, initializedCapability)) {
      throw new DaemonProtocolError(
        "DAEMON_CAPABILITY_MISMATCH",
        "daemon capability cannot change after initialization",
      );
    }
    if (secretEqual(request.capability, bootstrapCapability)) {
      switch (request.method) {
        case "bindCurrentMcpSeats":
          return fabric.bindCurrentMcpSeats(bindCurrentMcpSeatsInput(request.params));
        case "bootstrapMcpSeat": {
          const bootstrapInput = bootstrapMcpSeatInput(request.params, productRoot);
          const trustedInput = await withTrustedLocalSubject(bootstrapInput);
          const result = fabric.bootstrapTrustedCurrentMcpSeat(bootstrapInput, {
            canonicalRoot: trustedInput.canonicalRoot,
            trustRecordDigest: trustedInput.trustRecordDigest,
          });
          if (malformedBootstrapResultRemaining > 0) {
            malformedBootstrapResultRemaining -= 1;
            const malformed = { ...result } as Record<string, unknown>;
            delete malformed.custodyMutated;
            return malformed;
          }
          return result;
        }
        case "provisionLocalOperator":
          return fabric.provisionLocalOperator(await withTrustedLocalSubject(
            provisionLocalOperatorInput(request.params),
          ));
        case "openLocalOperatorConsoleCapability":
          return fabric.openLocalOperatorConsoleCapability(await withTrustedLocalSubject(
            openLocalOperatorConsoleCapabilityInput(request.params),
          ));
        case "issueLocalOperatorSessionCapability":
          return fabric.issueLocalOperatorSessionCapability(await withTrustedLocalSubject(
            issueLocalOperatorSessionCapabilityInput(request.params),
          ));
        case "openLocalOperatorConsoleSessionCapability":
          return fabric.openLocalOperatorConsoleSessionCapability(await withTrustedLocalSubject(
            issueLocalOperatorSessionCapabilityInput(request.params),
          ));
        case "openLocalOperatorConsoleTakeoverCapability":
          return fabric.openLocalOperatorConsoleTakeoverCapability(await withTrustedLocalSubject(
            openLocalOperatorTakeoverCapabilityInput(request.params),
          ));
        case "rotateLocalOperatorPrincipal":
          return fabric.rotateLocalOperatorPrincipal(await withTrustedLocalSubject(
            rotateLocalOperatorPrincipalInput(request.params),
          ));
        default:
          throw new DaemonProtocolError(
            "BOOTSTRAP_SCOPE_VIOLATION",
            "bootstrap capability is limited to private local bootstrap control methods",
          );
      }
    }
    return await dispatchClientMethod(fabric.connect(request.capability), request.method, request.params);
  };

  const reader = new BoundedNdjsonReader(socket, {
    maximumFrameBytes: FABRIC_PROTOCOL_LIMITS.maximumFrameBytes,
    idleTimeoutMs: FABRIC_PROTOCOL_LIMITS.idleTimeoutMs,
    onIdle: () => socket.destroy(),
    onError: (error) => {
      void writer.write({ id: "unknown", error: { name: error.name, code: error.code, message: error.message } })
        .finally(() => socket.destroy());
    },
    onFrame: (line) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(line);
      } catch (cause: unknown) {
        void writer.write({
          id: "unknown",
          error: {
            name: "DaemonProtocolError",
            code: "DAEMON_PROTOCOL_INVALID",
            message: cause instanceof Error ? cause.message : "malformed daemon JSON",
          },
        }).catch(() => socket.destroy());
        return;
      }
      const id = isRecord(parsed) && typeof parsed.id === "string" ? parsed.id : "unknown";
      if (!isDaemonRequest(parsed)) {
        void writer.write({
          id,
          error: { name: "DaemonProtocolError", code: "DAEMON_PROTOCOL_INVALID", message: "invalid daemon request" },
        }).catch(() => socket.destroy());
        return;
      }
      if (connectionInFlight >= FABRIC_PROTOCOL_LIMITS.maximumInFlightPerConnection) {
        void writer.write({
          id,
          error: {
            name: "DaemonProtocolError",
            code: "DAEMON_CONNECTION_OVERLOADED",
            message: `connection permits ${String(FABRIC_PROTOCOL_LIMITS.maximumInFlightPerConnection)} in-flight commands`,
          },
        }).catch(() => socket.destroy());
        return;
      }
      if (!servingAdmission.tryAdmit()) {
        void writer.write({
          id,
          error: {
            name: "DaemonProtocolError",
            code: "DAEMON_OVERLOADED",
            message: "daemon is draining and is not accepting commands",
          },
        }).catch(() => socket.destroy());
        return;
      }
      if (totalInFlight >= FABRIC_PROTOCOL_LIMITS.maximumTotalInFlight) {
        void writer.write({
          id,
          error: {
            name: "DaemonProtocolError",
            code: "DAEMON_OVERLOADED",
            message: `daemon permits ${String(FABRIC_PROTOCOL_LIMITS.maximumTotalInFlight)} in-flight commands`,
          },
        }).catch(() => socket.destroy());
        return;
      }
      connectionInFlight += 1;
      totalInFlight += 1;
      void respond(
        id,
        () => dispatch(parsed),
        parsed.method === "initialize" ? () => { initializationComplete = true; } : undefined,
      );
    },
  });
  socket.once("close", () => reader.close());
};

const servePublicConnection = (socket: Socket): void => {
  servePublicProtocolConnection(socket, {
    daemonVersion: FABRIC_DAEMON_VERSION,
    daemonInstanceGeneration,
    offeredFeatures: PROTOCOL_FEATURES,
    limits: PROTOCOL_LIMITS,
    verifyCredential: (credential) => {
      if (secretEqual(credential, bootstrapCapability)) {
        throw new DaemonProtocolError(
          "AUTHENTICATION_FAILED",
          "bootstrap discovery capability is not a public protocol principal",
        );
      }
      return fabric.verifyProtocolCredential(credential);
    },
    dispatch: async (context, operation, input) => {
      if (!servingAdmission.tryAdmit()) {
        throw Object.assign(new Error("daemon is draining and is not accepting commands"), {
          code: "OVERLOADED",
        });
      }
      if (totalInFlight >= FABRIC_PROTOCOL_LIMITS.maximumTotalInFlight) {
        throw Object.assign(new Error(
          `daemon permits ${String(FABRIC_PROTOCOL_LIMITS.maximumTotalInFlight)} in-flight commands`,
        ), { code: "OVERLOADED" });
      }
      totalInFlight += 1;
      try {
        return await fabric.dispatchPublicProtocol(context, operation, input);
      } finally {
        releaseInFlight();
      }
    },
    afterResponse: ({ context, operation, input, result }) => {
      if (
        operation === FABRIC_OPERATIONS.operatorDetach &&
        context.principal.kind === "operator" &&
        isRecord(result) &&
        result.detached === true
      ) {
        scheduleIdleStop();
        return;
      }
      if (operation !== FABRIC_OPERATIONS.operatorActionCommit || context.principal.kind !== "operator") return;
      const value: unknown = input;
      const principal = context.principal;
      const command = isRecord(value) ? value.command : undefined;
      if (
        !isRecord(command) || typeof command.commandId !== "string" ||
        !isRecord(result) || result.commandId !== command.commandId
      ) return;
      const matches = [...pendingDaemonStops.values()].filter((pending) =>
        pending.operatorId === principal.operatorId &&
        pending.projectId === principal.projectId &&
        pending.principalGeneration === principal.principalGeneration &&
        pending.commandId === command.commandId &&
        pending.operation === "daemon-stop");
      if (matches.length !== 1) return;
      completeQueuedDaemonStop(matches[0]!.custodyId);
    },
  });
};

const server = createServer((socket) => {
  if (sockets.size >= FABRIC_PROTOCOL_LIMITS.maximumConnections) {
    const writer = new BoundedNdjsonWriter(socket, {
      maximumFrameBytes: FABRIC_PROTOCOL_LIMITS.maximumFrameBytes,
      maximumPendingWrites: FABRIC_PROTOCOL_LIMITS.maximumInFlightPerConnection,
    });
    void writer.write({
      id: "connection",
      error: {
        name: "DaemonProtocolError",
        code: "DAEMON_CONNECTION_LIMIT",
        message: `daemon accepts at most ${String(FABRIC_PROTOCOL_LIMITS.maximumConnections)} connections`,
      },
    }).catch(() => undefined).finally(() => socket.destroy());
    return;
  }
  sockets.add(socket);
  socket.once("close", () => sockets.delete(socket));
  routeDaemonConnection(socket, {
    maximumFirstFrameBytes: Math.min(
      FABRIC_PROTOCOL_LIMITS.maximumFrameBytes,
      PROTOCOL_LIMITS.maximumFrameBytes,
    ),
    idleTimeoutMs: Math.min(
      FABRIC_PROTOCOL_LIMITS.idleTimeoutMs,
      PROTOCOL_LIMITS.idleTimeoutMs,
    ),
    onRoute: (protocol, routedSocket) => {
      if (protocol === "public-v1") servePublicConnection(routedSocket);
      else servePrivateControlConnection(routedSocket);
    },
  });
});

const openServingSocket = async (): Promise<void> =>
  await openRecoverableUnixListener(server, socketPath, {
    admissionFence: servingAdmission,
    onListening: async () => {
      bootstrapSocketOwned = true;
      const barrierPath = process.env.NODE_ENV === "test"
        ? process.env.AGENT_FABRIC_TEST_BOOTSTRAP_SOCKET_BARRIER_PATH
        : undefined;
      if (barrierPath === undefined) return;
      writeFileSync(barrierPath, `${String(process.pid)}\n`, { flag: "wx", mode: 0o600 });
      while (existsSync(barrierPath)) {
        await new Promise<void>((resolvePromise) => setTimeout(resolvePromise, 20));
      }
    },
  });

await openServingSocket();
fabric.markDaemonRuntimeRunning(daemonInstanceGeneration);

let shuttingDown = false;
let shutdownTransition: ElectionLockHandle | undefined;
const beginShutdownTransition = async (): Promise<void> => {
  if (shutdownTransition !== undefined) return;
  const acquired = await FLOCK_ELECTION_LOCK_PORT.tryAcquire(join(runtimeDirectory, "daemon-shutdown.lock"));
  if (acquired === undefined) throw new Error("daemon shutdown transition is already owned");
  shutdownTransition = acquired;
};
const markProductionTerminal = async (
  signal: NodeJS.Signals | null,
  state: "stopped" | "crashed" = "stopped",
  exitCode: number | null = null,
): Promise<void> => {
  if (bootstrapMode !== "production-election") return;
  const paths = privateDiscoveryPaths(runtimeDirectory);
  const expected = {
    actionId: bootstrapActionId as string,
    electionGeneration: bootstrapElectionGeneration,
    daemonInstanceGeneration,
    socketPath,
    pid: process.pid,
    bootstrapCapabilityHash: createHash("sha256").update(bootstrapCapability).digest("hex"),
  };
  const election = new BootstrapElection({ runtimeDirectory });
  const result = await election.withExclusiveLock(
    `terminal-${String(bootstrapActionId)}`,
    async () => await markPrivateDiscoveryTerminal({
      paths,
      expected,
      state,
      exitCode,
      signal,
    }),
  );
  if (result.role === "observer") {
    const discovery = await readPrivateDiscovery(paths, socketPath);
    if (
      discovery.status !== "terminal" ||
      discovery.owner.actionId !== expected.actionId ||
      discovery.owner.electionGeneration !== expected.electionGeneration ||
      discovery.owner.daemonInstanceGeneration !== expected.daemonInstanceGeneration
    ) {
      throw new Error("daemon could not confirm its terminal discovery generation");
    }
  }
};

const stopElection = new BootstrapElection({ runtimeDirectory });
const closeServingSocket = async (): Promise<void> =>
  await closeRecoverableUnixListener({
    server,
    sockets,
    waitForInFlight,
    admissionFence: servingAdmission,
  });

const finishProcess = async (input: {
  signal: NodeJS.Signals | null;
  state: "stopped" | "crashed";
  exitCode: number;
}): Promise<never> => {
  closeBackgroundWorkers();
  return await finalizeDaemonShutdown({
    requestedState: input.state,
    requestedExitCode: input.exitCode,
    closeFabric: async () => {
      try {
        await fabric.close();
      } finally {
        lifecycleReceiptAuthority?.close();
        lifecycleReceiptAuthority = undefined;
      }
    },
    removeSocket: async () => { rmSync(socketPath, { force: true }); },
    releaseLocks: async () => await releaseDaemonLocks(daemonLocks),
    markTerminal: async ({ state, exitCode }) => {
      await markProductionTerminal(input.signal, state, exitCode);
      await shutdownTransition?.release();
      shutdownTransition = undefined;
    },
    reportFailure: (failure) => {
      process.stderr.write(`${failure.message}\n`);
    },
    exit: (exitCode) => process.exit(exitCode),
  });
};

const attemptIdleStopWithServingRecovery = async (input: {
  actionId: string;
  signal: NodeJS.Signals | null;
}): Promise<IdleStopResult> => {
  let socketClosed = false;
  try {
    return await fabric.attemptIdleStop({
      actionId: input.actionId,
      daemonInstanceGeneration,
      election: stopElection,
      beforeElectionRelease: beginShutdownTransition,
      closeSocket: async () => {
        socketClosed = true;
        await closeServingSocket();
      },
      reopenSocket: async () => {
        await openServingSocket();
        socketClosed = false;
      },
    });
  } catch (error: unknown) {
    if (!socketClosed) throw error;
    shuttingDown = true;
    return await finishProcess({ signal: input.signal, state: "crashed", exitCode: 1 });
  }
};

const idleStopAttemptSocketPath = process.env.NODE_ENV === "test"
  ? process.env.AGENT_FABRIC_TEST_IDLE_STOP_ATTEMPT_SOCKET_PATH
  : undefined;
const holdTestOperatorDetachAttempt = async (): Promise<void> => {
  if (idleStopAttemptSocketPath === undefined) return;
  await new Promise<void>((resolvePromise, reject) => {
    const socket = createConnection(idleStopAttemptSocketPath);
    socket.once("connect", () => {
      socket.write("idle-stop-attempt:operator-detach\n", (error) => {
        if (error !== undefined) reject(error);
      });
    });
    socket.once("end", resolvePromise);
    socket.once("error", reject);
  });
};

const idleScheduler = new IdleShutdownScheduler({
  graceMs: 250,
  sweepMs: 30_000,
  attempt: async ({ actionId, reason }) => {
    if (reason === "operator-detach") await holdTestOperatorDetachAttempt();
    return await attemptIdleStopWithServingRecovery({
      actionId: `${actionId}:${String(daemonInstanceGeneration)}`,
      signal: null,
    });
  },
  onStopped: async () => {
    shuttingDown = true;
    await finishProcess({ signal: null, state: "stopped", exitCode: 0 });
  },
});
idleScheduler.start();
const resultDeadlineScheduler = new ResultDeadlineScheduler({
  intervalMs: 500,
  daemonInstanceGeneration,
  pass: (input) => { fabric.runResultDeadlinePass(input); },
});
resultDeadlineScheduler.start();
const notificationTimer = setInterval(() => {
  void fabric.runNativeNotificationPass().catch(() => undefined);
}, 1_000);
notificationTimer.unref();
void fabric.runNativeNotificationPass().catch(() => undefined);
const herdrPollInterval = herdrPresencePollInterval(herdrProcessConfiguration);
const herdrTimer = herdrPollInterval === null ? null : setInterval(() => {
  void fabric.runHerdrPresencePass().catch(() => undefined);
}, herdrPollInterval);
herdrTimer?.unref();
scheduleIdleStop = () => idleScheduler.schedule("operator-detach");
closeBackgroundWorkers = () => {
  idleScheduler.close();
  resultDeadlineScheduler.close();
  clearInterval(notificationTimer);
  if (herdrTimer !== null) clearInterval(herdrTimer);
};

completeQueuedDaemonStop = (custodyId: string): void => {
  const pending = pendingDaemonStops.get(custodyId);
  if (pending === undefined || shuttingDown) return;
  pendingDaemonStops.delete(custodyId);
  setImmediate(() => {
    if (shuttingDown) return;
    let socketClosed = false;
    void fabric.attemptDrainedStop({
      actionId: `operator-daemon-stop:${custodyId}`,
      token: pending.token,
      excludeOperatorEffectCustodyId: custodyId,
      election: stopElection,
      beforeElectionRelease: beginShutdownTransition,
      closeSocket: async () => {
        socketClosed = true;
        await closeServingSocket();
      },
      reopenSocket: async () => {
        await openServingSocket();
        socketClosed = false;
      },
    }).then(async (result) => {
      if (result.state !== "stopped") {
        fabric.recordDaemonStopCustodyResult({
          ...pending,
          daemonInstanceGeneration: pending.token.daemonInstanceGeneration,
          state: "rejected",
          result,
        });
        return;
      }
      fabric.recordDaemonStopCustodyResult({
        ...pending,
        daemonInstanceGeneration: pending.token.daemonInstanceGeneration,
        state: "stopped",
        result,
      });
      shuttingDown = true;
      await finishProcess({ signal: null, state: "stopped", exitCode: 0 });
    }).catch(async (error: unknown) => {
      let failure = error;
      try {
        fabric.recordDaemonStopCustodyResult({
          ...pending,
          daemonInstanceGeneration: pending.token.daemonInstanceGeneration,
          state: "failed",
          result: { message: error instanceof Error ? error.message : String(error) },
        });
      } catch (custodyError: unknown) {
        failure = new AggregateError([error, custodyError], "daemon stop and custody persistence both failed");
      }
      process.stderr.write(`operator daemon stop failed: ${failure instanceof Error ? failure.message : String(failure)}\n`);
      if (!socketClosed) return;
      shuttingDown = true;
      await finishProcess({ signal: null, state: "crashed", exitCode: 1 });
    });
  });
};

const signalStop = new GuardedIdleStopController(async (signal) => {
  const result = await attemptIdleStopWithServingRecovery({
    actionId: `signal-idle-stop:${signal}:${String(daemonInstanceGeneration)}`,
    signal,
  });
  if (result.state === "stopped") {
    shuttingDown = true;
    // A handled termination request is a graceful exit, not signal death.
    await finishProcess({ signal: null, state: "stopped", exitCode: 0 });
  }
  return result;
});
const shutdown = (signal: "SIGINT" | "SIGTERM"): void => {
  if (shuttingDown) return;
  void signalStop.request(signal).then((result) => {
    if (result.state === "busy") {
      process.stderr.write(`daemon remains active after ${signal}: ${result.reason}\n`);
    }
  }).catch((error: unknown) => {
    process.stderr.write(`guarded daemon stop failed: ${error instanceof Error ? error.message : String(error)}\n`);
  });
};

process.on("SIGTERM", () => shutdown("SIGTERM"));
process.on("SIGINT", () => shutdown("SIGINT"));
process.stdout.write(`${JSON.stringify({ ready: true })}\n`);
