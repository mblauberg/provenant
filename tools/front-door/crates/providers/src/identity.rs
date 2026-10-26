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
fn digest(identity: &str) -> Option<String> {
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
        if provenant_records::AdapterSpec::get(adapter).is_none()
            || key == "unknown"
            || key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(crate::ProviderError::UnknownIdentity);
        }
        if !directory.is_absolute()
            || directory
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || directory.components().count() < 4
        {
            return Err(crate::ProviderError::InvalidPath);
        }
        Ok(Self {
            adapter: adapter.into(),
            key: key.into(),
            directory,
        })
    }
    pub(crate) fn adapter(&self) -> &str {
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
            return Err(crate::ProviderError::IdentityMismatch);
        }
        Ok(())
    }
}

/// Immutable registry of detected identities. A selected account cannot include
/// another registered account's config tree in its state grants.
pub struct Accounts(Vec<RegisteredAccount>);
impl Accounts {
    pub fn new(accounts: Vec<RegisteredAccount>) -> Result<Self, crate::ProviderError> {
        if accounts.len() > 128 {
            return Err(crate::ProviderError::Overflow);
        }
        for (index, account) in accounts.iter().enumerate() {
            if accounts.iter().skip(index + 1).any(|other| {
                (account.adapter == other.adapter && account.key == other.key)
                    || account.directory.starts_with(&other.directory)
                    || other.directory.starts_with(&account.directory)
            }) {
                return Err(crate::ProviderError::IdentityMismatch);
            }
        }
        Ok(Self(accounts))
    }
    pub fn selected(
        &self,
        route: &provenant_records::ResolvedRoute,
    ) -> Result<&RegisteredAccount, crate::ProviderError> {
        self.0
            .iter()
            .find(|a| a.validate_route(route).is_ok())
            .ok_or(crate::ProviderError::UnknownIdentity)
    }
}
