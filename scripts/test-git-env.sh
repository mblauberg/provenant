#!/bin/sh
# Fixture repositories must not inherit a writer lane's Git selectors or shim.
for name in $(env | sed -n 's/^\(GIT_[A-Za-z0-9_]*\)=.*/\1/p'); do
  unset "$name"
done
PATH="/opt/homebrew/bin:/usr/local/bin:/opt/local/bin:/usr/bin:/bin:$PATH"
GIT_CONFIG_GLOBAL=/dev/null
GIT_CONFIG_NOSYSTEM=1
export PATH GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM
