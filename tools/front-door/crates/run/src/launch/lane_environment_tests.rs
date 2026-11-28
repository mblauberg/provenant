use super::*;

#[test]
fn unavailable_lane_port_settings_skip_allocation() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let not_directory = temp.path().join("not-directory");
    std::fs::write(&not_directory, b"fixture")?;
    let policy = provenant_records::policy::Policy::default();
    assert!(
        !policy
            .rule("LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE")
            .rule
            .refusable
    );
    assert_eq!(
        policy
            .rule("LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE")
            .rule
            .strength,
        provenant_records::policy::Strength::Warn
    );
    assert!(lane_port_plan_or_warn(&not_directory, &policy).is_none());
    let mut off = policy.clone();
    off.overlay(
        &std::collections::BTreeMap::from([(
            "LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE".into(),
            provenant_records::policy::Strength::Off,
        )]),
        provenant_records::Layer::Project,
    );
    assert_eq!(
        off.rule("LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE").source.layer,
        provenant_records::Layer::Project
    );
    assert!(lane_port_plan_or_warn(&not_directory, &off).is_none());
    let defaults = lane_port_plan_or_warn(temp.path(), &policy).expect("default plan");
    assert_eq!(
        defaults.blocks_origin,
        Some(provenant_os::lane_ports::BlocksOrigin::CompiledDefault)
    );
    std::fs::write(
        temp.path().join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nlane_port_blocks=2\n",
    )?;
    let configured = lane_port_plan_or_warn(temp.path(), &policy).expect("configured plan");
    assert_eq!(configured.blocks, 2);
    assert_eq!(
        configured.blocks_origin,
        Some(provenant_os::lane_ports::BlocksOrigin::HostConfig)
    );
    Ok(())
}

#[test]
fn lane_environment_exposes_applied_route_and_clears_absent_values() {
    use provenant_core::consumer_env::{ACCOUNT, EFFORT, MODEL, ROUTE};
    let mut environment = vec![];
    let mut route = RouteRecord {
        provider: Some("claude".into()),
        model: Some("opus[1m]".into()),
        effort: Some("high".into()),
        account_label: Some("work [east]".into()),
        ..Default::default()
    };
    let parent = AttemptId::from_bytes([6; 16]);
    set_lane_environment(&mut environment, 1, parent, &route);
    for (name, value) in [
        (ROUTE, "claude/opus[1m]@high"),
        (MODEL, "opus[1m]"),
        (EFFORT, "high"),
        (ACCOUNT, "work [east]"),
    ] {
        assert!(
            environment
                .iter()
                .any(|(key, actual)| key == name && actual == value)
        );
    }
    route.effort = None;
    route.account_label = None;
    set_lane_environment(&mut environment, 1, parent, &route);
    assert!(
        environment
            .iter()
            .any(|(key, value)| key == ROUTE && value == "claude/opus[1m]")
    );
    assert!(
        !environment
            .iter()
            .any(|(key, _)| key == EFFORT || key == ACCOUNT)
    );
}

#[test]
fn lane_environment_overrides_adapter_values() {
    let parent = AttemptId::from_bytes([6; 16]);
    let mut environment = vec![
        (
            OsString::from(provenant_core::consumer_env::LANE_DEPTH),
            OsString::from("99"),
        ),
        (OsString::from("OTHER"), OsString::from("kept")),
    ];
    set_lane_environment(&mut environment, 2, parent, &RouteRecord::default());
    assert_eq!(
        environment,
        vec![
            (OsString::from("OTHER"), OsString::from("kept")),
            (
                OsString::from(provenant_core::consumer_env::LANE_DEPTH),
                OsString::from("2")
            ),
            (
                OsString::from(provenant_core::consumer_env::PARENT_ATTEMPT_ID),
                OsString::from(parent.to_string())
            ),
        ]
    );
}
