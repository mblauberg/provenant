## Fixes

- Failed Agy lanes retain partial response text. A headless permission denial
  now appears in the result and terminal reason, including when the provider
  produced no answer, so `lanes` and `output --json` explain which permission
  was denied (#1776).
