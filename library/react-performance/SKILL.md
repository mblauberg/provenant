---
name: react-performance
description: "Use for slow React 19 UIs: render profiling, input lag, React Compiler memoisation and lint errors, transitions, bundle size. Not for Query caching (tanstack) or visual design (ui-ux-design)."
---

# React performance

This skill targets client-rendered React 19 with React Compiler 1.x on Vite.
Before starting, confirm the React version and whether the compiler is on, and
for which files. If the app renders on a server (Server Components, SSR,
Next.js), this skill does not cover that layer; follow the framework's own
docs.

## Measure first

- **Profile a production profiling build** (`vite build`, then `vite preview`).
  A plain production build strips React's profiling instrumentation, so alias
  `react-dom/client` to `react-dom/profiling` in the Vite config for the
  profiling run. Dev mode doubles renders under Strict Mode and loads modules
  unbundled, so its numbers mislead.
- **Use the right tools:** React DevTools Profiler shows which component
  rendered and why. Chrome's Performance panel shows React's Scheduler and
  Components tracks from 19.2. INP and LCP come from field data or `web-vitals`.
  Run a bundle visualiser on the build output.
- **Classify the bottleneck before editing:** network waterfall, JavaScript
  size, render work, layout and paint, or memory. Re-measure under the same
  conditions, and keep a change only if the number moved.

## React Compiler

- **New code:** write plain components and let the compiler memoise components
  and hooks. Use `useMemo` or `useCallback` only as an escape hatch, mainly to
  hold an effect dependency steady.
- **Existing code:** leave manual memoisation alone. Removing it can change the
  compiled output, so do that only as a separate, tested change.
- **Limits:** the compiler does not memoise ordinary functions called during
  render, and it shares no results between components. A costly calculation
  used in several places needs a cache outside React, or lifting.
- **Compiler lint errors** (`eslint-plugin-react-hooks` v6 or later) mean the
  code breaks React's rules or blocks compilation, so fix the code. Typical
  causes:
  - touching `ref.current` during render;
  - mutating props, state or values after render;
  - a synchronous `setState` inside an effect;
  - impure render (`Date.now()`, `Math.random()`);
  - a component defined inside another component.
- **Escape hatches:** `incompatible-library` means a library's API cannot be
  memoised safely, so handle that value deliberately. Use `'use no memo'` only
  as a short-lived, commented debugging step.
- **Coverage:** compiling more files is not a fix in itself. Follow the
  project's opt-in rule and the evidence it asks for.

## Fixes, roughly by payoff

1. **Waits.** Start independent requests together. Prefetch at route or intent
   boundaries (loaders: see `tanstack`). Break parent-then-child fetch
   chains.
2. **Bytes.** Split routes with `lazy()`, load rare heavy libraries with
   dynamic `import()`, and avoid barrel imports the bundler cannot prune.
3. **Render work.** Derive values in render rather than syncing through
   effects. Push state down or split contexts. Keep pointer, scroll and timer
   values in refs or a `useSyncExternalStore` store. Virtualise long lists.
4. **Responsiveness.** Wrap expensive, non-urgent updates in `startTransition`.
   `useDeferredValue` helps an expensive view of fast-changing input only when
   the slow child is memoised, which the compiler does for you in compiled
   files. From 19.3, transitions render independently, so a slow one no longer
   blocks the others.
5. **`<Activity mode="hidden">`.** It keeps a hidden subtree's state and can
   pre-render it at low priority. It also keeps DOM and memory alive and tears
   down effects while hidden. Use it where kept state is the point, such as tabs
   and back navigation, not as a cache.
6. **`useEffectEvent`.** Use it when an effect must read the latest props
   without resubscribing. The function it returns is not a dependency.

## Reject

Reject a "win" that shifts the cost to a later interaction, weakens freshness
or authorisation, or appears only on synthetic data. Report before and after
numbers with their conditions, and anything left unmeasured.

## Sources

Checked 22 October 2026: react.dev blog (React 19.2, React Compiler 1.0,
React 19.3), react.dev/learn/react-compiler,
react.dev/reference/eslint-plugin-react-hooks, vite.dev/blog (Vite 8.1).
