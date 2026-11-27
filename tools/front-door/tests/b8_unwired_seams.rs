//! B8 invariants: the batch's new public seams have no dead-code allows, and
//! `LaunchTimings` has one owner. Spec: docs/specs/agent-native.md §3.4 and §4.

const RECLAIM: &str = include_str!("../crates/run/src/reclaim.rs");
const LAUNCH: &str = include_str!("../crates/run/src/launch.rs");
const CARRY: &str = include_str!("../crates/run/src/worktree_carry.rs");
const OBSERVER: &str = include_str!("../src/pr/observer.rs");
const RUN_MODEL: &str = include_str!("../crates/fabric-runtime/src/graph/model.rs");
const RUN_LIB: &str = include_str!("../crates/run/src/lib.rs");

#[test]
fn batch_new_public_types_have_no_dead_code_allows() {
    for (name, source) in [
        ("reclaim", RECLAIM),
        ("launch", LAUNCH),
        ("worktree_carry", CARRY),
        ("observer", OBSERVER),
    ] {
        assert!(
            !source.contains("allow(dead_code"),
            "{name} reintroduced a dead-code allow on a batch seam"
        );
    }
}

#[test]
fn launch_timings_has_one_owner() {
    assert!(
        !LAUNCH.contains("struct LaunchTimings"),
        "the run crate must not redeclare LaunchTimings"
    );
    assert!(
        RUN_MODEL.contains("pub struct LaunchTimings"),
        "fabric-runtime owns the single LaunchTimings"
    );
    assert!(
        !RUN_LIB.contains("LaunchTimings"),
        "the run crate must not re-export a type it does not own"
    );
}
