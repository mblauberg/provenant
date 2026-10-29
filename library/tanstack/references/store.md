# TanStack Store selectors

Version-sensitive guidance last checked 22 October 2026; verify against the installed package.

Checked against `@tanstack/react-store` and `@tanstack/store` 0.11.2 source and
declarations.

- Read with `useSelector(store, selector, { compare })`. `useStore` is a
  deprecated alias with a bare `compare` third argument.
- The default `compare` is `===`. A selector that returns a new object or array
  each call re-renders on every store change. Select a primitive, several
  `useSelector` calls, or pass `{ compare: shallow }` (`shallow` is exported
  from `@tanstack/store`).
- Omitting the selector subscribes to the whole value.
- `store.setState` takes an updater function, `(prev) => next`, not a value.
  Group writes that belong together in `actions` (the second argument of
  `createStore`) and call `store.actions.x()`.
- `createStore(() => derived)` makes a read-only derived store. It has no
  `setState`.
- `_useStore` is marked experimental in the docs. Avoid it in app code.
- Server data belongs in Query, not Store. Keep Store for client state that
  several components share.
