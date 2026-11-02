use super::set_cargo_profile;
use provenant_config::LaneBuildSettings;
fn value(env: &[(std::ffi::OsString, std::ffi::OsString)], name: &str) -> Option<String> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.to_string_lossy().into_owned())
}
#[test]
fn lane_defaults_disable_incremental_and_trim_debug_info() {
    let mut env = Vec::new();
    set_cargo_profile(&mut env, &LaneBuildSettings::default());
    assert_eq!(value(&env, "CARGO_INCREMENTAL").as_deref(), Some("0"));
    assert_eq!(
        value(&env, "CARGO_PROFILE_DEV_DEBUG").as_deref(),
        Some("line-tables-only")
    );
}
#[test]
fn configuration_overrides_the_defaults() {
    let mut env = Vec::new();
    set_cargo_profile(
        &mut env,
        &LaneBuildSettings {
            cargo_incremental: Some(true),
            cargo_debug: Some("full".into()),
            ..LaneBuildSettings::default()
        },
    );
    assert_eq!(value(&env, "CARGO_INCREMENTAL").as_deref(), Some("1"));
    assert_eq!(
        value(&env, "CARGO_PROFILE_DEV_DEBUG").as_deref(),
        Some("full")
    );
}
#[test]
fn supplied_environment_takes_precedence() {
    let mut env = vec![("CARGO_INCREMENTAL".into(), "1".into())];
    set_cargo_profile(&mut env, &LaneBuildSettings::default());
    assert_eq!(value(&env, "CARGO_INCREMENTAL").as_deref(), Some("1"));
    assert_eq!(env.len(), 2);
}
