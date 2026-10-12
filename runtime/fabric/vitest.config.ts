import { defineConfig } from "vitest/config";

export default defineConfig({ test: { setupFiles: ["./tests/hermetic-tmpdir.ts"] } });
