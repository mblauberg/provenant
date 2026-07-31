import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/system/split-root-contract.system.spec.ts"],
    testTimeout: 15_000,
    hookTimeout: 15_000,
    pool: "forks",
    sequence: { concurrent: false },
  },
});
