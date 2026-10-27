# TanStack Form

Checked against `@tanstack/react-form` 1.33.5 declarations and the Form docs.
No Intent skill shipped for it in that version.

- Form state does not re-render the component. Read it with
  `useSelector(form.store, (s) => s.values.x)` for logic, or
  `<form.Subscribe selector={...}>` for UI. Always pass a selector; omitting it
  re-renders on every change.
- `useStore` is still exported but deprecated; it is an alias of
  `useSelector` from `@tanstack/react-store`. Use `useSelector` in new code.
- Inside `form.Field`, read `field.state.value` and `field.state.meta`
  directly. Components that use `useFieldContext` sit outside that render, so
  read meta with `useSelector(field.store, ...)`.
- Array fields (`mode="array"`) do not re-render when an item's value changes
  (per the latest docs; not tested in 1.33.5). Render each item's value in its
  own `form.Field`.
- Async validators accept `onChangeAsyncDebounceMs`. Set it for server checks
  so they do not run per keystroke.
- Schema validation uses Standard Schema libraries (Zod 3.24 or newer, among
  others).
- For side effects on a change, use the listeners API, not `useEffect` on
  form values.
