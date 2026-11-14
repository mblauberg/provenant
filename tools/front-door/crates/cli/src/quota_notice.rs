//! Deduplicated, non-secret login expiry notices to registered local chairs.
use provenant_core::{
    ActorId, CancelToken, Clock, HostId, OperationId, ProjectId, Roots, WaitBudget,
};
use provenant_fabric_runtime::{
    coordination::{LocalCoordination, MailInput, Mutation, ThreadPause},
    identity::chair_session_mailbox,
};
use provenant_quota::{AccountScope, LoginExpiryState, QuotaReport};
use provenant_records::CanonicalV1;

/// Deliver only facts observed for the selected account. No credential or raw
/// provider output enters this function. Missing host/session bindings are not
/// invented; the quota report remains available independently of delivery.
pub(super) fn deliver(
    roots: &Roots,
    report: &QuotaReport,
    warning_seconds: u64,
    clock: &dyn Clock,
) -> Result<(), String> {
    let now = clock
        .wall_now()
        .map_err(|error| error.to_string())?
        .as_timestamp()
        .as_second();
    let candidates: Vec<_> = report
        .accounts
        .iter()
        .filter_map(|row| {
            let login = row.login_expiry.as_ref()?;
            let current =
                super::login_expiry_at(roots.state.as_path(), &row.account, warning_seconds, now);
            if current.expires_unix_seconds != login.expires_unix_seconds
                || !matches!(
                    current.state,
                    LoginExpiryState::Expiring | LoginExpiryState::Expired
                )
            {
                return None;
            }
            matches!(
                login.state,
                LoginExpiryState::Expiring | LoginExpiryState::Expired
            )
            .then_some((row, login.expires_unix_seconds?))
        })
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let Some(host) =
        provenant_config::lookup_host(roots).map_err(|_| "host identity unavailable".to_owned())?
    else {
        return Ok(());
    };
    // One shared budget bounds all delivery work; undelivered notices retry on
    // the next tick, and completed operations replay idempotently.
    let budget = WaitBudget::cli(clock.mono_now(), 2).map_err(|e| e.to_string())?;
    let mut failures = 0usize;
    for project in provenant_config::registered_projects(roots)
        .map_err(|_| "registered chair projects unavailable".to_owned())?
    {
        let project = project.project_id();
        let sessions = match provenant_workflow::session::discover_sessions(&roots.state, project) {
            Ok(sessions) => sessions,
            Err(_) => {
                failures += 1;
                continue;
            }
        };
        for chair in sessions {
            if chair.record.host != host {
                continue;
            }
            let session = chair.record.host_session_id.as_str();
            for (row, expiry) in &candidates {
                if budget.deadline().is_expired(clock.mono_now()) {
                    return Err(
                        "login notice delivery budget elapsed; the next quota tick retries".into(),
                    );
                }
                if send(
                    &roots.state,
                    (project, host),
                    session,
                    &row.account,
                    *expiry,
                    clock,
                    budget,
                )
                .is_err()
                {
                    failures += 1;
                }
            }
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(format!(
            "{failures} chair delivery or discovery operations failed; the next quota tick retries"
        ))
    }
}

fn send(
    state: &provenant_core::StateRoot,
    (project, host): (ProjectId, HostId),
    session: &str,
    account: &AccountScope,
    expiry: i64,
    clock: &dyn Clock,
    budget: WaitBudget,
) -> Result<(), String> {
    account.validate().map_err(|e| e.to_string())?;
    let digest =
        CanonicalV1::record_digest("quota-notice-actor", 1, &host).map_err(|e| e.to_string())?;
    let mut sender = [0; 16];
    sender.copy_from_slice(&digest.as_bytes()[..16]);
    let sender = ActorId::from_bytes(sender);
    let recipient = chair_session_mailbox(project, session);
    let key = CanonicalV1::bytes(&(
        "login-expiry",
        &account.adapter,
        &account.account_key,
        &account.label,
        expiry,
        session,
    ))
    .map_err(|e| e.to_string())?;
    let digest = CanonicalV1::file_digest(key.as_bytes())
        .to_string()
        .replace(':', "-");
    let operation =
        OperationId::parse(&format!("login-expiry-{digest}")).map_err(|e| e.to_string())?;
    let expires_at = jiff::Timestamp::from_second(expiry).map_err(|e| e.to_string())?;
    let body = format!(
        "Login for {} [{}] expires {expires_at}. Run `provenant quota` for account-specific recovery.",
        account.label, account.adapter
    );
    let cancel = CancelToken::new();
    let service = LocalCoordination::open(state, project, sender, clock, &ThreadPause, &cancel)
        .map_err(|e| e.to_string())?;
    service
        .mutate(
            Mutation::Mail(MailInput::Send {
                extra: Default::default(),
                recipients: vec![recipient],
                kind: provenant_fabric_store::MessageKind::Note,
                body,
                artifacts: Vec::new(),
                reply_to: None,
            }),
            &operation,
            budget,
            &cancel,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, DurationMicros, MonoTime, RootInputs, TestClock, WallTime};
    use provenant_fabric_runtime::coordination::{CoordinationApi, MailResult};
    use provenant_quota::{LoginExpiryState, LoginObservation};

    #[test]
    fn delivery_reaches_registered_chair_once_and_ignores_unknown_expiry() {
        use provenant_quota::{AccountRow, Coverage, LoginExpiryView, QuotaStatus};
        use provenant_workflow::session::{HostSessionId, SessionStore};
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let checkout = temp.path().join("checkout");
        let state = temp.path().join("state");
        for path in [&home, &checkout, &state] {
            std::fs::create_dir(path).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let common = checkout.join(".git");
        std::fs::create_dir(&common).unwrap();
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(state);
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let project = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(common).unwrap(),
        )
        .unwrap()
        .project_id();
        let host = provenant_config::bind_host(&roots).unwrap();
        let clock = TestClock::new(
            WallTime::from_micros(100_000_000).unwrap(),
            MonoTime::from_micros(0),
        );
        let cancel = CancelToken::new();
        SessionStore::new(
            roots.state.clone(),
            project,
            HostSessionId::parse("session").unwrap(),
        )
        .with_chair(host, ActorId::from_bytes([9; 16]))
        .save(
            b"fixture",
            WaitBudget::cli(clock.mono_now(), 2).unwrap(),
            &cancel,
            &clock,
        )
        .unwrap();
        let mut report = QuotaReport {
            schema_version: 1,
            generated_unix_seconds: 100,
            accounts: vec![AccountRow {
                account: AccountScope {
                    adapter: "claude".into(),
                    account_key: "key".into(),
                    label: "work".into(),
                },
                readiness: None,
                auth_state: None,
                login_expiry: Some(LoginExpiryView {
                    state: LoginExpiryState::Expiring,
                    expires_unix_seconds: Some(140),
                    observed_unix_seconds: Some(100),
                }),
                snapshot: None,
                status: QuotaStatus::Available,
                resets_at: None,
                refusals: vec![],
                free_pool_refusal: false,
                age_seconds: None,
                next_due_unix_seconds: None,
                refreshing: false,
                error: None,
                repair: None,
                capacity_cooldowns: vec![],
                display: None,
            }],
            sources: vec![],
            coverage: Coverage { total: 1, known: 1 },
            refreshing_ids: vec![],
            next_due_unix_seconds: None,
            report_path: String::new(),
            display: Default::default(),
            display_icon_warning: None,
        };
        let readiness = super::super::private(roots.state.as_path()).unwrap();
        let readiness = super::super::child(
            &readiness,
            std::ffi::OsStr::new(provenant_quota::READINESS_DIRECTORY),
        )
        .unwrap();
        let directory = super::super::child(&readiness, std::ffi::OsStr::new("key")).unwrap();
        let file = provenant_quota::ReadinessFile {
            login: Some(LoginObservation {
                scope: provenant_quota::ReadinessScope {
                    host: provenant_quota::LOCAL_HOST.into(),
                    adapter: "claude".into(),
                    account_key: "key".into(),
                    route: provenant_quota::ACCOUNT_ROUTE.into(),
                },
                observed_unix_seconds: 100,
                valid_until_unix_seconds: 1000,
                expires_unix_seconds: Some(140),
            }),
            ..Default::default()
        };
        provenant_os::fs::Directory::atomic_write_with_permissions(
            &directory,
            std::ffi::OsStr::new("claude.json"),
            ".fixture",
            &serde_json::to_vec(&file).unwrap(),
            0o600,
            provenant_os::fs::PublishMode::Replace,
        )
        .unwrap()
        .require_durable()
        .unwrap();
        deliver(&roots, &report, 50, &clock).unwrap();
        deliver(&roots, &report, 50, &clock).unwrap();
        report.accounts[0].login_expiry.as_mut().unwrap().state = LoginExpiryState::Unknown;
        deliver(&roots, &report, 50, &clock).unwrap();
        let service = LocalCoordination::open(
            &roots.state,
            project,
            ActorId::from_bytes([9; 16]),
            &clock,
            &ThreadPause,
            &cancel,
        )
        .unwrap();
        let MailResult::Read { page, .. } = service
            .mail(
                MailInput::Read {
                    extra: Default::default(),
                    after: Some(0),
                    limit: 10,
                    session: Some("session".into()),
                },
                WaitBudget::cli(clock.mono_now(), 2).unwrap(),
                &cancel,
            )
            .unwrap()
        else {
            panic!("mail result")
        };
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].message.body.contains("work [claude]"));
    }

    #[test]
    fn chair_notice_is_deduplicated_and_recovery_allows_a_new_expiry() {
        let temp = tempfile::tempdir().unwrap();
        let mut inputs = RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let host = HostId::from_bytes([2; 16]);
        let mut clock = TestClock::new(
            WallTime::from_micros(100_000_000).unwrap(),
            MonoTime::from_micros(0),
        );
        let account = AccountScope {
            adapter: "claude".into(),
            account_key: "key".into(),
            label: "work".into(),
        };
        let mut evidence = LoginObservation {
            scope: provenant_quota::ReadinessScope {
                host: provenant_quota::LOCAL_HOST.into(),
                adapter: "claude".into(),
                account_key: "key".into(),
                route: provenant_quota::ACCOUNT_ROUTE.into(),
            },
            observed_unix_seconds: 100,
            valid_until_unix_seconds: 1000,
            expires_unix_seconds: Some(140),
        };
        assert_eq!(evidence.state(100, 50), LoginExpiryState::Expiring);
        send(
            &roots.state,
            (project, host),
            "session",
            &account,
            140,
            &clock,
            WaitBudget::cli(clock.mono_now(), 2).unwrap(),
        )
        .unwrap();
        send(
            &roots.state,
            (project, host),
            "session",
            &account,
            140,
            &clock,
            WaitBudget::cli(clock.mono_now(), 2).unwrap(),
        )
        .unwrap();
        evidence.expires_unix_seconds = Some(500);
        assert_eq!(evidence.state(100, 50), LoginExpiryState::Fresh);
        clock
            .advance(DurationMicros::from_micros(360_000_000))
            .unwrap();
        assert_eq!(evidence.state(460, 50), LoginExpiryState::Expiring);
        send(
            &roots.state,
            (project, host),
            "session",
            &account,
            500,
            &clock,
            WaitBudget::cli(clock.mono_now(), 2).unwrap(),
        )
        .unwrap();
        evidence.expires_unix_seconds = None;
        assert_eq!(evidence.state(460, 50), LoginExpiryState::Unknown);
        let cancel = CancelToken::new();
        let service = LocalCoordination::open(
            &roots.state,
            project,
            ActorId::from_bytes([3; 16]),
            &clock,
            &ThreadPause,
            &cancel,
        )
        .unwrap();
        let MailResult::Read { page, .. } = service
            .mail(
                MailInput::Read {
                    extra: Default::default(),
                    after: Some(0),
                    limit: 10,
                    session: Some("session".into()),
                },
                WaitBudget::cli(clock.mono_now(), 2).unwrap(),
                &cancel,
            )
            .unwrap()
        else {
            panic!("mail result")
        };
        assert_eq!(page.items.len(), 2);
        for item in page.items {
            assert!(item.message.body.contains("work [claude]"));
            assert!(item.message.body.contains("1970-01-01T00:"));
            assert!(!item.message.body.contains("fake-secret"));
            assert!(!item.message.body.contains("raw-provider-output"));
        }
    }
}
