## Fixes

### Git and safety

- Local-disclosure masking of credential-shaped values now stops at JSON string closes, raw NUL record separators and line endings, so a diagnostic stream cannot swallow the record or document boundary a caller parses. Byte-exact captures are not masked at all, and their text is masked only when it reaches an error, warning or log; seed-build argv is masked before it is echoed into an error (#3751)
