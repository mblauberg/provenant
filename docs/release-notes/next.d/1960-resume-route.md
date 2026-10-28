## Fixes

Resuming a named route keeps the prior attempt's applied provider, model and
account instead of drawing the pool again. Refused continuation requests no
longer expose attempt IDs that were never recorded.
