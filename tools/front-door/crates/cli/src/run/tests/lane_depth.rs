use super::*;

#[test]
fn shared_submit_context_uses_depth_lineage_and_state_as_one_contract() {
    enum Expected {
        Success(u64, Option<AttemptId>),
        Failure(Status, ErrorKind),
    }
    let state = Path::new("/state");
    let child_state = Path::new("/state/children/parent");
    let parent = AttemptId::from_bytes([7; 16]);
    let parent_text = parent.to_string();
    let cases = [
        (None, None, 2, None, Expected::Success(1, None)),
        (
            Some("1"),
            Some(parent_text.as_str()),
            2,
            None,
            Expected::Success(2, Some(parent)),
        ),
        (
            Some("2"),
            Some(parent_text.as_str()),
            2,
            Some(state),
            Expected::Failure(Status::Refused, ErrorKind::LaneDepth),
        ),
        (
            None,
            None,
            2,
            Some(state),
            Expected::Failure(Status::Refused, ErrorKind::LaneContext),
        ),
        (
            Some("1"),
            None,
            2,
            Some(state),
            Expected::Failure(Status::Refused, ErrorKind::LaneContext),
        ),
        (
            None,
            Some(parent_text.as_str()),
            2,
            Some(state),
            Expected::Failure(Status::Refused, ErrorKind::LaneContext),
        ),
        (
            Some("1"),
            Some(parent_text.as_str()),
            2,
            Some(child_state),
            Expected::Failure(Status::Refused, ErrorKind::LaneContext),
        ),
        (
            Some("0"),
            Some(parent_text.as_str()),
            2,
            None,
            Expected::Failure(Status::Refused, ErrorKind::LaneContext),
        ),
        (
            Some("invalid"),
            Some(parent_text.as_str()),
            3,
            None,
            Expected::Failure(Status::Invalid, ErrorKind::LaneContext),
        ),
        (
            None,
            Some("invalid"),
            3,
            None,
            Expected::Failure(Status::Invalid, ErrorKind::ParentAttempt),
        ),
    ];
    for (depth, parent_attempt, max_depth, run_state, expected) in cases {
        let actual = lane_dispatch_values(depth, parent_attempt, max_depth, state, run_state);
        match expected {
            Expected::Success(depth, parent) => assert_eq!(actual.unwrap(), (depth, parent)),
            Expected::Failure(status, kind) => {
                let failure = actual.expect_err("invalid lane context must be refused");
                assert_eq!(failure.status, status);
                assert_eq!(failure.kind, kind);
            }
        }
    }
}
