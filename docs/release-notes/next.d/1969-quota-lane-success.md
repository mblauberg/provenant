## Behaviour changes

- A successful lane clears its account's quota authentication error. Opening the menu or waking the host rechecks signed-out accounts when their credential or profile metadata has changed, without reading credential contents or polling. Unchanged signed-out accounts keep the paced fallback retry, and manual refresh also retries them (#1969).
