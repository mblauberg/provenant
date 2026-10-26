Claude dispatch now reports credential read failures with a non-secret reason,
without suggesting another sign-in or starting an OAuth refresh that could
revoke a token used by other running lanes. Corrupt stored credential JSON is
also a read failure and does not trigger refresh. A token that has already
expired is still refreshed, since no running lane can be using it. Quota
refresh continues to use file-backed credentials when that is the login's
credential source (#1789).
