# TanStack Hotkeys

`@tanstack/hotkeys` ships its own Intent skills (`hotkeys-core`, `app-hotkeys`,
and `react-hotkeys` in the adapter). Load them rather than relying on this
page: `intent load @tanstack/hotkeys#hotkeys-core` and
`@tanstack/react-hotkeys#react-hotkeys`.

Pitfalls from `hotkeys-core` as shipped in the adapter 0.17.0 tree:

- Register shortcuts with `useHotkey` where the action lives. Do not add
  `keydown` listeners, `isMac` checks or a shortcut registry.
- A disabled hotkey does not block the browser: `Mod+S` still opens Save Page.
  Use `preventDefault: 'always'` where that matters.
- Dialogs use an `exclusive` scope, named per instance.
- Import from the adapter only. A second copy of `@tanstack/hotkeys` splits
  scopes.
- `useHotkeys` and `HotkeysProvider` share names with react-hotkeys-hook but
  not signatures. Remove the old library.
