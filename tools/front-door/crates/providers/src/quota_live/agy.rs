use super::*;
use crate::identity::agy_fingerprint;

pub(crate) fn capture_source(
    live: &LiveQuota,
    plan: &QuotaProbePlan,
    previous: Option<&AccountRecord>,
) -> Result<ProbeCapture, ProbeFailure> {
    live.capture_source(plan, previous, None, |live, account, deadline, details| {
        live.agy(account, deadline, details)
    })
}

pub(super) fn agy_signed_in_account(
    signed_in: bool,
    log: Option<Vec<u8>>,
) -> Result<String, ProbeFailure> {
    if !signed_in {
        return Err(ProbeFailure::Auth);
    }
    let actual = log
        .and_then(|bytes| agy_fingerprint(&bytes))
        .ok_or(ProbeFailure::Auth)?;
    Ok(actual)
}

impl LiveQuota {
    #[allow(clippy::print_stderr)]
    pub(super) fn agy(
        &self,
        account: &Account,
        deadline: i64,
        details: &mut QuotaDetails,
    ) -> Result<Value, ProbeFailure> {
        // An isolated agy account is signed in under its own directory as
        // `HOME`. A missing keychain has its own failure, never auth, and the
        // probe re-checks it around the spawn.
        // Never let a failed quota probe leave an earlier dispatch identity cached.
        crate::agy::forget_identity(&self.state, &account.identity_dir);
        let stamp_before = crate::agy::keychain_stamp(&account.identity_dir);
        let probe_home =
            crate::agy::ProbeHome::select(&account.identity_dir, &self.home).map_err(|error| {
                match error {
                    crate::ProviderError::IsolatedKeychainUnavailable(_) => {
                        ProbeFailure::IsolatedKeychainUnavailable
                    }
                    _ => ProbeFailure::Auth,
                }
            })?;
        let scratch = crate::host::HostProbeDir::create(&self.state)
            .map_err(|_| ProbeFailure::ProbeDirectory)?;
        let log = scratch.path().join("cli.log");
        let log_arg = log.to_str().ok_or(ProbeFailure::Malformed)?;
        let payload = self.json_command(
            &probe_home,
            "agy",
            &[
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--log-file",
                log_arg,
            ],
            None,
            deadline,
            crate::agy::REGISTRATION.quota.probe_seconds,
        )?;
        let signed_in = payload.get("status").and_then(Value::as_str) == Some("SUCCESS");
        let actual = agy_signed_in_account(signed_in, read(&log).ok())?;
        let key = format!("agy-{actual}");
        // A safe atomic save is not a missing keychain. Its identity stamp is
        // uncertain for this observation, so retry briefly rather than applying
        // the unavailable-keychain backoff or caching an old account identity.
        if crate::agy::keychain_stamp(&account.identity_dir) != stamp_before {
            return Err(ProbeFailure::Network);
        }
        let models = crate::quota::agy::extract_agy_models(Some(&payload), None);
        crate::agy::remember_identity(
            &self.state,
            &account.identity_dir,
            &key,
            self.now(),
            stamp_before,
            &models,
        );
        if key != account.scope.account_key {
            if let Some(roots) = &self.roots {
                // Failure to save never makes the stale identity routable. A
                // later quota probe retries under the normal mismatch backoff.
                match crate::accounts::reconcile_agy(
                    roots,
                    &account.identity_dir,
                    &account.scope.account_key,
                    &key,
                ) {
                    Ok(true) => {
                        for registered in self.accounts.borrow_mut().iter_mut() {
                            if registered.scope == account.scope {
                                registered.scope.account_key.clone_from(&key);
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        *self.last_reconcile_error.borrow_mut() = Some(error.to_string());
                        eprintln!("provenant: failed to reconcile agy account switch: {error}");
                    }
                }
            }
            return Err(ProbeFailure::AccountMismatch);
        }
        details.fingerprint = Some(actual);
        Ok(payload)
    }
}
