//! Shell parsing and process-kill policy. The hook process edge remains Python-owned.

const RESERVED: &[&str] = &[
    "{", "}", "!", "if", "then", "else", "elif", "fi", "while", "until", "do", "done", "for", "in",
    "case", "esac", "function", "select",
];
const WRAPPERS: &[&str] = &[
    "sudo",
    "doas",
    "env",
    "nice",
    "ionice",
    "timeout",
    "xargs",
    "nohup",
    "command",
    "builtin",
    "exec",
    "time",
    "setsid",
    "stdbuf",
    "caffeinate",
];

fn assignment(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else {
        return false;
    };
    if value.contains('\n') {
        return false;
    }
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn strip_reserved(mut words: &[String]) -> &[String] {
    while words
        .first()
        .is_some_and(|w| RESERVED.contains(&w.as_str()))
    {
        words = &words[1..];
    }
    while words.first().is_some_and(|w| assignment(w)) {
        words = &words[1..];
    }
    words
}

fn short_options(wrapper: &str) -> &str {
    match wrapper {
        "sudo" => "ugpCDhRrTtU",
        "doas" => "uC",
        "env" => "uCS",
        "nice" => "n",
        "ionice" => "cnp",
        "timeout" => "sk",
        "xargs" => "IJnPLsdEa",
        "stdbuf" => "ioe",
        _ => "",
    }
}

fn long_options(wrapper: &str) -> &[&str] {
    match wrapper {
        "sudo" => &[
            "user",
            "group",
            "host",
            "prompt",
            "chdir",
            "role",
            "type",
            "other-user",
            "close-from",
        ],
        "env" => &["unset", "chdir", "split-string"],
        "nice" => &["adjustment"],
        "ionice" => &["class", "classdata"],
        "timeout" => &["signal", "kill-after"],
        "xargs" => &[
            "max-args",
            "max-procs",
            "max-lines",
            "delimiter",
            "arg-file",
            "replace",
        ],
        "stdbuf" => &["input", "output", "error"],
        _ => &[],
    }
}

/// Returns the unwrapped command and whether xargs supplied its arguments.
pub fn unwrap_command(mut words: &[String]) -> (&[String], bool) {
    let mut xargs = false;
    words = strip_reserved(words);
    while let Some(first) = words.first() {
        let wrapper = first.rsplit('/').next().unwrap_or("");
        if !WRAPPERS.contains(&wrapper) {
            break;
        }
        xargs |= wrapper == "xargs";
        words = &words[1..];
        while let Some(word) = words.first() {
            if !(word.starts_with('-') || (matches!(wrapper, "env" | "nice") && word.contains('=')))
            {
                break;
            }
            let long_value = word
                .strip_prefix("--")
                .is_some_and(|w| !w.contains('=') && long_options(wrapper).contains(&w));
            let short_value = !word.starts_with("--")
                && word.len() == 2
                && short_options(wrapper).contains(&word[1..]);
            words = &words[(if long_value || short_value { 2 } else { 1 }).min(words.len())..];
        }
        if wrapper == "timeout" {
            words = &words[usize::from(!words.is_empty())..];
        }
        words = strip_reserved(words);
    }
    (words, xargs)
}

fn decimal(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| crate::pending::decimal_digit(c).is_some())
}

// Python isdigit() also includes compatibility digits; signal option parsing uses it.
fn digits(word: &str) -> bool {
    !word.is_empty() && word.chars().all(crate::pending::python_digit)
}

