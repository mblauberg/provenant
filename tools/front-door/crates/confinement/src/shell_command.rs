//! Conservative bounded inspection of direct harness shell commands.
//! Custody and OS confinement remain authoritative; this is no shell evaluator.
use thiserror::Error;

pub const DENY_REASON: &str =
    "Direct process control is refused. Use `provenant cancel TARGET` for owned work.";
#[derive(Debug, Error)]
#[error("{DENY_REASON}")]
pub struct ShellRefusal;

/// Refuse process control wherever bash would run it as a command: after
/// separators and keywords, behind wrappers such as `sudo` and `xargs`, in
/// `$(...)`, backticks, process substitution and unquoted heredocs, and in
/// bodies handed to shells. The same names as arguments, as in `rg pkill`,
/// stay usable. Dynamic expansion cannot establish a command name, so `$CMD`
/// in command position is refused, as is incomplete or overdeep syntax. When a
/// shell or `xargs` may read commands as data, any process-control word in the
/// line is refused. Scripts, aliases and functions are not interpreted.
pub fn check(command: &str) -> Result<(), ShellRefusal> {
    inspect(command, 0)
}
fn inspect(command: &str, depth: usize) -> Result<(), ShellRefusal> {
    if depth > 8 || command.len() > 1024 * 1024 || command.contains('\0') {
        return Err(ShellRefusal);
    }
    let mut lexer = Lexer {
        chars: command.chars().collect(),
        at: 0,
        seen: Vec::new(),
        data_reaches_shell: false,
    };
    lexer.list(depth, Until::End)?;
    if lexer.data_reaches_shell {
        for text in &lexer.seen {
            if data_names_control(text)? {
                return Err(ShellRefusal);
            }
        }
    }
    Ok(())
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
/// Commands any of whose operands may be command text; each is inspected as a
/// command line. The interpreters come first.
const SHELLS: [&str; 24] = [
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh", "source", ".", "eval",
    "trap", "su", "runuser", "ssh", "watch", "flock", "parallel", "script", "chroot", "taskset",
    "chrt", "unbuffer",
];
/// Commands that run their first operand, with the options that take a
/// separate value. The last entry stands for `find -exec` and its siblings.
const WRAPPERS: [(&str, &[&str]); 16] = [
    (
        "sudo",
        &[
            "-u",
            "-g",
            "-h",
            "-p",
            "-C",
            "-D",
            "-r",
            "-t",
            "-T",
            "-U",
            "--user",
            "--group",
            "--host",
            "--prompt",
            "--close-from",
            "--chdir",
            "--role",
            "--type",
            "--command-timeout",
            "--other-user",
        ],
    ),
    ("doas", &["-u", "-C"]),
    ("env", &["-u", "-C", "--unset", "--chdir"]),
    ("exec", &["-a"]),
    ("command", &[]),
    ("builtin", &[]),
    ("nohup", &[]),
    ("nice", &["-n", "--adjustment"]),
    ("time", &["-f", "-o", "--format", "--output"]),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
    (
        "xargs",
        &[
            "-I",
            "-L",
            "-P",
            "-a",
            "-d",
            "-E",
            "-n",
            "-s",
            "--arg-file",
            "--delimiter",
            "--eof",
            "--max-args",
            "--max-procs",
            "--max-lines",
            "--max-chars",
            "--process-slot-var",
        ],
    ),
    (
        "stdbuf",
        &["-i", "-o", "-e", "--input", "--output", "--error"],
    ),
    ("caffeinate", &["-t", "-w"]),
    ("setsid", &[]),
    (
        "ionice",
        &[
            "-c",
            "-n",
            "-p",
            "-P",
            "-u",
            "--class",
            "--classdata",
            "--pid",
            "--pgid",
            "--uid",
        ],
    ),
    ("-exec", &[]),
];
const ENV: usize = 2;
const XARGS: usize = 10;
const FIND: usize = WRAPPERS.len() - 1;
const FIND_ACTIONS: [&str; 4] = ["-exec", "-execdir", "-ok", "-okdir"];
/// Keywords after which the next word is again in command position.
const KEEP_COMMAND: [&str; 14] = [
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "esac",
    "coproc",
];
/// Keywords whose following words are names or patterns, not commands.
const START_ARGUMENTS: [&str; 4] = ["case", "for", "select", "function"];

#[derive(Clone, Copy, PartialEq)]
enum Until {
    End,
    Paren,
    Backtick,
}
#[derive(Clone, Copy)]
enum Position {
    Command,
    Wrapped(usize),
    /// The value of a wrapper option; the wrapper's operands follow.
    Value(usize),
    /// The value of `env -S`, which env splits into a command.
    Body(usize),
    Shell,
    Argument,
}
#[derive(Clone, Copy)]
enum Redirect {
    None,
    Target,
    Heredoc { strip_tabs: bool },
}
struct Heredoc {
    delimiter: String,
    quoted: bool,
    strip_tabs: bool,
}
/// Parse state of one list; `position`, `redirect` and `find` belong to the
/// current simple command.
struct Scope {
    position: Position,
    redirect: Redirect,
    find: bool,
    cases: usize,
    heredocs: Vec<Heredoc>,
}
impl Scope {
    fn reset(&mut self) {
        self.position = Position::Command;
        self.redirect = Redirect::None;
        self.find = false;
    }
}
#[derive(Default)]
struct Word {
    text: String,
    started: bool,
    quoted: bool,
    dynamic: bool,
    /// Length of the literal unquoted prefix, once quoting or expansion began.
    plain: Option<usize>,
}
impl Word {
    fn push(&mut self, character: char) {
        self.started = true;
        self.text.push(character);
    }
    fn special(&mut self) {
        self.started = true;
        self.plain.get_or_insert(self.text.len());
    }
    fn is_assignment(&self) -> bool {
        let Some(equals) = self.text.find('=') else {
            return false;
        };
        let name = self.text[..equals].trim_end_matches('+');
        let name = name.split_once('[').map_or(name, |(array, _)| array);
        self.plain.is_none_or(|plain| equals <= plain)
            && name.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
            && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    }
    /// A literal descriptor such as `2` or `{fd}` written straight before a
    /// redirection operator.
    fn is_fd(&self) -> bool {
        let named = self
            .text
            .strip_prefix('{')
            .and_then(|t| t.strip_suffix('}'));
        self.plain.is_none()
            && (!self.text.is_empty() && self.text.chars().all(|c| c.is_ascii_digit())
                || named.is_some_and(|name| {
                    !name.is_empty() && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                }))
    }
}
struct Lexer {
    chars: Vec<char>,
    at: usize,
    /// Decoded words and heredoc bodies, for the data rule.
    seen: Vec<String>,
    data_reaches_shell: bool,
}
impl Lexer {
    fn next(&mut self) -> Option<char> {
        let character = self.chars.get(self.at).copied()?;
        self.at += 1;
        Some(character)
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }
    fn eat(&mut self, character: char) -> bool {
        let found = self.peek() == Some(character);
        self.at += usize::from(found);
        found
    }
    fn list(&mut self, depth: usize, until: Until) -> Result<(), ShellRefusal> {
        if depth > 8 {
            return Err(ShellRefusal);
        }
        let mut scope = Scope {
            position: Position::Command,
            redirect: Redirect::None,
            find: false,
            cases: 0,
            heredocs: Vec::new(),
        };
        let mut word = Word::default();
        let closed = loop {
            let Some(character) = self.next() else {
                break false;
            };
            match character {
                '\\' => match self.next().ok_or(ShellRefusal)? {
                    // A backslash-newline is a line continuation: bash removes both.
                    '\n' => {}
                    escaped => {
                        word.special();
                        word.quoted = true;
                        word.push(escaped);
                    }
                },
                '\'' => {
                    word.special();
                    word.quoted = true;
                    self.single_quoted(&mut word, false)?;
                }
                '"' => self.double_quoted(&mut word, depth)?,
                '$' => self.dollar(&mut word, depth)?,
                '`' if until == Until::Backtick => break true,
                '`' => {
                    let start = self.at - 1;
                    self.substitute(&mut word, depth, Until::Backtick, start)?;
                }
                ')' if until == Until::Paren && scope.cases == 0 => break true,
                '#' if !word.started => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                '<' | '>' if self.eat('(') => {
                    let start = self.at - 2;
                    self.substitute(&mut word, depth, Until::Paren, start)?;
                }
                '<' | '>' | '&' if character != '&' || self.peek() == Some('>') => {
                    if word.is_fd() {
                        word = Word::default();
                    } else {
                        self.finish(&mut scope, &mut word, depth)?;
                    }
                    scope.redirect = self.redirect(character);
                }
                '(' => {
                    self.finish(&mut scope, &mut word, depth)?;
                    self.list(depth + 1, Until::Paren)?;
                    scope.reset();
                }
                ';' | '&' | '|' | ')' | '\n' => {
                    self.finish(&mut scope, &mut word, depth)?;
                    scope.reset();
                    if character == '\n' {
                        self.heredocs(&mut scope, depth)?;
                    }
                }
                c if c.is_whitespace() => self.finish(&mut scope, &mut word, depth)?,
                c => word.push(c),
            }
        };
        self.finish(&mut scope, &mut word, depth)?;
        if until != Until::End && !closed {
            return Err(ShellRefusal);
        }
        Ok(())
    }
    fn single_quoted(&mut self, word: &mut Word, ansi: bool) -> Result<(), ShellRefusal> {
        loop {
            match self.next().ok_or(ShellRefusal)? {
                '\'' => return Ok(()),
                '\\' if ansi => {
                    let escaped = self.next().ok_or(ShellRefusal)?;
                    word.push(escaped);
                }
                character => word.push(character),
            }
        }
    }
    fn double_quoted(&mut self, word: &mut Word, depth: usize) -> Result<(), ShellRefusal> {
        word.special();
        word.quoted = true;
        loop {
            match self.next().ok_or(ShellRefusal)? {
                '"' => return Ok(()),
                '\\' => match self.next().ok_or(ShellRefusal)? {
                    '\n' => {}
                    escaped => {
                        if !"$`\"\\".contains(escaped) {
                            word.push('\\');
                        }
                        word.push(escaped);
                    }
                },
                '$' => self.dollar(word, depth)?,
                '`' => {
                    let start = self.at - 1;
                    self.substitute(word, depth, Until::Backtick, start)?;
                }
                character => word.push(character),
            }
        }
    }
    /// An expansion after `$`. Substitutions inside it are inspected; the raw
    /// text is kept so bodies handed to shells are inspected again.
    fn dollar(&mut self, word: &mut Word, depth: usize) -> Result<(), ShellRefusal> {
        let start = self.at - 1;
        if self.eat('(') {
            return self.substitute(word, depth, Until::Paren, start);
        }
        let mut inner = Word::default();
        if self.eat('{') {
            loop {
                match self.next().ok_or(ShellRefusal)? {
                    '}' => break,
                    '\\' => {
                        self.next().ok_or(ShellRefusal)?;
                    }
                    '\'' => self.single_quoted(&mut inner, false)?,
                    '"' => self.double_quoted(&mut inner, depth)?,
                    '$' => self.dollar(&mut inner, depth)?,
                    '`' => {
                        let start = self.at - 1;
                        self.substitute(&mut inner, depth, Until::Backtick, start)?;
                    }
                    _ => {}
                }
            }
        } else if self.eat('\'') {
            self.single_quoted(&mut inner, true)?;
        }
        word.special();
        word.dynamic = true;
        word.text.extend(&self.chars[start..self.at]);
        Ok(())
    }
    fn substitute(
        &mut self,
        word: &mut Word,
        depth: usize,
        until: Until,
        start: usize,
    ) -> Result<(), ShellRefusal> {
        self.list(depth + 1, until)?;
        word.special();
        word.dynamic = true;
        word.text.extend(&self.chars[start..self.at]);
        Ok(())
    }
    fn redirect(&mut self, operator: char) -> Redirect {
        if operator == '&' {
            self.at += 1; // The `>` of `&>`.
        }
        if operator == '<' && self.eat('<') {
            if self.eat('<') {
                return Redirect::Target;
            }
            return Redirect::Heredoc {
                strip_tabs: self.eat('-'),
            };
        }
        let _ = self.eat('>') || self.eat('&') || self.eat('|');
        Redirect::Target
    }
    /// Consume the bodies of heredocs whose line just ended. A quoted
    /// delimiter keeps the body literal; otherwise its substitutions run.
    fn heredocs(&mut self, scope: &mut Scope, depth: usize) -> Result<(), ShellRefusal> {
        for heredoc in std::mem::take(&mut scope.heredocs) {
            let mut body = String::new();
            while self.at < self.chars.len() {
                let end = self.chars[self.at..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(self.chars.len(), |offset| self.at + offset);
                let line: String = self.chars[self.at..end].iter().collect();
                let compared = if heredoc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if compared == heredoc.delimiter {
                    self.at = (end + 1).min(self.chars.len());
                    break;
                }
                body.push_str(&line);
                body.push('\n');
                if heredoc.quoted {
                    self.at = (end + 1).min(self.chars.len());
                    continue;
                }
                let mut inner = Word::default();
                while let Some(character) = self.peek().filter(|&c| c != '\n') {
                    self.at += 1;
                    match character {
                        '\\' => self.at += usize::from(self.peek().is_some_and(|c| c != '\n')),
                        '$' => self.dollar(&mut inner, depth)?,
                        '`' => {
                            let start = self.at - 1;
                            self.substitute(&mut inner, depth, Until::Backtick, start)?;
                        }
                        _ => {}
                    }
                }
                self.at = (self.at + 1).min(self.chars.len());
            }
            self.seen.push(body);
        }
        Ok(())
    }
    fn finish(
        &mut self,
        scope: &mut Scope,
        word: &mut Word,
        depth: usize,
    ) -> Result<(), ShellRefusal> {
        let word = std::mem::take(word);
        if !word.started {
            return Ok(());
        }
        if self.seen.len() >= 16_384 {
            return Err(ShellRefusal);
        }
        self.seen.push(word.text.clone());
        match std::mem::replace(&mut scope.redirect, Redirect::None) {
            Redirect::Target => return Ok(()),
            Redirect::Heredoc { strip_tabs } => {
                scope.heredocs.push(Heredoc {
                    delimiter: word.text,
                    quoted: word.quoted,
                    strip_tabs,
                });
                return Ok(());
            }
            Redirect::None => {}
        }
        let text = word.text.as_str();
        let keyword = |list: &[&str]| word.plain.is_none() && list.contains(&text);
        scope.position = match scope.position {
            Position::Command if word.is_assignment() => Position::Command,
            Position::Command if keyword(&KEEP_COMMAND) => {
                if text == "esac" {
                    scope.cases = scope.cases.saturating_sub(1);
                }
                Position::Command
            }
            Position::Command if keyword(&START_ARGUMENTS) => {
                scope.cases += usize::from(text == "case");
                Position::Argument
            }
            Position::Wrapped(wrapper)
                if text == "--"
                    || word.plain.is_none() && duration(text)
                    || wrapper == ENV && word.is_assignment() =>
            {
                Position::Wrapped(wrapper)
            }
            Position::Wrapped(ENV) if text == "-S" || text == "--split-string" => {
                Position::Body(ENV)
            }
            Position::Wrapped(wrapper) if text.starts_with('-') => {
                let split = text
                    .strip_prefix("--split-string=")
                    .or_else(|| text.strip_prefix("-S"));
                if let Some(body) = split.filter(|_| wrapper == ENV) {
                    inspect(body, depth + 1)?;
                }
                if WRAPPERS[wrapper].1.contains(&text) {
                    Position::Value(wrapper)
                } else {
                    Position::Wrapped(wrapper)
                }
            }
            Position::Value(wrapper) => Position::Wrapped(wrapper),
            Position::Body(wrapper) => {
                inspect(text, depth + 1)?;
                Position::Wrapped(wrapper)
            }
            Position::Shell => {
                inspect(text, depth + 1)?;
                Position::Shell
            }
            Position::Argument if scope.find && FIND_ACTIONS.contains(&text) => {
                Position::Wrapped(FIND)
            }
            Position::Argument => Position::Argument,
            Position::Command | Position::Wrapped(_) => self.command(scope, &word)?,
        };
        Ok(())
    }
    /// Classify a command word: refuse process control, follow wrappers and
    /// shells. Brace and glob forms count when any expansion could match.
    fn command(&mut self, scope: &mut Scope, word: &Word) -> Result<Position, ShellRefusal> {
        if word.dynamic {
            return Err(ShellRefusal);
        }
        let mut expansions = Vec::new();
        expand_braces(&word.text, &mut expansions)?;
        let names: Vec<&str> = expansions
            .iter()
            .map(|expansion| expansion.rsplit('/').next().unwrap_or_default())
            .collect();
        let named = |list: &[&str]| {
            list.iter()
                .position(|item| names.iter().any(|name| may_name(name, item)))
        };
        if named(&PROCESS_CONTROL).is_some() {
            return Err(ShellRefusal);
        }
        if named(&SHELLS).is_some() {
            self.data_reaches_shell = true;
            return Ok(Position::Shell);
        }
        if let Some(wrapper) = named(&WRAPPERS.map(|(name, _)| name)) {
            self.data_reaches_shell |= wrapper == XARGS;
            return Ok(Position::Wrapped(wrapper));
        }
        scope.find |= named(&["find"]).is_some();
        Ok(Position::Argument)
    }
}
/// Durations and counts such as `timeout 5s` stay in wrapper position.
fn duration(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ".smhd".contains(c))
}
/// Text that may reach a shell or `xargs` as data, such as `echo kill 1 | sh`,
/// is checked for any process-control word, quotes removed.
fn data_names_control(text: &str) -> Result<bool, ShellRefusal> {
    let text: String = text.chars().filter(|c| !"'\"\\".contains(*c)).collect();
    for word in text.split(|c: char| c.is_whitespace() || ";|&()<>`$".contains(c)) {
        let mut expansions = Vec::new();
        expand_braces(word, &mut expansions)?;
        for expansion in &expansions {
            let name = expansion.rsplit('/').next().unwrap_or_default();
            if PROCESS_CONTROL
                .iter()
                .any(|control| may_name(name, control))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
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
    #[test]
    fn every_command_position_is_inspected() {
        for command in [
            "echo $(kill 1)",
            "echo \"`pkill x`\"",
            "cat <(kill 1)",
            "echo \"${x:-$(kill 1)}\"",
            "sudo -u root kill 1",
            "sudo --user root kill 1",
            "env FOO=1 kill 1",
            "env -i kill 1",
            "env -S 'kill 1'",
            "{env,-i} kill 1",
            "timeout 5 kill 1",
            "timeout -s TERM 5s kill 1",
            "nice -n 5 kill 1",
            "xargs -I {} kill {}",
            "X=1 kill 1",
            "X+=1 kill 1",
            ">out kill 1",
            "true &>/dev/null; kill 1",
            "echo ok\nkill 1",
            "find . -name x -exec kill {} \\;",
            "find . -exec true \\; -exec kill {} \\;",
            "if true; then kill 1; fi",
            "for p in 1; do kill $p; done",
            "(kill 1)",
            "{ kill 1; }",
            "f() { kill 1; }",
            "case x in a) kill 1;; esac",
            "echo \"$(case x in a) kill 1;; esac)\"",
            "bash -lc 'echo ok; kill 1'",
            "eval 'kill 1'",
            "su -c 'kill 1'",
            "trap 'kill 1' EXIT",
            "cat <<EOF\n$(kill 1)\nEOF",
            "echo \"$(cat <<EOF\n`kill 1`\nEOF\n)\"",
            "$CMD 1",
            "\"$HOME/bin/tool\" 1",
            "$(which tool) 1",
            "echo kill 1 | sh",
            "bash <<'EOF'\nkill 1\nEOF",
            "printf 'ki\"\"ll 1' | bash",
            "echo $(",
            "echo \"open",
        ] {
            assert!(check(command).is_err(), "{command}");
        }
    }
    #[test]
    fn arguments_and_ordinary_expansion_stay_usable() {
        for command in [
            "rg pkill src",
            "echo kill",
            "man kill",
            "git commit -m \"handle kill signal\"",
            "export PATH=\"$HOME/.cargo/bin:$PATH\"",
            "PATH=\"$HOME/bin:$PATH\" cargo test",
            "for f in *.rs; do echo \"$f\"; done",
            "git commit -m \"$(cat <<'EOF'\nStop the kill path (don't kill it)\nEOF\n)\"",
            "cat <<EOF\nkill 1 $HOME\nEOF",
            "cargo test 2>&1 | tail -5",
            "grep -n kill src/*.rs # kill",
            "echo $HOME",
            "ls > out.txt 2>/dev/null",
            "timeout 60 cargo test kill_handler",
            "xargs -I{} cp {} \"$DEST\"",
            "find . -name kill -print",
            "[ -f x ] && echo ok",
            "while read -r line; do echo \"$line\"; done < file",
            "echo 'single $(kill 1)'",
            "bash -c 'cargo test'",
            "sudo -u \"$USER\" make",
            "echo $((1 + (2 * 3)))",
            "VAR=$(git rev-parse HEAD) && echo \"$VAR\"",
        ] {
            assert!(check(command).is_ok(), "{command}");
        }
    }
}
