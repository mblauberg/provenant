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
