//! Conservative bounded inspection of direct harness shell commands.
//! Custody and OS confinement remain authoritative; this is no shell evaluator.
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    ProcessControl,
    DynamicCommandName,
    Expansion,
    Unparseable,
}

pub const DENY_REASON: &str = "Pattern and group process control is refused; targeted `kill [-SIGNAL] PID` with literal PIDs above 1 is allowed. Use `provenant cancel TARGET` for owned work.";

impl RefusalReason {
    pub const fn message(self) -> &'static str {
        match self {
            Self::ProcessControl => DENY_REASON,
            Self::DynamicCommandName => {
                "A dynamic command name is refused. Write the command path literally."
            }
            Self::Expansion => {
                "An unbounded expansion is refused. Write the command name literally."
            }
            Self::Unparseable => {
                "Shell syntax the guard cannot parse is refused. Simplify the command."
            }
        }
    }
}

#[derive(Debug, Error)]
#[error("{}", .0.message())]
pub struct ShellRefusal(pub RefusalReason);

impl ShellRefusal {
    const fn unparseable() -> Self {
        Self(RefusalReason::Unparseable)
    }
    const fn expansion() -> Self {
        Self(RefusalReason::Expansion)
    }
}

/// Refuse process control wherever bash would run it as a command: after
/// separators and keywords, behind wrappers such as `sudo` and `xargs`, in
/// `$(...)`, backticks, process substitution and unquoted heredocs, and in
/// bodies handed to shells. The same names as arguments, as in `rg pkill`,
/// stay usable. Dynamic expansion cannot establish a command name, so `$CMD`
/// in command position is refused, as is incomplete or overdeep syntax. When an
/// interpreter reads commands from stdin, as in `echo kill 1 | sh`, any
/// process-control word in the line is refused. `kill` with an optional
/// signal and literal PIDs above 1 is allowed, as are `kill -0` probes. Scripts, aliases, functions and other languages are not interpreted.
pub fn check(command: &str) -> Result<(), ShellRefusal> {
    inspect(command, 0).map(drop)
}
/// The simple commands the same walk finds, as words with the command name
/// first. Wrappers and shells are looked through, so `sudo git stash` and
/// `bash -c 'git stash'` both yield `git stash`. Fails as `check` does.
pub fn commands(command: &str) -> Result<Vec<Vec<String>>, ShellRefusal> {
    inspect(command, 0)
}
fn inspect(command: &str, depth: usize) -> Result<Vec<Vec<String>>, ShellRefusal> {
    if depth > 8 || command.len() > 1024 * 1024 || command.contains('\0') {
        return Err(ShellRefusal::unparseable());
    }
    let mut lexer = Lexer {
        chars: command.chars().collect(),
        at: 0,
        seen: Vec::new(),
        commands: Vec::new(),
        data_reaches_shell: false,
    };
    lexer.list(depth, Until::End)?;
    if lexer.data_reaches_shell {
        for text in &lexer.seen {
            if data_names_control(text)? {
                return Err(ShellRefusal(RefusalReason::ProcessControl));
            }
        }
    }
    Ok(lexer.commands)
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
/// command line. The first `INTERPRETERS` read commands from stdin without one.
const INTERPRETERS: usize = 11;
const SHELLS: [&str; 24] = [
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh", "source", ".", "eval",
    "trap", "su", "runuser", "ssh", "watch", "flock", "parallel", "script", "chroot", "taskset",
    "chrt", "unbuffer",
];
/// Commands that run their first operand, with the options that take a
/// separate value. The last entry stands for `find -exec` and its siblings.
const WRAPPERS: [(&str, &[&str]); 18] = [
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
            "-R",
            "-c",
            "-a",
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
    ("env", &["-u", "-C", "-P", "--unset", "--chdir"]),
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
    ("gtimeout", &["-s", "-k", "--signal", "--kill-after"]),
    ("arch", &["-arch", "-e"]),
    ("-exec", &[]),
];
const ENV: usize = 2;
const COMMAND: usize = 4;
const XARGS: usize = 10;
const FIND: usize = WRAPPERS.len() - 1;
const FIND_ACTIONS: [&str; 4] = ["-exec", "-execdir", "-ok", "-okdir"];
/// Keywords after which the next word is again in command position.
const KEEP_COMMAND: [&str; 14] = [
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "esac",
    "coproc",
];
/// Keywords whose following words are names or patterns, not commands.
const START_ARGUMENTS: [&str; 3] = ["case", "for", "select"];

#[derive(Clone, Copy, PartialEq)]
enum Until {
    End,
    Paren,
    Backtick,
}
#[derive(Clone, Copy, Default)]
enum Position {
    #[default]
    Command,
    Wrapped(usize),
    /// The value of a wrapper option; the wrapper's operands follow.
    Value(usize),
    /// The value of `env -S`, which env splits into a command.
    Body(usize),
    Shell,
    Argument,
    /// The name after `function`; the body follows in command position.
    Name,
    /// After a literal `kill`: an optional signal, then literal PIDs above 1.
    Kill,
    /// The value of `kill -s` or `-n`.
    KillSignal,
    /// After at least one PID, which only further PIDs may follow.
    KillPids,
}
#[derive(Clone, Copy, Default)]
enum Redirect {
    #[default]
    None,
    Target,
    Heredoc {
        strip_tabs: bool,
    },
}
struct Heredoc {
    delimiter: String,
    quoted: bool,
    strip_tabs: bool,
}
/// Parse state of one list; all but `cases` and `heredocs` belong to the
/// current simple command.
#[derive(Default)]
struct Scope {
    position: Position,
    redirect: Redirect,
    find: bool,
    /// An interpreter's operand count, and whether one was dynamic.
    interpreter: Option<(usize, bool)>,
    cases: usize,
    heredocs: Vec<Heredoc>,
    /// The words of the current simple command, once its name is known.
    argv: Vec<String>,
    /// A shell's `-c` was seen, and its body (the one command text) was taken.
    shell_c: bool,
    shell_body: bool,
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
    /// Finished simple commands, including those inside shell bodies.
    commands: Vec<Vec<String>>,
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
            return Err(ShellRefusal::unparseable());
        }
        let mut scope = Scope::default();
        let mut word = Word::default();
        let closed = loop {
            let Some(character) = self.next() else {
                break false;
            };
            match character {
                '\\' => match self.next().ok_or_else(ShellRefusal::unparseable)? {
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
                '$' => self.dollar(&mut word, depth, false)?,
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
                    self.end_command(&mut scope)?;
                    self.list(depth + 1, Until::Paren)?;
                }
                ';' | '&' | '|' | ')' | '\n' => {
                    self.finish(&mut scope, &mut word, depth)?;
                    self.end_command(&mut scope)?;
                    if character == '\n' {
                        self.heredocs(&mut scope, depth)?;
                    }
                }
                c if c.is_whitespace() => self.finish(&mut scope, &mut word, depth)?,
                c => word.push(c),
            }
        };
        self.finish(&mut scope, &mut word, depth)?;
        self.end_command(&mut scope)?;
        if until != Until::End && !closed {
            return Err(ShellRefusal::unparseable());
        }
        Ok(())
    }
    fn single_quoted(&mut self, word: &mut Word, ansi: bool) -> Result<(), ShellRefusal> {
        loop {
            match self.next().ok_or_else(ShellRefusal::unparseable)? {
                '\'' => return Ok(()),
                '\\' if ansi => {
                    let escaped = self.next().ok_or_else(ShellRefusal::unparseable)?;
                    word.push(escaped);
                }
                character => word.push(character),
            }
        }
    }
    /// Close a simple command: a `kill` without PIDs is refused, and an interpreter
    /// without a literal operand reads commands from stdin.
    fn end_command(&mut self, scope: &mut Scope) -> Result<(), ShellRefusal> {
        if matches!(scope.position, Position::Kill | Position::KillSignal) {
            return Err(ShellRefusal(RefusalReason::ProcessControl));
        }
        if let Some((operands, dynamic)) = scope.interpreter {
            self.data_reaches_shell |= operands == 0 || dynamic;
        }
        if !scope.argv.is_empty() {
            self.commands.push(std::mem::take(&mut scope.argv));
        }
        let (cases, heredocs) = (scope.cases, std::mem::take(&mut scope.heredocs));
        *scope = Scope {
            cases,
            heredocs,
            ..Scope::default()
        };
        Ok(())
    }
    fn double_quoted(&mut self, word: &mut Word, depth: usize) -> Result<(), ShellRefusal> {
        if depth > 8 {
            return Err(ShellRefusal::unparseable());
        }
        word.special();
        word.quoted = true;
        loop {
            match self.next().ok_or_else(ShellRefusal::unparseable)? {
                '"' => return Ok(()),
                '\\' => match self.next().ok_or_else(ShellRefusal::unparseable)? {
                    '\n' => {}
                    escaped => {
                        if !"$`\"\\".contains(escaped) {
                            word.push('\\');
                        }
                        word.push(escaped);
                    }
                },
                '$' => self.dollar(word, depth, true)?,
                '`' => {
                    let start = self.at - 1;
                    self.substitute(word, depth, Until::Backtick, start)?;
                }
                character => word.push(character),
            }
        }
    }
    /// An expansion after `$`. Substitutions inside it are inspected; the raw
    /// text is kept so bodies handed to shells are inspected again. Inside
    /// double quotes, single quotes in `${...}` are literal.
    fn dollar(&mut self, word: &mut Word, depth: usize, quoted: bool) -> Result<(), ShellRefusal> {
        if depth > 8 {
            return Err(ShellRefusal::unparseable());
        }
        let start = self.at - 1;
        if self.eat('(') {
            return self.substitute(word, depth, Until::Paren, start);
        }
        let mut inner = Word::default();
        if self.eat('{') {
            loop {
                match self.next().ok_or_else(ShellRefusal::unparseable)? {
                    '}' => break,
                    '\\' => {
                        self.next().ok_or_else(ShellRefusal::unparseable)?;
                    }
                    '\'' if !quoted => self.single_quoted(&mut inner, false)?,
                    '"' => self.double_quoted(&mut inner, depth + 1)?,
                    '$' => self.dollar(&mut inner, depth + 1, quoted)?,
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
                let (line, end) = self.heredoc_line(heredoc.quoted);
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
                while self.at < end {
                    match self.next().ok_or_else(ShellRefusal::unparseable)? {
                        '\\' => self.at += 1,
                        '$' => self.dollar(&mut inner, depth, true)?,
                        '`' => {
                            let start = self.at - 1;
                            self.substitute(&mut inner, depth, Until::Backtick, start)?;
                        }
                        _ => {}
                    }
                }
                // A substitution may run past the line; resume after its line.
                while self.next().is_some_and(|c| c != '\n') {}
            }
            self.seen.push(body);
        }
        Ok(())
    }
    /// The heredoc line at the cursor and the index of its newline. In an
    /// unquoted heredoc, backslash-newline joins lines before the delimiter
    /// comparison, as in bash and zsh.
    fn heredoc_line(&self, quoted: bool) -> (String, usize) {
        let mut line = String::new();
        let mut start = self.at;
        loop {
            let end = self.chars[start..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(self.chars.len(), |offset| start + offset);
            let segment = &self.chars[start..end];
            let escapes = segment.iter().rev().take_while(|&&c| c == '\\').count();
            if quoted || escapes % 2 == 0 || end == self.chars.len() {
                line.extend(segment);
                return (line, end);
            }
            line.extend(&segment[..segment.len() - 1]);
            start = end + 1;
        }
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
            return Err(ShellRefusal::unparseable());
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
            Position::Command if keyword(&["function"]) => Position::Name,
            Position::Name => Position::Command,
            Position::Kill | Position::KillSignal | Position::KillPids if word.plain.is_some() => {
                return Err(ShellRefusal(RefusalReason::ProcessControl));
            }
            Position::Kill if text == "-0" => Position::Argument,
            Position::Kill if text == "-s" || text == "-n" => Position::KillSignal,
            Position::Kill if text.strip_prefix('-').is_some_and(is_signal) => Position::Kill,
            Position::KillSignal if is_signal(text.trim_start_matches('-')) => Position::Kill,
            Position::Kill | Position::KillPids if is_pid(text) => Position::KillPids,
            Position::KillSignal | Position::Kill | Position::KillPids => {
                return Err(ShellRefusal(RefusalReason::ProcessControl));
            }
            Position::Wrapped(COMMAND) if text == "-v" || text == "-V" => Position::Argument,
            Position::Wrapped(XARGS) if !text.starts_with('-') && text.contains("{}") => {
                return Err(ShellRefusal::unparseable());
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
                    self.commands.extend(inspect(body, depth + 1)?);
                }
                if WRAPPERS[wrapper].1.contains(&text) {
                    Position::Value(wrapper)
                } else {
                    Position::Wrapped(wrapper)
                }
            }
            Position::Value(wrapper) => Position::Wrapped(wrapper),
            Position::Body(wrapper) => {
                self.commands.extend(inspect(text, depth + 1)?);
                Position::Wrapped(wrapper)
            }
            Position::Shell => {
                if let Some((operands, dynamic)) = &mut scope.interpreter
                    && !text.starts_with('-')
                {
                    *operands += 1;
                    *dynamic |= word.dynamic;
                }
                // After `sh -c BODY`, further words are positional parameters.
                if !scope.shell_body {
                    self.commands.extend(inspect(text, depth + 1)?);
                }
                if scope.interpreter.is_some() {
                    let option = word.plain.is_none() && text.starts_with('-');
                    if option && !text.starts_with("--") && text.contains('c') {
                        scope.shell_c = true;
                    } else if scope.shell_c && !option {
                        scope.shell_body = true;
                    }
                }
                Position::Shell
            }
            Position::Argument if scope.find && FIND_ACTIONS.contains(&text) => {
                Position::Wrapped(FIND)
            }
            Position::Argument => {
                if !scope.argv.is_empty() {
                    scope.argv.push(word.text);
                }
                Position::Argument
            }
            Position::Command | Position::Wrapped(_) => {
                let next = self.command(scope, &word)?;
                if matches!(next, Position::Argument) {
                    // `find -exec CMD` starts a second command in this scope.
                    if !scope.argv.is_empty() {
                        self.commands.push(std::mem::take(&mut scope.argv));
                    }
                    let name = word.text.rsplit('/').next().unwrap_or_default();
                    scope.argv.push(name.trim_start_matches('=').to_owned());
                }
                next
            }
        };
        Ok(())
    }
    /// Classify a command word: refuse process control, follow wrappers and
    /// shells. Brace and glob forms count when any expansion could match.
    fn command(&mut self, scope: &mut Scope, word: &Word) -> Result<Position, ShellRefusal> {
        if word.dynamic {
            return Err(ShellRefusal(RefusalReason::DynamicCommandName));
        }
        let mut expansions = Vec::new();
        expand_braces(&word.text, &mut expansions)?;
        // zsh expands a leading `=` to the command's path.
        let names: Vec<&str> = expansions
            .iter()
            .map(|expansion| expansion.rsplit('/').next().unwrap_or_default())
            .map(|name| name.trim_start_matches('='))
            .collect();
        if names == ["kill"] {
            // `xargs kill` takes its PIDs from stdin.
            if matches!(scope.position, Position::Wrapped(XARGS)) {
                return Err(ShellRefusal(RefusalReason::ProcessControl));
            }
            return Ok(Position::Kill);
        }
        let named = |list: &[&str]| {
            list.iter()
                .position(|item| names.iter().any(|name| may_name(name, item)))
        };
        if named(&PROCESS_CONTROL).is_some() {
            return Err(ShellRefusal(RefusalReason::ProcessControl));
        }
        if let Some(shell) = named(&SHELLS) {
            scope.interpreter = (shell < INTERPRETERS).then_some((0, false));
            return Ok(Position::Shell);
        }
        if let Some(wrapper) = named(&WRAPPERS.map(|(name, _)| name)) {
            return Ok(Position::Wrapped(wrapper));
        }
        scope.find |= named(&["find"]).is_some();
        Ok(Position::Argument)
    }
}
/// A signal name or number such as `9`, `TERM` or `SIGHUP`.
fn is_signal(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric())
}
/// A literal PID above 1; zero and negatives address process groups, and 1 is init.
fn is_pid(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_digit()) && text.parse::<u32>().is_ok_and(|pid| pid > 1)
}
/// Durations and counts such as `timeout 5s` stay in wrapper position.
fn duration(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ".smhd".contains(c))
}
/// Text that may reach an interpreter as data, such as `echo kill 1 | sh`,
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
    if out.len() > 256 || word.matches('{').count() > 64 {
        return Err(ShellRefusal::expansion());
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
/// `[...]` one character of its set (negated sets, ranges and classes such as
/// `[:alpha:]` match any), `*` any text. A name of only `*` and `?`, such as
/// `*`, is not refused, so ordinary glob arguments stay usable.
fn may_name(name: &str, control: &str) -> bool {
    if !name.contains(['*', '?', '[']) {
        return name == control;
    }
    let mut pattern = Vec::new();
    let mut characters = name.chars();
    while let Some(character) = characters.next() {
        let token = match character {
            '?' => Token::One(None),
            '*' if matches!(pattern.last(), Some(Token::Any)) => continue,
            '*' => Token::Any,
            '[' if name.contains(']') => {
                let (mut set, mut any) = (String::new(), false);
                while let Some(inner) = characters.next() {
                    if inner == ']' && !set.is_empty() {
                        break;
                    }
                    if inner == '[' && matches!(characters.clone().next(), Some(':' | '.' | '=')) {
                        let mut previous = inner;
                        for class in characters.by_ref() {
                            if class == ']' && matches!(previous, ':' | '.' | '=') {
                                break;
                            }
                            previous = class;
                        }
                        any = true;
                    }
                    set.push(inner);
                }
                if any || set.starts_with(['!', '^']) || set.contains('-') {
                    Token::One(None)
                } else {
                    Token::One(Some(set))
                }
            }
            literal => Token::One(Some(literal.to_string())),
        };
        pattern.push(token);
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
/// Linear in the pattern: `reachable[i]` holds when the tokens so far can
/// consume exactly `text[..i]`.
fn matches_glob(pattern: &[Token], text: &[char]) -> bool {
    let mut reachable = vec![false; text.len() + 1];
    reachable[0] = true;
    for token in pattern {
        let mut next = vec![false; text.len() + 1];
        match token {
            Token::Any => {
                if let Some(first) = reachable.iter().position(|&r| r) {
                    next[first..].fill(true);
                }
            }
            Token::One(set) => {
                for (i, character) in text.iter().enumerate() {
                    next[i + 1] =
                        reachable[i] && set.as_ref().is_none_or(|set| set.contains(*character));
                }
            }
        }
        if !next.contains(&true) {
            return false;
        }
        reachable = next;
    }
    reachable[text.len()]
}
#[cfg(test)]
mod tests {
    // Parsing performance tests measure elapsed wall time.
    #![allow(clippy::disallowed_methods)]

    use super::*;
    #[test]
    fn process_control_never_obtains_authority_from_shell_wrapping() {
        for command in [
            "sudo /usr/bin/pkill -f claude",
            "sh -c 'ki\"\"llall -9 42'",
            "ps | xargs kill",
            "killall codex",
            "${CMD} 42",
            "pki\\\nll 1",
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
    fn refusals_report_their_reason() {
        let braces = "{a,b}".repeat(65);
        for (command, reason) in [
            ("pkill -f claude", RefusalReason::ProcessControl),
            ("$CMD 1", RefusalReason::DynamicCommandName),
            (braces.as_str(), RefusalReason::Expansion),
            ("echo \"open", RefusalReason::Unparseable),
        ] {
            assert_eq!(check(command).expect_err(command).0, reason, "{command}");
        }
    }
    #[test]
    fn every_command_position_is_inspected() {
        for command in [
            "echo $(pkill 1)",
            "echo \"`pkill x`\"",
            "cat <(pkill 1)",
            "echo \"${x:-$(pkill 1)}\"",
            "sudo -u root pkill 1",
            "sudo --user root pkill 1",
            "env FOO=1 pkill 1",
            "env -i pkill 1",
            "env -S 'pkill 1'",
            "{env,-i} pkill 1",
            "timeout 5 pkill 1",
            "timeout -s TERM 5s pkill 1",
            "nice -n 5 pkill 1",
            "xargs -I {} pkill {}",
            "X=1 pkill 1",
            "X+=1 pkill 1",
            ">out pkill 1",
            "true &>/dev/null; pkill 1",
            "echo ok\npkill 1",
            "find . -name x -exec pkill {} \\;",
            "find . -exec true \\; -exec pkill {} \\;",
            "if true; then pkill 1; fi",
            "for p in 1; do pkill $p; done",
            "(pkill 1)",
            "{ pkill 1; }",
            "f() { pkill 1; }",
            "case x in a) pkill 1;; esac",
            "echo \"$(case x in a) pkill 1;; esac)\"",
            "bash -lc 'echo ok; pkill 1'",
            "eval 'pkill 1'",
            "su -c 'pkill 1'",
            "trap 'pkill 1' EXIT",
            "cat <<EOF\n$(pkill 1)\nEOF",
            "echo \"$(cat <<EOF\n`pkill 1`\nEOF\n)\"",
            "$CMD 1",
            "\"$HOME/bin/tool\" 1",
            "$(which tool) 1",
            "echo kill 1 | sh",
            "cat <<EOF\nEO\\\nF\npkill 1\nEOF",
            "function f { pkill 1; }; f",
            "/bin/[[:alpha:]]ill 1",
            "/bin/[[:alpha:][:digit:]]ill 1",
            "env -P /bin pkill 1",
            "sudo -R /x pkill 1",
            "=pkill 1",
            "echo \"${x:-'}\"; pkill 1; echo \"'}\"",
            "arch -arm64 pkill 1",
            "gtimeout 5 pkill 1",
            "kill",
            "kill -9",
            "kill -s KILL",
            "kill 0",
            "kill 1",
            "sudo kill -9 1",
            "kill -1",
            "kill -- -1",
            "kill 1 -1",
            "kill -9 $PID",
            "kill $(pgrep x)",
            "kill \"1\"",
            "xargs kill 123",
            "echo 1 | xargs kill -9",
            "pkill x",
            "killall5",
            "skill x",
            "killpg 1",
            "echo kill 1 | bash",
            "printf kill | xargs -I{} {} 1",
            "source <(echo x)",
            "bash <<'EOF'\nkill 1\nEOF",
            "printf 'ki\"\"ll 1' | bash",
            "echo $(",
            "echo \"open",
        ] {
            assert!(check(command).is_err(), "{command}");
        }
    }
    #[test]
    fn pathological_input_is_bounded() {
        use std::time::{Duration, Instant};
        let nested = format!(
            "{}{}; kill 1",
            "${a:-\"".repeat(100_000),
            "\"}".repeat(100_000)
        );
        for command in [
            format!("{}z 1", "*".repeat(64)),
            format!("{}z 1", "*k".repeat(64)),
            nested,
            format!("{} 1", "{".repeat(20_000)),
        ] {
            let started = Instant::now();
            let _ = check(&command);
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "{}",
                &command[..16]
            );
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
            "rg -l foo | xargs rg kill",
            "find . -name '*.rs' | xargs grep -n pkill",
            "bash scripts/x.sh && git commit -m 'fix kill path'",
            "command -v pkill",
            "while kill -0 \"$PID\"; do sleep 1; done",
            "kill 84322",
            "kill -9 123 456",
            "kill -TERM 42",
            "kill -s KILL 42",
            "kill -s -HUP 42",
            "sudo kill -HUP 42 && echo ok",
        ] {
            assert!(check(command).is_ok(), "{command}");
        }
    }
}
