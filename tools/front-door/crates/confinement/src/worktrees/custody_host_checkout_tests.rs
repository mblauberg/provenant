use super::*;

const MOVED: &[&str] = &["refs/heads/lane", "refs/heads/published/lane"];

#[test]
fn unrelated_host_operation_does_not_block_a_linked_lane() {
    assert!(!host_operation_blocks(
        false,
        Some("refs/heads/side"),
        "refs/heads/lane",
        MOVED
    ));
    // An unknown ref (detached HEAD, unreadable head-name) is not a moved
    // ref, so it cannot refuse settlement on its own.
    assert!(!host_operation_blocks(
        false,
        None,
        "refs/heads/lane",
        MOVED
    ));
}

#[test]
fn host_operation_on_a_moved_ref_blocks_a_linked_lane() {
    assert!(host_operation_blocks(
        false,
        Some("refs/heads/lane"),
        "refs/heads/lane",
        MOVED
    ));
    assert!(host_operation_blocks(
        false,
        Some("refs/heads/published/lane"),
        "refs/heads/lane",
        MOVED
    ));
}

#[test]
fn primary_checkout_keeps_the_lanes_own_operation_without_refusing() {
    // The lane's worktree is the primary checkout, so this is its own Git:
    // a stopped rebase or merge of its branch warns and proceeds with the
    // committed tip rather than refusing and hiding lane_git_operation_warning.
    assert!(!host_operation_blocks(
        true,
        Some("refs/heads/lane"),
        "refs/heads/lane",
        MOVED
    ));
    assert!(!host_operation_blocks(
        true,
        Some("refs/heads/side"),
        "refs/heads/lane",
        MOVED
    ));
    assert!(!host_operation_blocks(true, None, "refs/heads/lane", MOVED));
}

#[test]
fn primary_checkout_operation_on_the_delivery_destination_blocks() {
    // The one ref settlement moves that is not the lane's own in-progress
    // operation: publication would move it underneath that operation.
    assert!(host_operation_blocks(
        true,
        Some("refs/heads/published/lane"),
        "refs/heads/lane",
        MOVED
    ));
}
