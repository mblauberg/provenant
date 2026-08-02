import type { Socket } from "node:net";

import { BoundedNdjsonWriter } from "../transport/bounded-ndjson.js";

export type DaemonConnectionProtocol = "private-control-v1" | "public-v1";

export type DaemonConnectionRouterOptions = Readonly<{
  maximumFirstFrameBytes: number;
  idleTimeoutMs: number;
  onRoute(protocol: DaemonConnectionProtocol, socket: Socket): void;
}>;

function positiveInteger(value: number, label: string): number {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new TypeError(`${label} must be a positive safe integer`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function classifyFirstFrame(value: unknown): DaemonConnectionProtocol | "ambiguous" {
  if (!isRecord(value)) return "private-control-v1";
  const publicMarkers = Object.hasOwn(value, "operation") || Object.hasOwn(value, "input");
  const privateControlMarkers = Object.hasOwn(value, "method") ||
    Object.hasOwn(value, "params") ||
    Object.hasOwn(value, "capability");
  if (publicMarkers && privateControlMarkers) return "ambiguous";
  return publicMarkers ? "public-v1" : "private-control-v1";
}

/**
 * Selects the wire protocol from one bounded NDJSON frame, then replays every
 * byte to the selected protocol reader. The router never authenticates or
 * dispatches a request itself.
 */
export function routeDaemonConnection(
  socket: Socket,
  options: DaemonConnectionRouterOptions,
): void {
  const maximumFirstFrameBytes = positiveInteger(
    options.maximumFirstFrameBytes,
    "maximumFirstFrameBytes",
  );
  const idleTimeoutMs = positiveInteger(options.idleTimeoutMs, "idleTimeoutMs");
  const chunks: Buffer[] = [];
  let bufferedBytes = 0;
  let routed = false;
  let idleTimer: NodeJS.Timeout | undefined;

  const cleanup = (): void => {
    if (idleTimer !== undefined) clearTimeout(idleTimer);
    socket.off("data", onData);
    socket.off("end", onIncomplete);
    socket.off("close", onIncomplete);
    socket.off("error", onSocketError);
  };

  const reject = (code: string, message: string): void => {
    if (routed) return;
    routed = true;
    cleanup();
    socket.once("error", () => undefined);
    const writer = new BoundedNdjsonWriter(socket, {
      maximumFrameBytes: Math.max(maximumFirstFrameBytes, 4_096),
      maximumPendingWrites: 1,
    });
    // Rejected peers may keep their write side open or leave unread inbound
    // data behind. A graceful end can then retain the daemon descriptor.
    void writer.write({
      id: "connection",
      error: { name: "DaemonProtocolError", code, message },
    }).catch(() => undefined).finally(() => socket.destroy());
  };

  const route = (protocol: DaemonConnectionProtocol): void => {
    if (routed) return;
    routed = true;
    cleanup();
    socket.pause();
    const replay = Buffer.concat(chunks, bufferedBytes);
    if (replay.length > 0) socket.unshift(replay);
    try {
      options.onRoute(protocol, socket);
      socket.resume();
    } catch (error: unknown) {
      socket.once("error", () => undefined);
      socket.destroy(error instanceof Error ? error : new Error(String(error)));
    }
  };

  const onIncomplete = (): void => {
    if (!routed) {
      routed = true;
      cleanup();
      socket.destroy();
    }
  };
  const onSocketError = (): void => {
    if (!routed) {
      routed = true;
      cleanup();
    }
  };
  const onData = (chunk: Buffer | string): void => {
    if (routed) return;
    resetIdleTimer();
    const bytes = typeof chunk === "string" ? Buffer.from(chunk) : chunk;
    const newline = bytes.indexOf(0x0a);
    if (newline === -1) {
      if (bufferedBytes + bytes.length > maximumFirstFrameBytes) {
        reject(
          "NDJSON_FRAME_TOO_LARGE",
          `daemon first frame exceeds ${String(maximumFirstFrameBytes)} bytes`,
        );
        return;
      }
      chunks.push(bytes);
      bufferedBytes += bytes.length;
      return;
    }
    if (bufferedBytes + newline > maximumFirstFrameBytes) {
      reject(
        "NDJSON_FRAME_TOO_LARGE",
        `daemon first frame exceeds ${String(maximumFirstFrameBytes)} bytes`,
      );
      return;
    }
    chunks.push(bytes);
    const firstFrameParts = chunks.slice(0, -1);
    firstFrameParts.push(bytes.subarray(0, newline));
    let firstFrameBytes = Buffer.concat(firstFrameParts, bufferedBytes + newline);
    bufferedBytes += bytes.length;
    if (firstFrameBytes.at(-1) === 0x0d) firstFrameBytes = firstFrameBytes.subarray(0, -1);
    let frame: string;
    try {
      frame = new TextDecoder("utf-8", { fatal: true }).decode(firstFrameBytes);
    } catch {
      reject("NDJSON_INVALID_UTF8", "daemon first frame is not valid UTF-8");
      return;
    }
    let decoded: unknown;
    try {
      decoded = JSON.parse(frame);
    } catch {
      route("private-control-v1");
      return;
    }
    const protocol = classifyFirstFrame(decoded);
    if (protocol === "ambiguous") {
      reject(
        "DAEMON_PROTOCOL_AMBIGUOUS",
        "daemon first frame mixes private-control and public protocol fields",
      );
      return;
    }
    route(protocol);
  };

  const resetIdleTimer = (): void => {
    if (idleTimer !== undefined) clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      if (!routed) {
        routed = true;
        cleanup();
        socket.destroy();
      }
    }, idleTimeoutMs);
    idleTimer.unref();
  };
  resetIdleTimer();
  socket.on("data", onData);
  socket.once("end", onIncomplete);
  socket.once("close", onIncomplete);
  socket.once("error", onSocketError);
}
