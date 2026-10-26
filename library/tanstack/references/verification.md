# Query and table verification

Table tests render the component and assert on rows, header sort state and
selection, not on `table` internals. Tests get a fresh `QueryClient` with
`retry: false` and go through the public hook or component.

`@tanstack/eslint-plugin-query` provides `exhaustive-deps`,
`stable-query-client`, `no-rest-destructuring`, `no-unstable-deps` and
`prefer-query-options`. Suggest it where it is missing; do not add it unasked.
