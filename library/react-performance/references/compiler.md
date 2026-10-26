# React Compiler

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