pub fn kill_reason(args: &[String]) -> Option<String> {
    let mut targets = Vec::new();
    let (mut options_done, mut signalled, mut i) = (false, false, 0);
    while let Some(word) = args.get(i) {
        if !options_done && word == "--" {
            options_done = true;
        } else if !options_done && matches!(word.as_str(), "-l" | "-L" | "-t") {
            return None;
        } else if !options_done && matches!(word.as_str(), "-s" | "-n" | "--signal") {
            signalled = true;
            i += 1;
        } else if !options_done && word.starts_with('-') && word.len() > 1 {
            if signalled && digits(&word[1..]) {
                return Some("kill of a process group".into());
            }
            signalled = true;
        } else {
            targets.push(word);
        }
        i += 1;
    }
    for target in targets {
        if target.starts_with('-') {
            return Some("kill of a process group".into());
        }
        if !decimal(target) {
            return Some(format!(
                "kill target {} is not a literal PID",
                provenant_core::py::python_repr(target)
            ));
        }
        if target
            .chars()
            .all(|c| crate::pending::decimal_digit(c) == Some(0))
        {
            return Some("kill 0 signals the caller's process group".into());
        }
    }
    None
}

/// A shell simple command; bodies are here-strings and here-documents.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ShellCommand {
    pub words: Vec<String>,
    pub bodies: Vec<String>,
}

fn fragment(text: &[char], start: usize, end: usize) -> String {
    text.get(start.min(text.len())..end.min(text.len()))
        .unwrap_or_default()
        .iter()
        .collect()
}

fn starts(text: &[char], at: usize, needle: &str) -> bool {
    text.get(at..).is_some_and(|rest| {
        rest.iter()
            .copied()
            .zip(needle.chars())
            .all(|(a, b)| a == b)
            && rest.len() >= needle.chars().count()
    })
}

