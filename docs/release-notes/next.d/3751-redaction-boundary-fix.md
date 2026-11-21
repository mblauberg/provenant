## Fixes

### Git and safety

- Local-disclosure masking of credential-shaped values now stops at JSON string closes, raw NUL record separators and line endings, so masking cannot corrupt JSON documents or `-z` git porcelain that a caller parses; values the caller owns are marked with a redaction-ownership reason rather than a byte-exact-parse one (#3751)
