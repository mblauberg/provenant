//! Non-secret account keys derived at the provider boundary. Never hash tokens
//! or paths: callers pass only the documented identity fields.
use provenant_records::CanonicalV1;
use serde::Deserialize;

/// Lane Q's Claude scheme: normalise the sign-in identifier, then take the
/// first 16 hexadecimal SHA-256 digits. Provider prefixes separate namespaces.
pub fn fingerprint(identity: &str) -> Option<String> {
    digest(&identity.trim().to_lowercase())
}
/// The organisation UUID from Claude's `auth status`, kept apart from the
/// account key so existing keys, routes and quota rows stay valid.
pub fn org_fingerprint(org_id: &str) -> Option<String> {
    digest(&org_id.trim().to_lowercase())
}
pub(crate) fn digest(identity: &str) -> Option<String> {
    if identity.is_empty() || identity.len() > 4096 || identity.chars().any(char::is_control) {
        return None;
    }
    let digest = CanonicalV1::file_digest(identity.as_bytes()).to_string();
    digest.strip_prefix("sha256:")?.get(..16).map(str::to_owned)
}

/// Codex's own account_id, not an auth-directory label or a token substring.
pub fn codex_account(auth: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Auth {
        tokens: Tokens,
    }
    #[derive(Deserialize)]
    struct Tokens {
        account_id: String,
    }
    let auth: Auth = serde_json::from_slice(auth).ok()?;
    Some(format!("codex-{}", digest(&auth.tokens.account_id)?))
}

/// Cursor uses the subject of its signed-in access token. Decoding claims is
/// attribution only, not token validation; the usage request validates auth.
pub fn cursor_account(token: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Claims {
        sub: String,
    }
    let claims: Claims = serde_json::from_slice(&base64url(token.split('.').nth(1)?)?).ok()?;
    Some(format!("cursor-{}", digest(&claims.sub)?))
}

/// Agy's usage log identifies its Google sign-in without opening a token store.
pub fn agy_fingerprint(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (_, tail) = text.rsplit_once("OAuth: authenticated successfully as ")?;
    let email = tail.split_whitespace().next().filter(|s| s.contains('@'))?;
    Some(format!("google-{}", fingerprint(email)?.get(..12)?))
}

pub fn base64url(s: &str) -> Option<Vec<u8>> {
    let (mut bits, mut count) = (0u32, 0u32);
    let mut bytes = Vec::new();
    for c in s.bytes().take_while(|b| *b != b'=') {
        let n = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | n as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(bytes)
}

/// A provider-detected identity registered at a canonical config directory.
/// Registration contains no credential material. The composition edge owns
/// discovery/canonicalisation, and routing selects its account key.
#[derive(Clone, Debug)]
pub struct RegisteredAccount {
    adapter: String,
    key: String,
    directory: std::path::PathBuf,
}
impl RegisteredAccount {
    pub fn new(
        adapter: &str,
        key: &str,
        directory: std::path::PathBuf,
    ) -> Result<Self, crate::ProviderError> {
        use std::path::Component;
        if !provenant_quota::component(adapter)
            || key == "unknown"
            || key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(crate::ProviderError::UnknownIdentity);
        }
        // Agy's registered directory is a `HOME` holding a keychain, never a
        // config tree that overlaps another account's, so any directory below
        // the root qualifies, including a shallow home such as `/Users/me`.
        let shallowest = if provenant_records::AdapterSpec::get(adapter)
            .is_some_and(|spec| spec.isolated_login_keychain)
        {
            2
        } else {
            4
        };
        if !directory.is_absolute()
            || directory
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || directory.components().count() < shallowest
        {
            return Err(crate::ProviderError::InvalidPath);
        }
        Ok(Self {
            adapter: adapter.into(),
            key: key.into(),
            directory,
        })
    }
    pub fn adapter(&self) -> &str {
        &self.adapter
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }
    pub fn validate_route(
        &self,
        route: &provenant_records::ResolvedRoute,
    ) -> Result<(), crate::ProviderError> {
        if route.adapter != self.adapter || route.account_key.as_deref() != Some(&self.key) {
            return Err(provenant_records::policy::Policy::default().boundary_error(
                "IDENTITY_REGISTEREDACCOUNT_VALIDATE_ROUTE_AUTHORITY",
                provenant_records::policy::Facts::violation(
                    "the registered account does not match the resolved provider route",
                ),
                crate::ProviderError::IdentityMismatch,
            ));
        }
        Ok(())
    }
}

/// Resolve the state and credential stores an account actually owns. HOME-rooted
/// adapters keep client state below their declared harness directory, not across
/// their entire HOME. Resolve every store separately to catch symlink aliases.
pub fn account_state_paths(adapter: &str, directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let spec = provenant_records::AdapterSpec::get(adapter);
    let home_rooted = spec
        .and_then(|spec| spec.install)
        .filter(|install| install.lane_home.0 == "HOME");
    let paths = match home_rooted {
        Some(install) => {
            let mut paths = vec![directory.join(install.home)];
            if let Some(spec) = spec {
                paths.extend(
                    spec.credential_files
                        .iter()
                        .map(|file| directory.join(file)),
                );
            }
            paths
        }
        None => vec![directory.to_owned()],
    };
    paths
        .into_iter()
        .map(|path| {
            provenant_core::PathResolver::resolve_existing_ancestor(
                &provenant_os::fs::SystemPathResolver,
                &path,
            )
            .unwrap_or(path)
        })
        .collect()
}

