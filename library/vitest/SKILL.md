---
name: vitest
description: "Use for Vitest tests, mocking, timers, coverage, browser mode and migrations. Not for choosing a test strategy (test-design) or React rendering performance (react-performance)."
---

# Vitest

Check the installed Vitest and Vite versions and project configuration before changing tests. Select tests by public behaviour with test-design; run type assertions in the project's typecheck mode as well as ordinary tests.

Migration defaults can silently alter mock history, fake clocks, browser mocks, coverage selection and reporter destinations. Consult the dated notes for the installed major, then verify an affected test or output path. Await asynchronous assertions: an unobserved promise is not evidence of success.

Mock at external boundaries and restore state between tests. Check the actual browser environment for browser-specific module behaviour; a Node result cannot prove it. Report weakened assertions.

Read the [dated release notes](references/releases-2026.md) for version-sensitive details.
