//! The graph stack size is one named constant, shared with the test harness.
use super::GRAPH_STACK_BYTES;

// Spec: docs/specs/rust-workspace.md#bounded-debug-stack-frames
#[test]
fn test_harness_threads_get_the_graph_stack_size() -> Result<(), Box<dyn std::error::Error>> {
    let config = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cargo/config.toml"),
    )?;
    let configured = config
        .lines()
        .filter_map(|line| line.trim().strip_prefix("RUST_MIN_STACK"))
        .find_map(|rest| rest.trim().strip_prefix('='))
        .ok_or("tools/front-door/.cargo/config.toml sets no RUST_MIN_STACK")?
        .trim()
        .trim_matches(['"', '\''])
        .parse::<usize>()?;
    assert_eq!(configured, GRAPH_STACK_BYTES);
    Ok(())
}