/// Whether resolved state or credential stores overlap, including equality.
pub fn account_directories_overlap(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

/// Immutable registry of detected identities. A selected account cannot include
/// another registered account's config tree in its state grants.
pub struct Accounts(Vec<RegisteredAccount>);
impl Accounts {
    pub fn new(accounts: Vec<RegisteredAccount>) -> Result<Self, crate::ProviderError> {
        if accounts.len() > 128 {
            return Err(crate::ProviderError::Overflow);
        }
        let stores: Vec<_> = accounts
            .iter()
            .map(|account| account_state_paths(&account.adapter, &account.directory))
            .collect();
        for (index, account) in accounts.iter().enumerate() {
            if accounts
                .iter()
                .enumerate()
                .skip(index + 1)
                .any(|(other_index, other)| {
                    let same_identity =
                        account.adapter == other.adapter && account.key == other.key;
                    // Entries for an adapter with only a local login are metadata
                    // associations, not distinct trees that require isolation.
                    let shared_local_login = account.adapter == other.adapter
                        && provenant_records::AdapterSpec::get(&account.adapter)
                            .is_some_and(provenant_records::AdapterSpec::uses_shared_local_login);
                    same_identity
                        || (!shared_local_login
                            && stores[index].iter().any(|path| {
                                stores[other_index]
                                    .iter()
                                    .any(|other| account_directories_overlap(path, other))
                            }))
                })
            {
                return Err(crate::ProviderError::IdentityMismatch);
            }
        }
        Ok(Self(accounts))
    }
    pub fn selected(
        &self,
        route: &provenant_records::ResolvedRoute,
    ) -> Result<&RegisteredAccount, crate::ProviderError> {
        let account = self
            .0
            .iter()
            .find(|account| {
                account.adapter == route.adapter
                    && route.account_key.as_deref() == Some(account.key.as_str())
            })
            .ok_or_else(|| {
                provenant_records::policy::Policy::default().boundary_error(
                    "IDENTITY_REGISTEREDACCOUNT_VALIDATE_ROUTE_AUTHORITY",
                    provenant_records::policy::Facts::violation(
                        "no registered account matches the resolved provider route",
                    ),
                    crate::ProviderError::UnknownIdentity,
                )
            })?;
        account.validate_route(route)?;
        Ok(account)
    }
}

#[cfg(test)]
mod account_registry_tests {
    use super::*;

    // Spec: docs/specs/routing.md#accounts-and-credentials
    #[test]
    fn agy_home_login_can_coexist_with_an_isolated_registered_identity() {
        let home = std::path::PathBuf::from("/fixture/home");
        let local = RegisteredAccount::new("agy", "local", home.clone()).unwrap();
        let isolated =
            RegisteredAccount::new("agy", "agy-registered", home.join(".agy-work")).unwrap();
        let nested =
            RegisteredAccount::new("agy", "agy-nested", home.join(".agy-work/nested")).unwrap();
        let accounts = Accounts::new(vec![local, isolated, nested]).expect("independent stores");
        for (key, directory) in [
            ("local", home.clone()),
            ("agy-registered", home.join(".agy-work")),
            ("agy-nested", home.join(".agy-work/nested")),
        ] {
            let mut route = crate::tests::route("agy", "gemini-3.8-flash");
            route.account_key = Some(key.into());
            assert_eq!(
                accounts
                    .selected(&route)
                    .expect("selected account")
                    .directory(),
                directory
            );
        }
    }

    // Spec: docs/specs/routing.md#accounts-and-credentials
    #[test]
    fn registered_accounts_must_not_share_resolved_stores() {
        let home = std::path::PathBuf::from("/fixture/home");
        let first = RegisteredAccount::new("agy", "agy-first", home.join(".agy-work")).unwrap();
        let nested =
            RegisteredAccount::new("agy", "agy-nested", home.join(".agy-work/.gemini/nested"))
                .unwrap();
        assert!(matches!(
            Accounts::new(vec![first, nested]),
            Err(crate::ProviderError::IdentityMismatch)
        ));
    }

    // Spec: docs/specs/routing.md#accounts-and-credentials
    #[test]
    fn shared_local_login_metadata_accounts_can_share_a_directory() {
        let store = std::path::PathBuf::from("/fixture/home/.local/share/opencode");
        let go = RegisteredAccount::new("opencode", "opencode-go", store.clone()).unwrap();
        let zen = RegisteredAccount::new("opencode", "opencode-zen", store).unwrap();
        assert!(Accounts::new(vec![go, zen]).is_ok());
    }

    // Spec: docs/specs/routing.md#accounts-and-credentials
    #[test]
    fn shared_local_login_does_not_allow_cross_adapter_overlap() {
        let store = std::path::PathBuf::from("/fixture/home/.local/share/opencode");
        let opencode = RegisteredAccount::new("opencode", "opencode-go", store.clone()).unwrap();
        let cursor = RegisteredAccount::new("cursor", "cursor-one", store).unwrap();
        assert!(matches!(
            Accounts::new(vec![opencode, cursor]),
            Err(crate::ProviderError::IdentityMismatch)
        ));
    }

    #[test]
    fn local_login_does_not_hide_cross_provider_root_overlap() {
        let home = std::path::PathBuf::from("/fixture/home");
        let local = RegisteredAccount::new("agy", "local", home.clone()).unwrap();
        let other_provider =
            RegisteredAccount::new("codex", "work", home.join(".gemini/work")).unwrap();
        assert!(matches!(
            Accounts::new(vec![local, other_provider]),
            Err(crate::ProviderError::IdentityMismatch)
        ));
    }
}
