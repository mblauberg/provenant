use super::*;

#[test]
fn shared_submit_context_refuses_depth_above_the_limit() {
    let parent = AttemptId::from_bytes([7; 16]).to_string();
    let state = Path::new("/state");
    let result = lane_dispatch_values(Some("2"), Some(&parent), 2, state, Some(state));
    let Err(failure) = result else {
        panic!("depth above the configured maximum must be refused");
    };
    assert_eq!(failure.status, Status::Refused);
    assert_eq!(failure.kind.code(), "LANE_DEPTH");
}

#[test]
fn shared_submit_context_assigns_root_depth_and_parent() {
    let parent = AttemptId::from_bytes([8; 16]);
    let parent_text = parent.to_string();
    let state = Path::new("/state");
    let result = lane_dispatch_values(Some("1"), Some(&parent_text), 2, state, Some(state));
    assert!(matches!(result, Ok((2, Some(found))) if found == parent));
}

#[test]
fn shared_submit_context_refuses_incomplete_or_zero_nested_context() {
    let parent = AttemptId::from_bytes([9; 16]).to_string();
    let state = Path::new("/state");
    let failure = lane_dispatch_values(
        Some("1"),
        Some(&parent),
        3,
        state,
        Some(Path::new("/state/children/old")),
    )
    .expect_err("a retired child-store marker must be refused");
    assert_eq!(failure.kind.code(), "LANE_CONTEXT");
    for (depth, parent_attempt) in [
        (None, None),
        (Some("1"), None),
        (None, Some(parent.as_str())),
    ] {
        let failure = lane_dispatch_values(depth, parent_attempt, 3, state, Some(state))
            .expect_err("stripped or forged lineage must be refused");
        assert_eq!(failure.kind.code(), "LANE_CONTEXT");
    }
    for (depth, parent_attempt) in [
        (Some("1"), None),
        (None, Some(parent.as_str())),
        (Some("0"), Some(parent.as_str())),
    ] {
        let failure = lane_dispatch_values(depth, parent_attempt, 2, Path::new("/state"), None)
            .expect_err("incomplete or zero nested context must be refused");
        assert_eq!(failure.status, Status::Refused);
        assert_eq!(failure.kind.code(), "LANE_CONTEXT");
    }
}

#[test]
fn shared_submit_context_assigns_root_depth_without_parent() {
    assert!(matches!(
        lane_dispatch_values(None, None, 2, Path::new("/state"), None),
        Ok((1, None))
    ));
}
