use super::*;

#[cfg(test)]
#[test]
fn output_prompt_directs_scratch_outside_worktree() {
    let prompt = output_prompt(std::path::Path::new("/attempt"), "task");
    assert!(prompt.contains("$TMPDIR"));
    assert!(prompt.contains("not in the worktree"));
}

/// The launch prompt teaches the declaration command and keeps harness and
/// lane ownership distinct: settlement seals the snapshot from declarations,
/// never from a progress file or the answer text.
#[cfg(test)]
#[test]
fn output_prompt_teaches_the_declaration_command_and_host_ownership() {
    let prompt = output_prompt(std::path::Path::new("/attempt"), "task");
    assert!(prompt.contains("$TMPDIR"));
    assert!(prompt.contains("not in the worktree"));
    assert!(
        prompt.contains("Declare your result with `provenant declare --stdin`"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Field errors come back at once"),
        "{prompt}"
    );
    assert!(
        prompt.contains("prose and fenced blocks are not read"),
        "{prompt}"
    );
    assert!(
        prompt.contains("harness writes the attempt's own records"),
        "{prompt}"
    );
    assert!(
        prompt.contains("sealed handoff.json in the attempt directory"),
        "{prompt}"
    );
    assert!(prompt.contains("the lane never writes them"), "{prompt}");
    assert!(
        !prompt.contains("your progress handoff JSON under provider-state"),
        "{prompt}"
    );
    assert!(!prompt.contains("Progress handoff JSON:"), "{prompt}");
}
