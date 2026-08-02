import { connect as connectSocket, type Socket } from "node:net";

import { assertRequiredResultShapeFeatures } from "@local/agent-fabric-protocol";
import { v7 as uuidv7 } from "uuid";

import {
  FABRIC_DAEMON_VERSION,
  FABRIC_PROTOCOL_VERSION,
  isDaemonInitializeResult,
  isDaemonResponse,
  type DaemonInitializeResult,
  type DaemonResponse,
} from "./daemon-rpc-contract.js";
import { BoundedNdjsonReader, BoundedNdjsonWriter, FABRIC_PROTOCOL_LIMITS } from "./bounded-ndjson.js";

type Pending = {
  resolve(value: unknown): void;
  reject(error: Error): void;
  timeout: NodeJS.Timeout;
};

export type TimedNdjsonTransportOptions = {
  socketPath: string;
  capability: string;
  requiredCapabilities?: readonly string[];
  connectTimeoutMs?: number;
  requestTimeoutMs?: number;
};

export type TimedNdjsonTransportDependencies = {
  connect(path: string): Socket;
};

export class FabricRemoteError extends Error {
  readonly code: string;
  readonly preserved: boolean;

  constructor(code: string, message: string, options?: Readonly<{ preserved?: boolean; cause?: unknown }>) {
    super(message, options?.cause === undefined ? undefined : { cause: options.cause });
    this.name = "FabricError";
    this.code = code;
    this.preserved = options?.preserved ?? false;
  }
}

const defaultDependencies: TimedNdjsonTransportDependencies = { connect: connectSocket };

function positiveTimeout(value: number | undefined, fallback: number, label: string): number {
  const timeout = value ?? fallback;
  if (!Number.isFinite(timeout) || timeout <= 0) {
    throw new TypeError(`${label} must be a positive number`);
  }
  return timeout;
}

export class TimedNdjsonTransport {
  readonly #socket: Socket;
  readonly #reader: BoundedNdjsonReader;
  readonly #writer: BoundedNdjsonWriter;
  readonly #capability: string;
  readonly #requestTimeoutMs: number;
  readonly #pending = new Map<string, Pending>();
  #initializeResult: DaemonInitializeResult | undefined;
  #terminalError: Error | undefined;
  #closed = false;

