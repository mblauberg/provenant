//! Invariant: every refusal renders the concrete next step its owner supplied,
//! and the renderer never substitutes a circular `help` command for one.
use crate::support::Result;
use provenant_cli::{RenderMode, render};
use provenant_records::{CommandError, CommandResult, Outcome};

#[test]
// Invariant: a refusal's repair is emitted verbatim in every render mode.
fn a_refusal_renders_its_repair_and_never_a_help_fallback() -> Result {
    let repair = "run `provenant check config --json` and correct the accounts configuration";
    let mut result = CommandResult::new("quota show", Outcome::Error, ());
    result.error = Some(CommandError::new(
        "QUOTA_CONFIG",
        "invalid quota accounts configuration",
        None,
        repair,
    ));
    // The constructor is the only way to build a CommandError outside the
    // records crate, so the repair is always present before rendering.
    assert_eq!(
        result
            .error
            .as_ref()
            .and_then(|error| error.repair.as_deref()),
        Some(repair)
    );
    let modes = [
        RenderMode::Json,
        RenderMode::Human {
            text: "",
            read_only: false,
            summary: None,
        },
        RenderMode::Compact {
            text: "",
            read_only: false,
            summary: None,
        },
    ];
    for mode in modes {
        let mut output = Vec::new();
        render(&result, mode, &mut output)?;
        let text = String::from_utf8(output)?;
        assert!(text.contains(repair), "{text}");
        assert!(!text.contains("provenant help"), "{text}");
    }
    Ok(())
}
