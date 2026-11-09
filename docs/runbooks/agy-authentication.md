# Agy authentication in lanes
Owner: provenant
Anchor: tools/front-door/crates/providers/src/agy.rs

Agy uses the operating system's native keyring for silent sign-in. The
[vendor authentication guide](https://antigravity.google/docs/install/#local-silent-keyring-sign-in)
describes saved token profiles and a browser fallback when none is found. It
does not specify macOS service names, token refresh writes or keychain search
list files. Do not infer those details from the guide.

`providers/src/agy.rs#prepare` gives the CLI a private HOME containing a link to
the selected login keychain. An isolated account must have its own regular
keychain file; missing or replaced isolated keychains fail closed. The CLI
continues to own sign-in and token refresh through native keychain services.
Provenant neither copies credentials nor runs a separate refresh flow.

`confinement/src/permissions/render.rs#selected_keychain` recognises the typed,
selected authentication mechanism for both the user's login and isolated
keychains. It permits the native client and keychain IPC. The exact selected
login keychain can be read; neighbouring keychains and direct credential writes
remain denied. Keychain IPC runs through the user's macOS security services;
path restrictions on the client do not constrain what those services can access.
Account identity verification remains necessary.

## Diagnosis and evidence

The previous renderer always denied securityd IPC, and also denied execution of
`/usr/bin/security` for the shared login keychain. It excluded the shared login
keychain from authentication read exceptions. These conflict with the adapter's
native keychain launch requirements. Unconfined quota and identity probes do not
receive those denials. Launch-shape regression tests cover both account types
and both access modes through preparation and profile rendering.

This establishes a launch defect, not the exact failing operation in the
reported 1.6.22 attempt. Its output was inaccessible from the repair lane
(`not_found: the target is outside the calling lane's graph or permitted
subtree`). No real account was launched during repair. Private HOME, search
list preferences, refresh and network hypotheses therefore remain unproven.

`providers/src/agy.rs#failure_diagnostic` distinguishes explicit authentication
timeouts, missing saved sign-ins and keychain interaction refusals. If Agy emits
only `authentication failed or timed out`, the diagnostic says that the CLI
supplied no cause. That message alone cannot establish timeout or refusal;
Provenant must not fabricate a more specific cause. Arbitrary stderr is not
copied into the diagnostic.

## Chair checks after install

Use a trivial prompt, the same model and selected account, first in a read-only
lane and then in a writer lane. Repeat for the login account and each isolated
account. Keep credential stores closed throughout.

1. Compare a native headless launch under the selected account HOME with the
   installed lane. If both now work, the renderer correction resolves the
   observed discrepancy. If native works and the lane fails, record only the
   failing operation, service or endpoint from macOS sandbox denial metadata;
   do not collect credential contents or token-bearing payloads. A denied
   operation identifies the next minimal profile correction; no denial leaves
   HOME discovery or an application timeout as candidates.
2. If needed, on a configured host compare the same harmless headless request
   under an ephemeral HOME with only the selected keychain link, unconfined,
   against the account HOME. Ephemeral-only failure isolates HOME discovery
   from Seatbelt. Both succeeding isolates confinement. Do not add Preferences
   or copy Gemini configuration until a concrete missing path is observed.
3. Repeat a successful lane after the existing token needs normal refresh,
   followed by a native headless request for the same account. Failure only
   after expiry points to refresh, rather than initial keychain discovery.
   A specific keychain denial distinguishes storage access from an explicit
   authentication timeout. If the CLI supplies neither, retain the unknown
   diagnosis and ask the vendor for a cause-bearing diagnostic.

Do not sign out a working account to manufacture a failure. Synthetic stderr
fixtures cover signed-out and denied diagnostics; real launches belong to the
account owner. No `security` command or credential inspection is required.

## Accepted risk

A lane using the shared login account can read the host `login.keychain-db`
file, including from tools the provider launches. The file is encrypted, and
the owner accepted this exposure so that Agy works. Replacing it with an
isolated credential store or a host-side broker is follow-up work.
