# Disabled quota boundary fixtures

`expired.vscdb` is a synthetic SQLite `ItemTable` fixture with the Cursor
`cursorAuth/accessToken` key and an inert token string. It grants no runtime
authority. The integration test places it and synthetic Claude credentials in
an isolated HOME and installs fake curl/security/provider executables that
record any attempted invocation and fail. Show, tick and help must never invoke
them or expose their contents.

These negative authority fixtures cover credential isolation independently of
provider availability. All provider adapters are enabled by default; recordings
serve replay tests only.