fn skip_substitution(text: &[char], mut i: usize) -> usize {
    let (mut depth, mut quote) = (1, None);
    while let Some(&c) = text.get(i) {
        if let Some(q) = quote {
            if c == '\\' && q == '"' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else if c == '\\' {
            i += 1;
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    text.len()
}

fn skip_backtick(text: &[char], mut i: usize) -> usize {
    while text.get(i).is_some_and(|c| *c != '`') {
        i += if text[i] == '\\' { 2 } else { 1 };
    }
    (i + 1).min(text.len())
}

pub fn substitutions(text: &str, double_quoted: bool) -> Vec<String> {
    let text: Vec<char> = text.chars().collect();
    let (mut found, mut i, mut single) = (Vec::new(), 0, false);
    while let Some(&c) = text.get(i) {
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '\'' && !double_quoted {
            single = !single;
        } else if !single && starts(&text, i, "$(") && !starts(&text, i, "$((") {
            let end = skip_substitution(&text, i + 2);
            found.push(fragment(&text, i + 2, end.saturating_sub(1)));
            i = end;
            continue;
        } else if !single && c == '`' {
            let end = skip_backtick(&text, i + 1);
            found.push(fragment(&text, i + 1, end.saturating_sub(1)));
            i = end;
            continue;
        }
        i += 1;
    }
    found
}

#[derive(Default)]
struct Parser {
    commands: Vec<ShellCommand>,
    visible: Vec<bool>,
    word: String,
    in_word: bool,
    skip: &'static str,
    pending: Vec<(usize, String, bool)>,
}
impl Parser {
    fn new() -> Self {
        Self {
            commands: vec![ShellCommand::default()],
            visible: vec![false],
            ..Self::default()
        }
    }
    fn end_word(&mut self) {
        if !self.in_word {
            return;
        }
        self.in_word = false;
        let word = std::mem::take(&mut self.word);
        let owner = self.commands.len() - 1;
        match self.skip {
            "redirect" => {}
            "here" => self.commands[owner].bodies.push(word),
            "" => self.commands[owner].words.push(word),
            value => self.pending.push((owner, word, value == "heredoc-raw")),
        }
        self.skip = "";
    }
    fn end_command(&mut self) {
        self.end_word();
        let i = self.commands.len() - 1;
        self.visible[i] = !self.commands[i].words.is_empty() || !self.commands[i].bodies.is_empty();
        self.commands.push(ShellCommand::default());
        self.visible.push(false);
    }
}

pub fn parse(text: &str) -> (Vec<ShellCommand>, Vec<String>) {
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut nested, mut i) = (Parser::new(), Vec::new(), 0);
    let n = text.len();
    while i < n {
        let c = text[i];
        if c == '\\' {
            if text.get(i + 1) == Some(&'\n') {
                i += 2;
                continue;
            }
            p.in_word = true;
            if let Some(c) = text.get(i + 1) {
                p.word.push(*c);
            }
            i += 2;
        } else if c == '\'' {
            let end = (i + 1..n).find(|&j| text[j] == '\'').unwrap_or(n);
            p.in_word = true;
            p.word.push_str(&fragment(&text, i + 1, end));
            if p.skip == "heredoc" {
                p.skip = "heredoc-raw";
            }
            i = end + 1;
        } else if c == '"' {
            let mut j = i + 1;
            while j < n && text[j] != '"' {
                j += if text[j] == '\\' { 2 } else { 1 };
            }
            let body = fragment(&text, i + 1, j);
            nested.extend(substitutions(&body, false));
            p.in_word = true;
            let mut chars = body.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' && chars.peek().is_some_and(|c| *c != '\n') {
                    if let Some(c) = chars.next() {
                        p.word.push(c);
                    }
                } else {
                    p.word.push(c);
                }
            }
            if p.skip == "heredoc" {
                p.skip = "heredoc-raw";
            }
            i = j + 1;
        } else if starts(&text, i, "$'") {
            let mut end = i + 2;
            while end < n && text[end] != '\'' {
                end += if text[end] == '\\' { 2 } else { 1 };
            }
            p.in_word = true;
            p.word.push_str(&fragment(&text, i + 2, end));
            i = end + 1;
        } else if starts(&text, i, "$((") {
            let end = skip_substitution(&text, i + 2);
            p.in_word = true;
            p.word.push_str(&fragment(&text, i, end));
            i = end;
        } else if starts(&text, i, "$(") || matches!(c, '<' | '>') && starts(&text, i + 1, "(") {
            let end = skip_substitution(&text, i + 2);
            nested.push(fragment(&text, i + 2, end.saturating_sub(1)));
            if c == '$' {
                p.in_word = true;
                p.word.push_str("$(…)");
            }
            i = end;
        } else if c == '`' {
            let end = skip_backtick(&text, i + 1);
            nested.push(fragment(&text, i + 1, end.saturating_sub(1)));
            p.in_word = true;
            p.word.push_str("`…`");
            i = end;
        } else if starts(&text, i, "${") {
            let end = (i..n).find(|&j| text[j] == '}').unwrap_or(n - 1);
            p.in_word = true;
            p.word.push_str(&fragment(&text, i, end + 1));
            i = end + 1;
        } else if c == '#' && !p.in_word {
            while i < n && text[i] != '\n' {
                i += 1;
            }
        } else if matches!(c, ' ' | '\t') {
            p.end_word();
            i += 1;
        } else if c == '\n' {
            p.end_command();
            for (owner, delimiter, raw) in std::mem::take(&mut p.pending) {
                let mut lines = Vec::new();
                i += 1;
                while i < n {
                    let eol = (i..n).find(|&j| text[j] == '\n').unwrap_or(n);
                    let line = fragment(&text, i, eol);
                    i = eol + 1;
                    if line.trim_matches('\t') == delimiter {
                        break;
                    }
                    lines.push(line);
                }
                let body = lines.join("\n");
                if !raw {
                    nested.extend(substitutions(&body, false));
                }
                p.commands[owner].bodies.push(body);
                i = i.saturating_sub(1);
            }
            i += 1;
        } else if matches!(c, '<' | '>') {
            if p.in_word && digits(&p.word) {
                p.word.clear();
                p.in_word = false;
            } else {
                p.end_word();
            }
            if starts(&text, i, "<<<") {
                p.skip = "here";
                i += 3;
            } else if starts(&text, i, "<<") {
                p.skip = "heredoc";
                i += 2;
                if starts(&text, i, "-") {
                    i += 1;
                }
            } else {
                p.skip = "redirect";
                i += 1;
                while i < n && "<>&|".contains(text[i]) {
                    i += 1;
                }
            }
        } else if ";&|()".contains(c) {
            if c == '&' && starts(&text, i + 1, ">") {
                p.skip = "redirect";
                i += 2;
                continue;
            }
            p.end_command();
            p.skip = "";
            i += 1;
            while i < n && ";&|".contains(text[i]) && ";&|".contains(c) {
                i += 1;
            }
        } else {
            p.in_word = true;
            p.word.push(c);
            i += 1;
        }
    }
    p.end_command();
    (
        p.commands
            .into_iter()
            .zip(p.visible)
            .filter_map(|(c, visible)| visible.then_some(c))
            .collect(),
        nested,
    )
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Pattern(#[from] regex::Error),
}

fn word_character(c: char) -> bool {
    crate::pending::python_word_character(c)
}

struct Guard {
    raw: regex::Regex,
    wrappers: regex::Regex,
    wrapped: regex::Regex,
    heredoc: regex::Regex,
}
impl Guard {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            raw: regex::Regex::new(
                r"(?:^|[;&|({`\n]|\$\()[\s\x1c-\x1f]*(?:[^\s\x1c-\x1f]*/)?(?:pkill|killall)",
            )?,
            wrappers: regex::Regex::new(
                r"sudo|doas|env|xargs|nice|timeout|nohup|exec|command|time|setsid|stdbuf",
            )?,
            wrapped: regex::Regex::new(
                r"\A[^;&|\n]*?[\s\x1c-\x1f](?:[^\s\x1c-\x1f]*/)?(?:pkill|killall)\z",
            )?,
            heredoc: regex::Regex::new(
                r#"<<-?[\s\x1c-\x1f]*(['"])([\p{L}\p{N}_]+)(['"])[^\n]*\n"#,
            )?,
        })
    }
    fn raw_match(&self, text: &str) -> bool {
        // Python \b uses letters/numbers/underscore, not Rust regex's broader
        // Unicode word class (which additionally includes combining marks).
        if self.raw.find_iter(text).any(|m| {
            text[m.end()..]
                .chars()
                .next()
                .is_none_or(|c| !word_character(c))
        }) {
            return true;
        }
        for m in self.wrappers.find_iter(text) {
            if text[..m.start()]
                .chars()
                .next_back()
                .is_some_and(word_character)
                || text[m.end()..].chars().next().is_some_and(word_character)
            {
                continue;
            }
            let suffix = &text[m.end()..];
            // Check complete prefixes ending at a Python word boundary. Anchoring
            // the suffix prevents its search from restarting after ;, |, & or LF.
            // Trying every name also retains matches after a non-boundary name
            // such as `sudo echo pkillx pkill`.
            for name in ["pkill", "killall"] {
                if suffix.match_indices(name).any(|(start, _)| {
                    let end = start + name.len();
                    suffix[end..]
                        .chars()
                        .next()
                        .is_none_or(|c| !word_character(c))
                        && self.wrapped.is_match(&suffix[..end])
                }) {
                    return true;
                }
            }
        }
        false
    }
    fn remove_quoted_heredocs(&self, command: &str) -> Result<String, Error> {
        let (mut result, mut at) = (String::new(), 0);
        for captures in self.heredoc.captures_iter(command) {
            let Some(full) = captures.get(0) else {
                continue;
            };
            if full.start() < at
                || captures.get(1).map(|m| m.as_str()) != captures.get(3).map(|m| m.as_str())
            {
                continue;
            }
            let Some(delimiter) = captures.get(2) else {
                continue;
            };
            let closing = regex::Regex::new(&format!(
                r"\n[\s\x1c-\x1f]*{}(?:\n|$)",
                regex::escape(delimiter.as_str())
            ))?;
            let Some(end) = closing.find(&command[full.end()..]) else {
                continue;
            };
            result.push_str(&command[at..full.start()]);
            at = full.end() + end.end();
        }
        result.push_str(&command[at..]);
        Ok(result)
    }
    fn executable_text(&self, command: &str) -> Result<String, Error> {
        let text: Vec<char> = self.remove_quoted_heredocs(command)?.chars().collect();
        let (mut out, mut i) = (String::new(), 0);
        while let Some(&c) = text.get(i) {
            if c == '\\' {
                out.push_str(&fragment(&text, i, i + 2));
                i += 2;
            } else if c == '\'' {
                i = (i + 1..text.len())
                    .find(|&j| text[j] == '\'')
                    .map_or(text.len(), |j| j + 1);
                out.push(' ');
            } else if c == '"' {
                let mut j = i + 1;
                while j < text.len() && text[j] != '"' {
                    j += if text[j] == '\\' { 2 } else { 1 };
                }
                for inner in substitutions(&fragment(&text, i + 1, j), true) {
                    out.push('\n');
                    out.push_str(&self.executable_text(&inner)?);
                    out.push('\n');
                }
                out.push(' ');
                i = j + 1;
            } else {
                out.push(c);
                i += 1;
            }
        }
        Ok(out)
    }
    fn command_reason(
        &self,
        command: &ShellCommand,
        depth: usize,
    ) -> Result<Option<String>, Error> {
        let (words, via_xargs) = unwrap_command(&command.words);
        let Some(first) = words.first() else {
            return Ok(None);
        };
        let name = first.rsplit('/').next().unwrap_or("");
        let args = &words[1..];
        if ["pkill", "killall", "killall5", "skill"].contains(&name) {
            return Ok(Some(format!("{name} signals processes by name")));
        }
        if name == "kill" {
            return if via_xargs {
                Ok(Some("xargs kill takes its targets from a pipe".into()))
            } else {
                Ok(kill_reason(args))
            };
        }
        if ["sh", "bash", "zsh", "dash", "ksh"].contains(&name) {
            for (index, arg) in args.iter().enumerate() {
                if arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('c') {
                    if let Some(next) = args.get(index + 1) {
                        return self.deny_reason(next, depth + 1);
                    }
                    break;
                }
                if !arg.starts_with('-') {
                    break;
                }
            }
            for body in &command.bodies {
                if let Some(reason) = self.deny_reason(body, depth + 1)? {
                    return Ok(Some(reason));
                }
            }
        }
        if name == "eval" {
            return self.deny_reason(&args.join(" "), depth + 1);
        }
        if name == "." {
            return Ok(None);
        }
        if ["find", "fd"].contains(&name) {
            for (index, arg) in args.iter().enumerate() {
                if ["-exec", "-execdir", "-ok", "-okdir", "-x", "--exec"].contains(&arg.as_str()) {
                    let inner = ShellCommand {
                        words: args[index + 1..]
                            .iter()
                            .take_while(|s| ![";", "+"].contains(&s.as_str()))
                            .cloned()
                            .collect(),
                        bodies: vec![],
                    };
                    if let Some(reason) = self.command_reason(&inner, depth + 1)? {
                        return Ok(Some(reason));
                    }
                }
            }
        }
        Ok(None)
    }
    fn deny_reason(&self, command: &str, depth: usize) -> Result<Option<String>, Error> {
        if depth > 8 {
            return Ok(Some("command nesting is too deep to check".into()));
        }
        if self.raw_match(&self.executable_text(command)?) {
            return Ok(Some("pkill or killall signals processes by name".into()));
        }
        let (commands, nested) = parse(command);
        for item in &commands {
            if let Some(reason) = self.command_reason(item, depth)? {
                return Ok(Some(reason));
            }
        }
        for text in nested {
            if let Some(reason) = self.deny_reason(&text, depth + 1)? {
                return Ok(Some(reason));
            }
        }
        Ok(None)
    }
}

