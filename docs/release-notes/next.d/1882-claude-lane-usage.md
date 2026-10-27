## Fixes

Claude lane attempts now record input, output, cache read and cache write token
counts from the provider's final result event. Settled Claude attempts with no
provider usage record why usage is absent instead of leaving a bare missing
value.
