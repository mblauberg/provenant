import { defineConfig } from 'vitest/config';

// These suites fork real daemons, sockets and databases, so several suites
// running at once oversubscribe the machine and every wall-clock bound fails.
// Parallel lanes set this to keep one suite inside its share of the cores.
const maxWorkers = process.env.AGENT_FABRIC_TEST_MAX_WORKERS;

export default defineConfig({
  test: {
    environment: 'node',
    include: ['tests/**/*.test.ts'],
    testTimeout: 15_000,
    hookTimeout: 15_000,
    pool: 'forks',
    sequence: { concurrent: false },
    ...(maxWorkers === undefined ? {} : { maxWorkers: Number(maxWorkers) }),
  },
});
