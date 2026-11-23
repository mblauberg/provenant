use super::*;

#[test]
fn provider_and_account_pacing_share_a_table() {
    let config: RoutingConfig = toml_edit::de::from_str(
            "[pacing]\nclaude = 'pace'\n[pacing.accounts.pro]\npolicy = 'pace'\nwindows = ['five_hour']\nreserve = { five_hour = 15 }\ndeadline = 'thu 00:00'\n[pacing.free.opencode]\npolicy = 'max'\n",
        ).unwrap();
    assert_eq!(config.pacing.get("claude"), Some(&PacingPolicy::Pace));
    assert_eq!(
        config.pacing.accounts["pro"].reserve[&PacingWindow::FiveHour],
        15
    );
    assert_eq!(
        config.pacing.free["opencode"].policy,
        Some(PacingPolicy::Max)
    );
    assert!(config.validate().is_empty());
}

#[test]
fn deadline_forms_and_unknown_keys_are_validated_together() {
    for deadline in [
        "'thu 00:00'",
        "'day 25'",
        "'2026-11-02T09:00'",
        "{reset_of='account-a'}",
        "{reset_of='account-a',window='monthly'}",
    ] {
        let profile: PacingProfile =
            toml_edit::de::from_str(&format!("deadline={deadline}")).unwrap();
        assert!(profile.deadline.is_some());
    }
    let profile: PacingProfile = toml_edit::de::from_str("windows=['daily', 'five_hour']").unwrap();
    assert_eq!(
        profile.windows,
        Some(vec![PacingWindow::Daily, PacingWindow::FiveHour])
    );
    assert!(toml_edit::de::from_str::<PacingProfile>("windows=['hourly']").is_err());
    for invalid in [
        "deadline='thu 25:00'",
        "deadline='day 32'",
        "deadline={reset_of='account-a',when='weekly'}",
        "policy='unlimited'",
        "after_deadline='stop'",
        "window=['five_hour']",
    ] {
        assert!(
            toml_edit::de::from_str::<PacingProfile>(invalid).is_err(),
            "{invalid}"
        );
    }
    let error =
        toml_edit::de::from_str::<PacingProfile>("deadline={reset_of='account-a',when='weekly'}")
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("reset_of") && error.contains("window"),
        "{error}"
    );
}

#[test]
fn invalid_profile_is_rejected() {
    let mut config = RoutingConfig::default();
    config.pacing.accounts.insert(
        "pro".into(),
        PacingProfile {
            deadline: Some(PacingDeadline::ResetOf(ResetDeadline {
                reset_of: "bad account".into(),
                window: PacingWindow::SevenDay,
            })),
            reserve: BTreeMap::from([(PacingWindow::FiveHour, 101)]),
            ..PacingProfile::default()
        },
    );
    let errors = config
        .validate()
        .into_iter()
        .map(|error| (error.field, error.code))
        .collect::<Vec<_>>();
    assert!(errors.contains(&("pacing.accounts.pro.deadline".into(), "selector")));
    assert!(errors.contains(&("pacing.accounts.pro.reserve".into(), "range")));
}
