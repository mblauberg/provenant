use super::*;
use crate::identity::base64url;

fn identity_key(directory: &Path) -> Option<String> {
    crate::cursor::read_identity(directory)
        .ok()
        .map(|identity| identity.account_key)
}

pub(crate) fn capture_source(
    live: &LiveQuota,
    plan: &QuotaProbePlan,
    previous: Option<&AccountRecord>,
) -> Result<ProbeCapture, ProbeFailure> {
    live.capture_source(
        plan,
        previous,
        Some(identity_key),
        |live, account, deadline, _| live.cursor(account, deadline),
    )
}

impl LiveQuota {
    pub(super) fn cursor(&self, account: &Account, deadline: i64) -> Result<Value, ProbeFailure> {
        let identity =
            crate::cursor::read_identity(&account.identity_dir).map_err(|_| ProbeFailure::Auth)?;
        let token = identity.access_token().ok_or(ProbeFailure::Auth)?;
        let claims = token
            .split('.')
            .nth(1)
            .and_then(base64url)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .ok_or(ProbeFailure::Auth)?;
        let user = claims
            .get("sub")
            .and_then(Value::as_str)
            .and_then(|s| s.rsplit('|').next())
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            .ok_or(ProbeFailure::Auth)?;
        if claims
            .get("exp")
            .and_then(Value::as_i64)
            .is_none_or(|e| e <= self.now() + 60)
        {
            return Err(ProbeFailure::Auth);
        }
        self.http(
            "https://cursor.com/api/usage-summary",
            &[(
                "Cookie",
                format!("WorkosCursorSessionToken={user}%3A%3A{token}"),
            )],
            deadline,
        )
    }
}
