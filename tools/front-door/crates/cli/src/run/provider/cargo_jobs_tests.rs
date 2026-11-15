use super::{set_cargo_jobs, set_cargo_profile};
use provenant_config::LaneBuildSettings;

#[test]
fn supplied_environment_takes_precedence_for_lane_build_settings() {
    type Env = Vec<(std::ffi::OsString, std::ffi::OsString)>;
    let configure_profile: fn(&mut Env) = |env| {
        set_cargo_profile(env, &LaneBuildSettings::default());
    };
    let configure_jobs: fn(&mut Env) = |env| set_cargo_jobs(env, Some("9"), 3);
    let cases: [(&str, &str, Env, fn(&mut Env)); 2] = [
        (
            "CARGO_INCREMENTAL",
            "1",
            vec![("CARGO_INCREMENTAL".into(), "1".into())],
            configure_profile,
        ),
        (
            "CARGO_BUILD_JOBS",
            "7",
            vec![("CARGO_BUILD_JOBS".into(), "7".into())],
            configure_jobs,
        ),
    ];

    for (name, supplied, mut env, configure) in cases {
        configure(&mut env);
        let observed = env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string_lossy());
        assert_eq!(observed.as_deref(), Some(supplied));
    }
}
