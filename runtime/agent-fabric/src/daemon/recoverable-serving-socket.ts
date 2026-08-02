import { chmodSync } from "node:fs";
import type { Server, Socket } from "node:net";

import {
  DAEMON_SHUTDOWN_CLOSE_TIMEOUT,
  DAEMON_SHUTDOWN_DRAIN_TIMEOUT,
  waitWithShutdownDeadline,
} from "../lifecycle/shutdown-deadline.js";

export class RecoverableServingAdmissionFence {
  #accepting = true;

  close(): void {
    this.#accepting = false;
  }

  reopen(): void {
    this.#accepting = true;
  }

  tryAdmit(): boolean {
    return this.#accepting;
  }
}

export async function openRecoverableUnixListener(
  server: Server,
  socketPath: string,
  options: {
    setMode?(path: string, mode: number): void;
    admissionFence?: RecoverableServingAdmissionFence;
    onListening?(): Promise<void> | void;
  } = {},
): Promise<void> {
  if (server.listening) {
    options.admissionFence?.reopen();
    return;
  }
  const setMode = options.setMode ?? chmodSync;
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, async () => {
      server.off("error", reject);
      try {
        setMode(socketPath, 0o600);
        options.admissionFence?.reopen();
        await options.onListening?.();
        resolve();
      } catch (error: unknown) {
        server.close((closeError) => {
          reject(closeError === undefined
            ? error
            : new AggregateError([error, closeError], "socket mode hardening and listener close both failed"));
        });
      }
    });
  });
}

export async function closeRecoverableUnixListener(options: {
  server: Server;
  sockets: Iterable<Socket>;
  waitForInFlight(): Promise<void>;
  closeTimeoutMs: number;
  drainTimeoutMs: number;
  admissionFence?: RecoverableServingAdmissionFence;
}): Promise<void> {
  options.admissionFence?.close();
  let drainFailed = false;
  let drainError: unknown;
  let teardownFailed = false;
  let teardownError: unknown;
  try {
    await waitWithShutdownDeadline(
      options.waitForInFlight(),
      options.drainTimeoutMs,
      DAEMON_SHUTDOWN_DRAIN_TIMEOUT,
      `in-flight operations did not drain within ${String(options.drainTimeoutMs)}ms`,
    );
  } catch (error: unknown) {
    drainFailed = true;
    drainError = error;
  } finally {
    const closed = options.server.listening
      ? new Promise<void>((resolve, reject) => options.server.close((error) => {
          if (error === undefined) resolve();
          else reject(error);
        }))
      : Promise.resolve();
    for (const socket of options.sockets) socket.destroy();
    try {
      await waitWithShutdownDeadline(
        closed,
        options.closeTimeoutMs,
        DAEMON_SHUTDOWN_CLOSE_TIMEOUT,
        `serving socket did not close within ${String(options.closeTimeoutMs)}ms`,
      );
    } catch (error: unknown) {
      teardownFailed = true;
      teardownError = error;
    }
  }
  if (drainFailed) throw drainError;
  if (teardownFailed) throw teardownError;
}
