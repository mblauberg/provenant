//! Conservative bounded inspection of direct harness shell commands.
//! Custody and OS confinement remain authoritative; this is no shell evaluator.
use thiserror::Error;

pub const DENY_REASON: &str =
    "Direct process control is refused. Use `provenant cancel TARGET` for owned work.";
#[derive(Debug, Error)]
#[error("{DENY_REASON}")]
pub struct ShellRefusal;

/// Refuse process-control words through paths, wrappers and quoted shell -c
/// bodies. Dynamic expansion and incomplete/overdeep syntax cannot establish a
/// safe direct command. We intentionally do not interpret scripts or aliases.
pub fn check(command: &str) -> Result<(), ShellRefusal> {
    inspect(command, 0)
}
fn inspect(command: &str, depth: usize) -> Result<(), ShellRefusal> {
    if depth > 8 || command.len() > 1024 * 1024 || command.contains('\0') {
        return Err(ShellRefusal);
    }
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut words = 0;
    for character in command.chars() {
        if escape {
            word.push(character);
            escape = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escape = true;
            continue;
        }
        if Some(character) == quote {
            quote = None;
            continue;
        }
        if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
            continue;
        }
        if quote != Some('\'') && matches!(character, '$' | '`') {
            return Err(ShellRefusal);
        }
        if quote.is_none() && (character.is_whitespace() || ";|&()<>{}".contains(character)) {
            inspect_word(&word, depth)?;
            word.clear();
            words += 1;
            if words > 16_384 {
                return Err(ShellRefusal);
            }
        } else {
            word.push(character);
        }
    }
    if quote.is_some() || escape {
        return Err(ShellRefusal);
    }
    inspect_word(&word, depth)
}
fn inspect_word(word: &str, depth: usize) -> Result<(), ShellRefusal> {
    let name = word.rsplit('/').next().unwrap_or_default();
    if matches!(
        name,
        "kill"
            | "pkill"
            | "killall"
            | "killall5"
            | "skill"
            | "fuser"
            | "killpg"
            | "pidfd_send_signal"
    ) {
        return Err(ShellRefusal);
    }
    if word
        .chars()
        .any(|c| c.is_whitespace() || ";|&()".contains(c))
    {
        inspect(word, depth + 1)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_control_never_obtains_authority_from_shell_wrapping() {
        for command in [
            "kill 123",
            "sudo /usr/bin/pkill -f claude",
            "sh -c 'ki\"\"ll -9 42'",
            "ps | xargs kill",
            "killall codex",
            "${CMD} 42",
        ] {
            assert!(check(command).is_err(), "{command}");
        }
        assert!(check("echo hook-capture && git status --short").is_ok());
    }
}
