#!/bin/sh
set -eu

# Fabric owns validation, idempotency, the Herdr effect and its receipt. This
# bundled helper is intentionally only an argument-preserving client.
exec provenant fabric herdr steer "$@"
