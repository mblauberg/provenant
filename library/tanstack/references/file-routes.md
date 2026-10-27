# Router file routes and router-plugin

Checked against `@tanstack/router-plugin` 1.168.48 (its shipped `router-plugin`
skill) and the Router installation docs. Router's own skills (`router-core`
and children such as `search-params`, `data-loading`, `code-splitting`) ship
with the installed packages; load them with Intent.

- Pass `tanstackRouter({ target: 'react', autoCodeSplitting: true })` before
  `react()` in the Vite plugins. Wrong order fails silently.
- `routeTree.gen.ts` is generated. Never edit it; exclude it from linting and
  formatting.
- Files prefixed `-` in the routes directory are ignored by the router
  (`routeFileIgnorePrefix` default). Use that for colocated helpers.
- With `autoCodeSplitting`, components are split and loaders stay in the main
  bundle. Do not `export` component functions from route files; that bypasses
  splitting. In split files use `getRouteApi('/path')`, not an import of
  `Route`. Use `.lazy.tsx` only for manual splitting.
- Declare `Register` with the router type for typed `Link` and hooks, and
  avoid casts.
- Query integration: see the router integration reference in this skill.
