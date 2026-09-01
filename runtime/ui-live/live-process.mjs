export const LIVE_SERVER_STOP_TIMEOUT_MS = 12_000;

export function isProcessAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM means the process exists but is not inspectable by this user.
    return error?.code === 'EPERM';
  }
}

export async function waitForProcessExit(
  pid,
  { timeoutMs = LIVE_SERVER_STOP_TIMEOUT_MS, pollMs = 25 } = {},
) {
  const deadline = Date.now() + timeoutMs;
  while (isProcessAlive(pid) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }
  return !isProcessAlive(pid);
}
