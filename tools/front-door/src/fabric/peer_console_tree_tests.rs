use super::*;

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn peer_console_tree_preserves_the_requested_subtree_for_local_selection() {
    let command = console_tree_command(7, 42, true, Some("attempt-root".into()));
    assert!(matches!(command,
        provenant_cli::console::Console::Tree {
            max_depth: 7,
            max_nodes: 42,
            detail: true,
            hosts: None,
            subtree: Some(root),
        } if root == "attempt-root"));
}
