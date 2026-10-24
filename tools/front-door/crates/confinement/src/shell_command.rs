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
            // A backslash-newline is a line continuation: bash removes both.
            if character != '\n' {
                word.push(character);
            }
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
        if quote.is_none() && (character.is_whitespace() || ";|&()<>".contains(character)) {
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
const PROCESS_CONTROL: [&str; 8] = [
    "kill",
    "pkill",
    "killall",
    "killall5",
    "skill",
    "fuser",
    "killpg",
    "pidfd_send_signal",
];
fn inspect_word(word: &str, depth: usize) -> Result<(), ShellRefusal> {
    let mut expansions = Vec::new();
    expand_braces(word, &mut expansions)?;
    for expansion in &expansions {
        let name = expansion.rsplit('/').next().unwrap_or_default();
        if PROCESS_CONTROL
            .iter()
            .any(|control| may_name(name, control))
        {
            return Err(ShellRefusal);
        }
    }
    if word
        .chars()
        .any(|c| c.is_whitespace() || ";|&()".contains(c))
    {
        inspect(word, depth + 1)?;
    }
    Ok(())
}
/// Bash brace expansion of one word, bounded. Braces without a top-level
/// comma stay literal, as in bash.
fn expand_braces(word: &str, out: &mut Vec<String>) -> Result<(), ShellRefusal> {
    if out.len() > 256 {
        return Err(ShellRefusal);
    }
    let bytes = word.as_bytes();
    for open in (0..bytes.len()).filter(|&i| bytes[i] == b'{') {
        let (mut nesting, mut commas) = (0, vec![]);
        for close in open..bytes.len() {
            match bytes[close] {
                b'{' => nesting += 1,
                b',' if nesting == 1 => commas.push(close),
                b'}' => {
                    nesting -= 1;
                    if nesting == 0 && !commas.is_empty() {
                        let bounds = std::iter::once(open)
                            .chain(commas.iter().copied())
                            .chain(std::iter::once(close));
                        let bounds: Vec<_> = bounds.collect();
                        for pair in bounds.windows(2) {
                            let alternative = format!(
                                "{}{}{}",
                                &word[..open],
                                &word[pair[0] + 1..pair[1]],
                                &word[close + 1..]
                            );
                            expand_braces(&alternative, out)?;
                        }
                        return Ok(());
                    }
                    if nesting == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out.push(word.to_owned());
    Ok(())
}
/// Whether the glob `name` may match `control`: `?` matches one character,
/// `[...]` one character of its set (negated sets match any), `*` any text.
/// A name of only `*` and `?`, such as `*`, is not refused, so ordinary glob
/// arguments stay usable.
fn may_name(name: &str, control: &str) -> bool {
    if !name.contains(['*', '?', '[']) {
        return name == control;
    }
    let mut pattern = Vec::new();
    let mut characters = name.chars();
    while let Some(character) = characters.next() {
        pattern.push(match character {
            '?' => Token::One(None),
            '*' => Token::Any,
            '[' if name.contains(']') => {
                let set: String = characters
                    .by_ref()
                    .take_while(|&inner| inner != ']')
                    .collect();
                if set.starts_with(['!', '^']) || set.contains('-') {
                    Token::One(None)
                } else {
                    Token::One(Some(set))
                }
            }
            literal => Token::One(Some(literal.to_string())),
        });
    }
    pattern
        .iter()
        .any(|token| matches!(token, Token::One(Some(_))))
        && matches_glob(&pattern, &control.chars().collect::<Vec<_>>())
}
enum Token {
    /// One character, from the set when one is known.
    One(Option<String>),
    Any,
}
fn matches_glob(pattern: &[Token], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((Token::Any, rest)) => (0..=text.len()).any(|skip| matches_glob(rest, &text[skip..])),
        Some((Token::One(set), rest)) => text.split_first().is_some_and(|(first, tail)| {
            set.as_ref().is_none_or(|set| set.contains(*first)) && matches_glob(rest, tail)
        }),
    }
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
            "ki\\\nll 1",
            "k?ll 1",
            "/bin/k[i]ll 1",
            "k{i,}ll 1",
            "pk*l 1",
            "/bin/[k][i][l][l] 1",
            "{kill,x} 1",
            "/bin/[k]* 1",
        ] {
            assert!(check(command).is_err(), "{command}");
        }
        for command in [
            "echo hook-capture && git status --short",
            "ls *.rs src/[ab]* {a,b}.txt",
            "rm -f build/* && echo done",
            "cp a{,.bak} && mkdir -p src/{a,b}",
        ] {
            assert!(check(command).is_ok(), "{command}");
        }
    }
}
