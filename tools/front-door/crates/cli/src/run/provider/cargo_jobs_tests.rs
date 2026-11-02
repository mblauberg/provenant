
use super::set_cargo_jobs;
#[test]
fn cargo_jobs_respect_supplied_environment() {
    let mut env = vec![("CARGO_BUILD_JOBS".into(), "7".into())];
    set_cargo_jobs(&mut env, Some("9"), 3);
    assert_eq!(env[0].1, "7");
    let mut env = Vec::new();
    set_cargo_jobs(&mut env, Some("9"), 3);
    assert_eq!(env[0].1, "9");
    let mut env = Vec::new();
    set_cargo_jobs(&mut env, None, 3);
    assert_eq!(env[0].1, "3");
}
