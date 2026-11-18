//! Public CLI lifecycle, including output acknowledgement and explicit replay.
mod support;
use support::{Result, TestRig, call, git};

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn notes_cli_persists_delta_cursors_and_replays_without_rewriting() -> Result {
    let rig = TestRig::new()?;
    git(&rig, &["commit", "--allow-empty", "-qm", "seed"])?;
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    let first = call(
        &rig,
        &[
            "notes",
            "add",
            "--tag",
            "custom/interface",
            "--scope",
            "g/root",
            "--",
            "interface ready",
        ],
        0,
    )?;
    let provenance = &first["data"]["detail"]["provenance"];
    assert_eq!(provenance["verified"], false);
    assert_eq!(provenance["commit"], head.trim());
    assert!(provenance["lane"].is_null());
    assert!(!provenance["time"].is_null());
    let second = call(
        &rig,
        &[
            "notes",
            "add",
            "--scope",
            "g/root--child",
            "--resolves",
            "1",
            "--",
            "correction",
        ],
        0,
    )?;
    assert_eq!(second["data"]["detail"]["resolves"], 1);
    let read = |extra: &[&str]| {
        let mut args = vec![
            "notes",
            "read",
            "--reader",
            "r",
            "--graph",
            "g",
            "--subtree",
            "root",
        ];
        args.extend_from_slice(extra);
        call(&rig, &args, 0)
    };
    let first_page = read(&["--limit", "1"])?;
    assert_eq!(
        first_page["data"]["events"]
            .as_array()
            .ok_or("events")?
            .len(),
        1
    );
    assert_eq!(first_page["data"]["truncated"], true);
    assert_eq!(first_page["data"]["read_cap"]["source"], "launch");
    // Replay has no effect on the saved delta cursor.
    let replay = read(&["--replay"])?;
    assert_eq!(
        replay["data"]["events"].as_array().ok_or("events")?.len(),
        2
    );
    let next = read(&[])?;
    assert_eq!(next["data"]["events"][0]["id"], second["data"]["id"]);
    assert_eq!(next["data"]["events"].as_array().ok_or("events")?.len(), 1);
    assert_eq!(
        read(&[])?["data"]["events"]
            .as_array()
            .ok_or("events")?
            .len(),
        0
    );
    let replayed = read(&["--replay"])?;
    assert_eq!(replayed["data"]["events"][0], first["data"]);
    Ok(())
}
