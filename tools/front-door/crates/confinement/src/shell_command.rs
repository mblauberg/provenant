//! Conservative bounded inspection of direct harness shell commands.
//! Custody and OS confinement remain authoritative; this is no shell evaluator.

/// Input the walk cannot follow: incomplete or overdeep syntax.
#[derive(Debug)]
struct Unparseable;

/// The simple commands in `command`, as words with the command name first.
/// Wrappers and shells are looked through, so `sudo git push -f` and
/// `bash -c 'git push -f'` both yield `git push -f`. A command name is the last
/// path component. `None` when the syntax is incomplete or overdeep: callers
/// allow what they cannot read. Scripts, aliases, functions and other languages
/// are not interpreted.
pub fn commands(command: &str) -> Option<Vec<Vec<String>>> {
    inspect(command, 0).ok()
}
fn inspect(command: &str, depth: usize) -> Result<Vec<Vec<String>>, Unparseable> {
    if depth > 8 || command.len() > 1024 * 1024 || command.contains('\0') {
        return Err(Unparseable);
    }
    let mut lexer = Lexer {
        chars: command.chars().collect(),
        at: 0,
        commands: Vec::new(),
    };
    lexer.list(depth, Until::End)?;
    Ok(lexer.commands)
}
/// Commands any of whose operands may be command text; each is inspected as a
/// command line. The first `INTERPRETERS` take a `-c` body and positional
/// parameters after it.
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
    /// The command is a shell that takes `-c BODY`.
    interpreter: bool,
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
    /// Finished simple commands, including those inside shell bodies.
    commands: Vec<Vec<String>>,
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
    fn list(&mut self, depth: usize, until: Until) -> Result<(), Unparseable> {
        if depth > 8 {
            return Err(Unparseable);
        }
        let mut scope = Scope::default();
        let mut word = Word::default();
        let closed = loop {
            let Some(character) = self.next() else {
                break false;
            };
            match character {
                '\\' => match self.next().ok_or(Unparseable)? {
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
            return Err(Unparseable);
        }
        Ok(())
    }
    fn single_quoted(&mut self, word: &mut Word, ansi: bool) -> Result<(), Unparseable> {
        loop {
            match self.next().ok_or(Unparseable)? {
                '\'' => return Ok(()),
                '\\' if ansi => {
                    let escaped = self.next().ok_or(Unparseable)?;
                    word.push(escaped);
                }
                character => word.push(character),
            }
        }
    }
    /// Close a simple command and reset the scope for the next one.
    fn end_command(&mut self, scope: &mut Scope) -> Result<(), Unparseable> {
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
    fn double_quoted(&mut self, word: &mut Word, depth: usize) -> Result<(), Unparseable> {
        if depth > 8 {
            return Err(Unparseable);
        }
        word.special();
        word.quoted = true;
        loop {
            match self.next().ok_or(Unparseable)? {
                '"' => return Ok(()),
                '\\' => match self.next().ok_or(Unparseable)? {
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
    fn dollar(&mut self, word: &mut Word, depth: usize, quoted: bool) -> Result<(), Unparseable> {
        if depth > 8 {
            return Err(Unparseable);
        }
        let start = self.at - 1;
        if self.eat('(') {
            return self.substitute(word, depth, Until::Paren, start);
        }
        let mut inner = Word::default();
        if self.eat('{') {
            loop {
                match self.next().ok_or(Unparseable)? {
                    '}' => break,
                    '\\' => {
                        self.next().ok_or(Unparseable)?;
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
        word.text.extend(&self.chars[start..self.at]);
        Ok(())
    }
    fn substitute(
        &mut self,
        word: &mut Word,
        depth: usize,
        until: Until,
        start: usize,
    ) -> Result<(), Unparseable> {
        self.list(depth + 1, until)?;
        word.special();
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
    fn heredocs(&mut self, scope: &mut Scope, depth: usize) -> Result<(), Unparseable> {
        for heredoc in std::mem::take(&mut scope.heredocs) {
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
                if heredoc.quoted {
                    self.at = (end + 1).min(self.chars.len());
                    continue;
                }
                let mut inner = Word::default();
                while self.at < end {
                    match self.next().ok_or(Unparseable)? {
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
    ) -> Result<(), Unparseable> {
        let word = std::mem::take(word);
        if !word.started {
            return Ok(());
        }
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
            Position::Wrapped(COMMAND) if text == "-v" || text == "-V" => Position::Argument,
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
                // After `sh -c BODY`, further words are positional parameters.
                if !scope.shell_body && !text.starts_with('-') {
                    self.commands.extend(inspect(text, depth + 1)?);
                }
                if scope.interpreter {
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
                let next = self.command(scope, &word);
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
    /// Classify a command word: follow wrappers and shells.
    fn command(&mut self, scope: &mut Scope, word: &Word) -> Position {
        // zsh expands a leading `=` to the command's path.
        let name = word.text.rsplit('/').next().unwrap_or_default();
        let name = name.trim_start_matches('=');
        if let Some(shell) = SHELLS.iter().position(|item| *item == name) {
            scope.interpreter = shell < INTERPRETERS;
            return Position::Shell;
        }
        if let Some(wrapper) = WRAPPERS.iter().position(|(item, _)| *item == name) {
            return Position::Wrapped(wrapper);
        }
        scope.find |= name == "find";
        Position::Argument
    }
}
/// Durations and counts such as `timeout 5s` stay in wrapper position.
fn duration(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ".smhd".contains(c))
}
#[cfg(test)]
mod tests {

    use super::*;

    fn names(command: &str) -> Vec<String> {
        commands(command)
            .unwrap_or_else(|| panic!("unparseable: {command}"))
            .into_iter()
            .map(|argv| argv[0].clone())
            .collect()
    }
    #[test]
    fn every_command_position_is_inspected() {
        for command in [
            "echo $(mark 1)",
            "echo \"`mark x`\"",
            "cat <(mark 1)",
            "echo \"${x:-$(mark 1)}\"",
            "sudo -u root mark 1",
            "sudo --user root mark 1",
            "env FOO=1 mark 1",
            "env -i mark 1",
            "env -S 'mark 1'",
            "timeout 5 mark 1",
            "timeout -s TERM 5s mark 1",
            "nice -n 5 mark 1",
            "xargs -I {} mark {}",
            "X=1 mark 1",
            "X+=1 mark 1",
            ">out mark 1",
            "true &>/dev/null; mark 1",
            "echo ok\nmark 1",
            "find . -name x -exec mark {} \\;",
            "find . -exec true \\; -exec mark {} \\;",
            "if true; then mark 1; fi",
            "for p in 1; do mark $p; done",
            "(mark 1)",
            "{ mark 1; }",
            "f() { mark 1; }",
            "case x in a) mark 1;; esac",
            "bash -lc 'echo ok; mark 1'",
            "eval 'mark 1'",
            "su -c 'mark 1'",
            "trap 'mark 1' EXIT",
            "cat <<EOF\n$(mark 1)\nEOF",
            "echo \"$(cat <<EOF\n`mark 1`\nEOF\n)\"",
            "cat <<EOF\nEO\\\nF\nmark 1\nEOF",
            "function f { mark 1; }; f",
            "env -P /bin mark 1",
            "sudo -R /x mark 1",
            "=mark 1",
            "echo \"${x:-'}\"; mark 1; echo \"'}\"",
            "arch -arm64 mark 1",
            "gtimeout 5 mark 1",
            "/usr/bin/mark 1",
            "\"$HOME/bin/mark\" 1",
        ] {
            assert!(names(command).contains(&"mark".to_owned()), "{command}");
        }
    }
    #[test]
    fn commands_carry_their_words_and_nothing_else() {
        for (command, expected) in [
            (
                "git push -f origin main",
                vec![vec!["git", "push", "-f", "origin", "main"]],
            ),
            ("sudo git stash", vec![vec!["git", "stash"]]),
            ("bash -c 'git stash' _ 'echo'", vec![vec!["git", "stash"]]),
            (
                "echo git reset --hard",
                vec![vec!["echo", "git", "reset", "--hard"]],
            ),
            (
                "a 1 && b 2 | c",
                vec![vec!["a", "1"], vec!["b", "2"], vec!["c"]],
            ),
            ("rm -rf \"$HOME\"", vec![vec!["rm", "-rf", "$HOME"]]),
        ] {
            assert_eq!(commands(command).unwrap(), expected, "{command}");
        }
    }
    #[test]
    fn process_control_dynamic_names_and_sleeps_are_ordinary_commands() {
        for (command, name) in [
            ("pkill -f claude", "pkill"),
            ("killall codex", "killall"),
            ("kill -9 $PID", "kill"),
            ("kill 1", "kill"),
            ("sleep 30", "sleep"),
            ("$CMD 1", "$CMD"),
        ] {
            assert_eq!(names(command), [name], "{command}");
        }
        assert_eq!(names("echo kill 1 | sh"), ["echo"]);
    }
    #[test]
    fn incomplete_or_overdeep_input_is_unreadable() {
        for command in ["echo $(", "echo \"open", "echo ${x:-", &"(".repeat(20)] {
            assert!(commands(command).is_none(), "{command}");
        }
    }
    #[test]
    fn pathological_input_is_bounded() {
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
            let _ = commands(&command);
        }
    }
}
