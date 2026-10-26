Claude dispatch now fails with a credential-unavailable error when the host
cannot read the login keychain. It no longer starts an OAuth refresh probe in
that case, which could revoke a token used by other running lanes (#1789).
