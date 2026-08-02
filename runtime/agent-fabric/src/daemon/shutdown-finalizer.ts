export type ShutdownTerminalState = "stopped" | "crashed";

export type ShutdownFailure = Readonly<{
  stage: "close-fabric" | "remove-socket" | "release-locks" | "mark-terminal";
  cause: unknown;
}>;

export async function closeFabricWithLifecycleReceiptAuthority(input: Readonly<{
  closeFabric(): Promise<void>;
  closeAuthority(): void;
}>): Promise<void> {
  try {
    await input.closeFabric();
  } finally {
    input.closeAuthority();
  }
}

export async function finalizeDaemonShutdown(input: Readonly<{
  requestedState: ShutdownTerminalState;
  requestedExitCode: number;
  closeFabric(): Promise<void>;
  removeSocket(): Promise<void>;
  releaseLocks(): Promise<void>;
  markTerminal(input: Readonly<{ state: ShutdownTerminalState; exitCode: number }>): Promise<void>;
  reportFailure(failure: AggregateError): void;
  exit(code: number): never;
}>): Promise<never> {
  const failures: ShutdownFailure[] = [];
  let state = input.requestedState;
  let exitCode = input.requestedExitCode;

  const attempt = async (
    stage: ShutdownFailure["stage"],
    effect: () => Promise<void>,
  ): Promise<void> => {
    try {
      await effect();
    } catch (cause: unknown) {
      failures.push({ stage, cause });
      state = "crashed";
      exitCode = 1;
    }
  };

  await attempt("close-fabric", input.closeFabric);
  await attempt("remove-socket", input.removeSocket);
  await attempt("release-locks", input.releaseLocks);
  await attempt("mark-terminal", async () => await input.markTerminal({ state, exitCode }));

  if (failures.length > 0) {
    try {
      input.reportFailure(new AggregateError(
        failures.map(({ cause }) => cause),
        `daemon shutdown failed during ${failures.map(({ stage }) => stage).join(", ")}`,
      ));
    } catch {
      // Diagnostics must never defeat the final process exit.
    }
  }

  input.exit(exitCode);
}
