use super::*;

fn identity_key(directory: &Path) -> Option<String> {
    read(&directory.join("auth.json"))
        .ok()
        .and_then(|bytes| crate::identity::codex_account(&bytes))
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
        |live, account, deadline, _| live.codex(account, deadline),
    )
}

impl LiveQuota {
    pub(super) fn codex(&self, account: &Account, deadline: i64) -> Result<Value, ProbeFailure> {
        // The service handles its own credentials. EOF closes this non-inference exchange.
        let input = concat!(
            "{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"provenant\",\"version\":\"1\"}}}\n",
            "{\"method\":\"initialized\"}\n",
            "{\"id\":2,\"method\":\"account/rateLimits/read\"}\n"
        );
        let out = self.command(
            "codex",
            &["-s", "read-only", "-a", "never", "app-server"],
            Some(("CODEX_HOME", &account.identity_dir)),
            Some(input.as_bytes()),
            deadline,
            crate::codex::REGISTRATION.quota.probe_seconds,
        )?;
        if out.stdout.truncated || out.stdout.read_failed {
            return Err(ProbeFailure::Malformed);
        }
        for line in out.stdout.bytes.split(|b| *b == b'\n') {
            if let Ok(v) = serde_json::from_slice::<Value>(line)
                && v.get("id").and_then(Value::as_u64) == Some(2)
                && let Some(result) = v.get("result").filter(|r| r.is_object())
            {
                return Ok(result.clone());
            }
        }
        // App-server can be unavailable when its runtime database is read-only.
        // The usage endpoint accepts the same account-scoped OAuth token.
        let auth = value(&account.identity_dir.join("auth.json"))?;
        let token = auth
            .pointer("/tokens/access_token")
            .and_then(Value::as_str)
            .ok_or(ProbeFailure::Auth)?;
        let mut headers = vec![("Authorization", format!("Bearer {token}"))];
        if let Some(id) = auth.pointer("/tokens/account_id").and_then(Value::as_str) {
            headers.push(("ChatGPT-Account-Id", id.to_owned()));
        }
        self.http(
            "https://chatgpt.com/backend-api/wham/usage",
            &headers,
            deadline,
        )
    }
}
