# Disabled quota boundary fixtures

`expired.vscdb` is a synthetic SQLite `ItemTable` fixture with the Cursor
`cursorAuth/accessToken` key and an inert token string. It grants no runtime
authority. The integration test places it and synthetic Claude credentials in
an isolated HOME and installs fake curl/security/provider executables that
record any attempted invocation and fail. Show, tick and help must never invoke
them or expose their contents.

These are negative authority fixtures, not provider activation recordings.
The provider activation matrix remains all disabled; no successful HTTP,
keychain, or database extraction contract is claimed. Qualifying redacted
recordings and the supervisor's memory-only effects must precede activation.
