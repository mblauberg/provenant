import { chmodSync } from "node:fs";
import type { Server, Socket } from "node:net";

export const DAEMON_SHUTDOWN_DRAIN_TIMEOUT = "DAEMON_SHUTDOWN_DRAIN_TIMEOUT";
export const DAEMON_SHUTDOWN_CLOSE_TIMEOUT = "DAEMON_SHUTDOWN_CLOSE_TIMEOUT";
export const DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT = "DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT";

class DaemonShutdownTimeoutError extends Error {
  readonly code: DaemonShutdownTimeoutCode;

  constructor(code: DaemonShutdownTimeoutError["code"], message: string) {
    super(message);
    this.name = "DaemonShutdownTimeoutError";
    this.code = code;
  }
}

export type DaemonShutdownTimeoutCode =
  | typeof DAEMON_SHUTDOWN_DRAIN_TIMEOUT
  | typeof DAEMON_SHUTDOWN_CLOSE_TIMEOUT
  | typeof DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT;

export async function waitWithShutdownDeadline(
  pending: Promise<void>,
  timeoutMs: number,
  code: DaemonShutdownTimeoutCode,
  message: string,
): Promise<void> {
  let timer: NodeJS.Timeout | undefined;
  try {
    await Promise.race([
      pending,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new DaemonShutdownTimeoutError(code, message)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

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
  await waitWithShutdownDeadline(
    options.waitForInFlight(),
    options.drainTimeoutMs,
    DAEMON_SHUTDOWN_DRAIN_TIMEOUT,
    `in-flight operations did not drain within ${String(options.drainTimeoutMs)}ms`,
  );
  const closed = options.server.listening
    ? new Promise<void>((resolve, reject) => options.server.close((error) => {
        if (error === undefined) resolve();
        else reject(error);
      }))
    : Promise.resolve();
  for (const socket of options.sockets) socket.destroy();
  await waitWithShutdownDeadline(
    closed,
    options.closeTimeoutMs,
    DAEMON_SHUTDOWN_CLOSE_TIMEOUT,
    `serving socket did not close within ${String(options.closeTimeoutMs)}ms`,
  );
}