  private constructor(socket: Socket, capability: string, requestTimeoutMs: number) {
    this.#socket = socket;
    this.#capability = capability;
    this.#requestTimeoutMs = requestTimeoutMs;
    this.#writer = new BoundedNdjsonWriter(socket, {
      maximumFrameBytes: FABRIC_PROTOCOL_LIMITS.maximumFrameBytes,
      maximumPendingWrites: FABRIC_PROTOCOL_LIMITS.maximumClientPending,
    });
    this.#reader = new BoundedNdjsonReader(socket, {
      maximumFrameBytes: FABRIC_PROTOCOL_LIMITS.maximumFrameBytes,
      onFrame: (line) => this.#receive(line),
      onError: (error) => {
        this.#terminalError = new FabricRemoteError("DAEMON_PROTOCOL_INVALID", error.message);
        socket.destroy();
      },
    });
    socket.on("error", () => this.#disconnect());
    socket.on("close", () => this.#disconnect());
  }

  static async connect(
    options: TimedNdjsonTransportOptions,
    dependencies: TimedNdjsonTransportDependencies = defaultDependencies,
  ): Promise<TimedNdjsonTransport> {
    const connectTimeoutMs = positiveTimeout(options.connectTimeoutMs, 5_000, "connect timeout");
    const requestTimeoutMs = positiveTimeout(options.requestTimeoutMs, 30_000, "request timeout");
    const socket = dependencies.connect(options.socketPath);
    await new Promise<void>((resolve, reject) => {
      const cleanup = (): void => {
        clearTimeout(timeout);
        socket.off("connect", onConnect);
        socket.off("error", onError);
      };
      const onConnect = (): void => {
        cleanup();
        resolve();
      };
      const onError = (error: Error): void => {
        cleanup();
        socket.destroy();
        reject(error);
      };
      const timeout = setTimeout(() => {
        cleanup();
        socket.destroy();
        reject(new FabricRemoteError("DAEMON_CONNECT_TIMEOUT", "daemon connection timed out"));
      }, connectTimeoutMs);
      socket.once("connect", onConnect);
      socket.once("error", onError);
    });
    const transport = new TimedNdjsonTransport(socket, options.capability, requestTimeoutMs);
    try {
      const initialized = await transport.#callInternal("initialize", {
        protocolVersion: FABRIC_PROTOCOL_VERSION,
        client: { name: "agent-fabric", version: FABRIC_DAEMON_VERSION },
        capabilities: ["rpc", ...(options.requiredCapabilities ?? [])],
      });
      if (!isDaemonInitializeResult(initialized)) {
        throw new FabricRemoteError("DAEMON_PROTOCOL_MISMATCH", "daemon initialize response is incompatible");
      }
      assertRequiredResultShapeFeatures(
        initialized.capabilities,
        options.requiredCapabilities ?? [],
      );
      transport.#initializeResult = initialized;
      return transport;
    } catch (error: unknown) {
      transport.#terminalError = error instanceof Error ? error : new Error(String(error));
      socket.destroy();
      throw error;
    }
  }

  get initializeResult(): DaemonInitializeResult {
    if (this.#initializeResult === undefined) {
      throw new FabricRemoteError("DAEMON_NOT_INITIALIZED", "daemon transport is not initialized");
    }
    return this.#initializeResult;
  }

  get closed(): boolean {
    return this.#closed;
  }

  #disconnect(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#reader.close();
    this.#rejectPending(this.#terminalError ?? new FabricRemoteError("DAEMON_DISCONNECTED", "daemon connection closed"));
  }

  #rejectPending(error: Error): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.#pending.clear();
  }

  #receive(line: string): void {
    let response: DaemonResponse;
    try {
      const value: unknown = JSON.parse(line);
      if (!isDaemonResponse(value)) throw new Error("invalid daemon response");
      response = value;
    } catch (error: unknown) {
      this.#terminalError = new FabricRemoteError(
        "DAEMON_PROTOCOL_INVALID",
        error instanceof Error ? error.message : String(error),
      );
      this.#rejectPending(this.#terminalError);
      this.#socket.destroy();
      return;
    }
    if (response.id === "connection" && "error" in response) {
      this.#terminalError = new FabricRemoteError(response.error.code, response.error.message);
      this.#disconnect();
      this.#socket.destroy();
      return;
    }
    const pending = this.#pending.get(response.id);
    if (pending === undefined) return;
    this.#pending.delete(response.id);
    clearTimeout(pending.timeout);
    if ("error" in response) {
      pending.reject(new FabricRemoteError(response.error.code, response.error.message));
    } else {
      pending.resolve(response.result);
    }
  }

  async call(method: string, params: Record<string, unknown>): Promise<unknown> {
    if (this.#initializeResult === undefined) {
      throw new FabricRemoteError("DAEMON_NOT_INITIALIZED", "daemon transport is not initialized");
    }
    if (method === "initialize") {
      throw new FabricRemoteError("DAEMON_ALREADY_INITIALIZED", "daemon transport is already initialized");
    }
    return await this.#callInternal(method, params);
  }

  async #callInternal(method: string, params: Record<string, unknown>): Promise<unknown> {
    if (this.#closed) {
      throw new FabricRemoteError("DAEMON_DISCONNECTED", "daemon connection is closed");
    }
    if (this.#pending.size >= FABRIC_PROTOCOL_LIMITS.maximumClientPending) {
      throw new FabricRemoteError(
        "DAEMON_CLIENT_OVERLOADED",
        `daemon client has ${String(FABRIC_PROTOCOL_LIMITS.maximumClientPending)} pending calls`,
      );
    }
    const id = uuidv7();
    const result = new Promise<unknown>((resolve, reject) => {
      const timeout = setTimeout(() => {
        if (!this.#pending.delete(id)) return;
        reject(new FabricRemoteError("DAEMON_REQUEST_TIMEOUT", `daemon request timed out: ${method}`));
      }, this.#requestTimeoutMs);
      this.#pending.set(id, { resolve, reject, timeout });
    });
    try {
      await this.#writer.write({ id, capability: this.#capability, method, params });
    } catch (error: unknown) {
      const pending = this.#pending.get(id);
      if (pending !== undefined) {
        this.#pending.delete(id);
        clearTimeout(pending.timeout);
        pending.reject(error instanceof Error ? error : new Error(String(error)));
      }
    }
    return result;
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    const closed = new Promise<void>((resolve) => this.#socket.once("close", resolve));
    this.#socket.end();
    await closed;
  }
}
