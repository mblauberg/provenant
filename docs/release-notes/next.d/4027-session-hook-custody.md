## Fixes

### Sessions and hooks

- The session-start hook no longer prints the active-custody list twice: when the derived digest is shown, the autosave's duplicate custody section is withheld. When the bounded hook output reaches its cap, derived state now shrinks before the chair's checkpoint, so `## Intent` (up to 3 KiB) and the new `## Now` section (up to 1 KiB) survive intact while the digest and then the rest of the excerpt give way. A fresh checkpoint with nothing actionable drops the "no save needed by freshness policy" boilerplate. (#4027)
