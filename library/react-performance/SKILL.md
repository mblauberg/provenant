---
name: react-performance
description: "Use for React render profiling, input latency, React Compiler and bundle performance. Not for TanStack cache correctness (tanstack) or visual design (ui-ux-design)."
---

# React performance

Confirm installed React, compiler and framework versions and whether affected files are compiled. For regressions use diagnose; implement owns approved changes.

Measure the user action before editing. Record the build and profiling commands, data set, browser, device or throttling, metric and result in a run-owned artefact. Classify network, JavaScript, rendering, layout or memory cost. Repeat the same measurement after the change and retain both numbers. Field responsiveness and a lab trace answer different questions; identify which you have.

Use a production profiling build when examining React renders: a normal production build may omit profiling instrumentation, while development Strict Mode changes the work observed. Check the framework's build instructions and use browser profiling and React DevTools as appropriate.

With React Compiler enabled, check diagnostics and compiled coverage before adding manual memoisation. Existing memoisation can have semantic dependencies, so remove it only with evidence. Avoid moving work to a later interaction or weakening data freshness to improve one measurement. Report unmeasured effects. Use playwright for browser capture and tanstack for query behaviour.
