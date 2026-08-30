export const LIVE_SERVER_STARTUP_TIMEOUT_MS = 10_000;

export function observeStartup({
  startedAt,
  observedAt = Date.now(),
  baselineMs = LIVE_SERVER_STARTUP_TIMEOUT_MS,
}) {
  return {
    baselineMs,
    elapsedMs: Math.max(0, observedAt - startedAt),
  };
}

export function classifyStartupOutcome({ ready = false, exit = null, observation }) {
  if (ready) return { status: 'success', observation };
  if (exit) return { status: 'refused', observation, exit };
  if (!observation
    || !Number.isFinite(observation.baselineMs)
    || !Number.isFinite(observation.elapsedMs)
    || observation.baselineMs < 0
    || observation.elapsedMs < observation.baselineMs) {
    return { status: 'unclassified', observation };
  }
  return {
    status: 'timeout',
    classification: 'environmental_timeout',
    observation,
  };
}
