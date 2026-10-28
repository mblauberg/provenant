## Behaviour changes

- Clear an account's quota authentication error when a lane on that account succeeds. Menu-open and wake events compare credential and profile metadata and retry signed-out accounts when it changes, without reading credential contents or adding a polling loop. Unchanged signed-out accounts keep the paced fallback retry; manual refresh also retries them.