pub fn executable_text(command: &str) -> Result<String, Error> {
    Guard::new()?.executable_text(command)
}
pub fn deny_reason(command: &str) -> Result<Option<String>, Error> {
    Guard::new()?.deny_reason(command, 0)
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HookEnvelope {
    #[serde(rename = "hookSpecificOutput")]
    pub output: HookDecision,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HookDecision {
    #[serde(rename = "hookEventName")]
    pub event: String,
    #[serde(rename = "permissionDecision")]
    pub decision: String,
    #[serde(rename = "permissionDecisionReason")]
    pub reason: String,
}
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error(transparent)]
    Policy(#[from] Error),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}
/// Decoded Unicode hook boundary. Invalid shapes and non-Bash tools are silent.
/// Raw lone-surrogate JSON still needs the records owner's Python string codec;
/// callers must keep decode errors explicit until that integration is available.
pub fn hook_input(input: &[u8]) -> Result<crate::CommandOutput, HookError> {
    let event = provenant_records::checked_json::from_slice(input)?;
    hook_event(&event)
}
pub fn hook_event(event: &serde_json::Value) -> Result<crate::CommandOutput, HookError> {
    let Some(command) = event
        .get("tool_input")
        .and_then(|v| v.get("command"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(crate::CommandOutput::default());
    };
    if event.get("tool_name").and_then(serde_json::Value::as_str) != Some("Bash") {
        return Ok(crate::CommandOutput::default());
    }
    let Some(reason) = deny_reason(command)? else {
        return Ok(crate::CommandOutput::default());
    };
    let record=HookEnvelope {output:HookDecision {event:"PreToolUse".into(),decision:"deny".into(),reason:format!("Refused: {reason}. A pattern, group or expanded kill can hit another lane's process. Stop explicit PIDs with `provenant reap <pid>… [--tree]`.")}};
    let quote = crate::pending::ascii_json_string;
    let stdout=format!("{{\"hookSpecificOutput\": {{\"hookEventName\": {}, \"permissionDecision\": {}, \"permissionDecisionReason\": {}}}}}\n",quote(&record.output.event)?,quote(&record.output.decision)?,quote(&record.output.reason)?);
    Ok(crate::CommandOutput {
        stdout,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parser_and_decisions_match_source_policy_cases() {
        // Import policy fixtures, rather than copying the source tokenizer or its seams.
        type Case = (String, serde_json::Value, Option<String>, String);
        let Some(cases): Option<Vec<Case>> = crate::oracle::python(
            r#"
import ast,json,sys
from pathlib import Path
from scripts.lane_safety_guard import parse,deny_reason,executable_text
# Read literal test data without importing pytest-dependent process tests.
tree=ast.parse(Path('tests/test_lane_safety.py').read_text())
commands=[]
for node in tree.body:
    if isinstance(node,ast.Assign) and any(isinstance(t,ast.Name) and t.id in {'DENIED','ALLOWED'} for t in node.targets): commands.extend(ast.literal_eval(node.value))
commands += json.load(sys.stdin)
import random
rng=random.Random(812)
tokens=['pkill','killall','killall5','skill','kill 0','kill 42','echo','sudo','env A=1','timeout 5',"'",'"','\\',';','|','\n','\t',' ','(',')','$(kill 0)','`echo x`','${X:-pkill}','<','>','<<<',"é",'\u0300']
commands.extend(''.join(rng.choices(tokens,k=rng.randrange(1,14))) for _ in range(600))
print(json.dumps([(s,([{'words':c.words,'bodies':c.bodies} for c in parse(s)[0]],parse(s)[1]),deny_reason(s),executable_text(s)) for s in commands]))
"#,
            &serde_json::json!([
                "A='1\n2' kill 0",
                "cat <<'EOF'\npkill x\nEOF\npkill x",
                "echo pkill\u{0300}",
                "sudo pkill\u{0300}",
                "pkill\u{0345}",
                "sudo pkill\u{05bf}",
                "<<EOF\nbody\nEOF",
                "echo \"\\\n$(kill 0)\"",
                "cat <<A <<B\nx\nA\ny\nB",
                "é=$(echo x) kill 42",
                "sudo ls; echo pkill",
                "sudo ls\necho pkill",
                "sudo ls & echo killall",
                "sudo ls | echo pkill",
                "sudo ls; pkill x",
                "sudo ls\nkillall x",
                "sudo ls pkill",
                "sudo ls pkillx",
                "sudo ls xpkill",
                "sudo ls /usr/binxpkill",
                "sudo ls /usr/bin/killall",
                "sudo ls\n pkill",
                "sudo ls; echo /usr/bin/pkill",
                "sudo ls; echo ;/pkill",
                "sudo ls\necho x/pkill",
                "echo ${sudo pkillx pkill}",
                "echo ${sudo killallx killall}",
                "echo ${sudo a;/pkill}",
                "echo ${sudo ls; echo pkill}"
            ]),
        ) else {
            return;
        };
        let guard = Guard::new().unwrap();
        for (text, parsed, reason, executable) in cases {
            let (commands, nested) = parse(&text);
            assert_eq!(
                serde_json::json!([commands, nested]),
                parsed,
                "parse {text:?}"
            );
            assert_eq!(
                guard.executable_text(&text).unwrap(),
                executable,
                "executable {text:?}"
            );
            assert_eq!(
                guard.deny_reason(&text, 0).unwrap(),
                reason,
                "deny {text:?}"
            );
        }
    }

    #[test]
    fn literal_pid_zero_check_has_no_integer_range_or_digit_limit() {
        for target in [
            "99999999999999999999".into(),
            "7".repeat(4300),
            "7".repeat(4301),
            format!("{}١", "٠".repeat(4301)),
        ] {
            assert_eq!(kill_reason(&[target]), None);
        }
        for target in ["0".repeat(4300), "٠".repeat(4301)] {
            assert_eq!(
                kill_reason(&[target]),
                Some("kill 0 signals the caller's process group".into())
            );
        }
    }

    #[test]
    fn python_digit_limit_crash_has_an_explicit_safe_decision() {
        let targets = ["7".repeat(4301), "٠".repeat(4301)];
        let Some(errors): Option<Vec<String>> = crate::oracle::python(
            r#"
import json,sys
from scripts.lane_safety_guard import _kill_reason
errors=[]
for target in json.load(sys.stdin):
    try:
        _kill_reason([target])
        errors.append('no error')
    except ValueError as error:
        errors.append(type(error).__name__)
print(json.dumps(errors))
"#,
            &serde_json::json!(targets),
        ) else {
            return;
        };
        assert_eq!(errors, ["ValueError", "ValueError"]);
        assert_eq!(kill_reason(&targets[..1]), None);
        assert_eq!(
            kill_reason(&targets[1..]).as_deref(),
            Some("kill 0 signals the caller's process group")
        );
    }

    #[test]
    fn unwrap_matches_python_for_newlines_empty_arguments_and_unicode_options() {
        let cases = vec![
            vec!["A=1\n2", "kill", "0"],
            vec!["A=1\n", "kill", "0"],
            vec!["A=1\r2", "kill", "42"],
            vec!["env", "A=1\n2", "kill", "42"],
            vec!["sudo", "-é", "kill", "42"],
            vec!["timeout", "--signal"],
            vec!["", "kill"],
        ];
        let Some(expected): Option<Vec<(Vec<String>,bool)>> = crate::oracle::python(
            "import json,sys; from scripts.lane_safety_guard import _unwrap; print(json.dumps([_unwrap(x) for x in json.load(sys.stdin)]))", &serde_json::json!(cases)
        ) else { return; };
        for (words, expected) in cases.into_iter().zip(expected) {
            let words: Vec<String> = words.into_iter().map(str::to_owned).collect();
            let (remaining, xargs) = unwrap_command(&words);
            assert_eq!((remaining.to_vec(), xargs), expected);
        }
    }

    #[test]
    fn literal_pids_and_signal_listing_are_allowed() {
        for command in [
            "",
            "42",
            "-9 42 43",
            "-s TERM 42",
            "--signal=TERM 42",
            "-l",
            "-L",
            "-t",
            "٩٩",
        ] {
            assert_eq!(kill_reason(&words(command)), None, "{command}");
        }
    }

    #[test]
    fn group_and_expansion_refusals() {
        for command in ["0", "-9 0", "٠٠"] {
            assert_eq!(
                kill_reason(&words(command)).as_deref(),
                Some("kill 0 signals the caller's process group")
            );
        }
        for command in ["-- -42", "-9 -42", "-s TERM -42", "-TERM -42 -43", "-9 -²"] {
            assert_eq!(
                kill_reason(&words(command)).as_deref(),
                Some("kill of a process group")
            );
        }
        for target in ["$PID", "${PID}", "%1", "abc", "$(…)"] {
            assert_eq!(
                kill_reason(&[target.into()]),
                Some(format!("kill target '{target}' is not a literal PID"))
            );
        }
    }

    #[test]
    fn unwraps_nested_wrappers_and_consumes_option_values() {
        for command in [
            "sudo --user root env -u X timeout -s TERM 5 kill 42",
            "if X=1 /usr/bin/nice --adjustment=5 nohup kill 42",
            "stdbuf -o L kill 42",
            "doas -u root kill 42",
        ] {
            let words = words(command);
            let (remaining, via_xargs) = unwrap_command(&words);
            assert_eq!(remaining, &["kill", "42"], "{command}");
            assert!(!via_xargs);
        }
        let words = words("xargs -n1 kill -9");
        assert_eq!(unwrap_command(&words), (&words[2..], true));
    }

    #[test]
    fn missing_wrapper_arguments_stay_empty() {
        for command in ["sudo -u", "timeout", "env --chdir", "if X=1"] {
            assert!(unwrap_command(&words(command)).0.is_empty());
        }
    }
    #[test]
    fn decisions_match_python_source_on_adversarial_argv() {
        use serde_json::json;
        let mut cases = Vec::new();
        for signal in ["", "-9", "-TERM", "-s TERM", "--signal=TERM", "--"] {
            for target in [
                "42", "0", "-- -42", "-42", "$PID", "${PID}", "%1", "12 abc", "٠٠", "١٢", "²",
                "'quoted'", "a\\b", "é", "\u{200b}",
            ] {
                cases.push(words(&format!("{signal} {target}")));
            }
        }
        cases.extend([
            vec!["99999999999999999999".into()],
            vec!["7".repeat(4300)],
            vec!["0".repeat(4300)],
            vec!["٠".repeat(4300)],
            vec![format!("{}١", "٠".repeat(4299))],
        ]);
        let Some(expected): Option<Vec<Option<String>>> = crate::oracle::python(
            "import json,sys; from scripts.lane_safety_guard import _kill_reason; print(json.dumps([_kill_reason(a) for a in json.load(sys.stdin)]))", &json!(cases)
        ) else { return; };
        for (argv, expected) in cases.iter().zip(expected) {
            assert_eq!(kill_reason(argv), expected, "{argv:?}");
        }
    }
    #[test]
    fn decoded_hook_output_matches_actual_source_main() {
        let events = [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"tool_name":"Read","tool_input":{"command":"pkill x"}}),
            serde_json::json!({"tool_name":"Bash","tool_input":{"command":false}}),
            serde_json::json!({"tool_name":"Bash","tool_input":{"command":"echo safe"}}),
            serde_json::json!({"tool_name":"Bash","tool_input":{"command":"kill 😀"}}),
            serde_json::json!({"tool_name":"Bash","tool_input":{"command":"pkill x"}}),
        ];
        let Some(expected):Option<Vec<(String,String,u8)>>=crate::oracle::python(
            "import json,sys,subprocess; events=json.load(sys.stdin); print(json.dumps([(r.stdout,r.stderr,r.returncode) for event in events for r in [subprocess.run([sys.executable,'scripts/lane_safety_guard.py'],input=json.dumps(event),capture_output=True,text=True,timeout=10)]]))", &serde_json::json!(events)
        ) else {return;};
        for (event, (stdout, stderr, exit_code)) in events.iter().zip(expected) {
            assert_eq!(
                hook_input(&serde_json::to_vec(event).unwrap()).unwrap(),
                crate::CommandOutput {
                    stdout,
                    stderr,
                    exit_code
                }
            );
        }
        assert!(matches!(
            hook_input(b"{\"ignored\":18446744073709551616}"),
            Err(HookError::Decode(
                provenant_records::DecodeError::NumberOutOfRange(_)
            ))
        ));
    }
}
