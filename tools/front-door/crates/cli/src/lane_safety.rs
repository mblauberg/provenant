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

pub fn kill_reason(args: &[String]) -> Result<Option<String>, crate::pending::IntegerError> {
    let mut targets = Vec::new();
    let (mut options_done, mut signalled, mut i) = (false, false, 0);
    while let Some(word) = args.get(i) {
        if !options_done && word == "--" {
            options_done = true;
        } else if !options_done && matches!(word.as_str(), "-l" | "-L" | "-t") {
            return Ok(None);
        } else if !options_done && matches!(word.as_str(), "-s" | "-n" | "--signal") {
            signalled = true;
            i += 1;
        } else if !options_done && word.starts_with('-') && word.len() > 1 {
            if signalled && digits(&word[1..]) {
                return Ok(Some("kill of a process group".into()));
            }
            signalled = true;
        } else {
            targets.push(word);
        }
        i += 1;
    }
    for target in targets {
        if target.starts_with('-') {
            return Ok(Some("kill of a process group".into()));
        }
        if !decimal(target) {
            return Ok(Some(format!(
                "kill target {} is not a literal PID",
                crate::pending::python_repr(target)
            )));
        }
        if crate::pending::decimal_integer(target)? == 0 {
            return Ok(Some("kill 0 signals the caller's process group".into()));
        }
    }
    Ok(None)
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
                "<<EOF\nbody\nEOF",
                "echo \"\\\n$(kill 0)\"",
                "cat <<A <<B\nx\nA\ny\nB",
                "é=$(echo x) kill 42"
            ]),
        ) else {
            return;
        };
        for (text, parsed, _reason, _executable) in cases {
            let (commands, nested) = parse(&text);
            assert_eq!(
                serde_json::json!([commands, nested]),
                parsed,
                "parse {text:?}"
            );

        }
    }

    #[test]
    fn integer_bounds_are_typed_and_python_digit_limit_is_preserved() {
        assert!(matches!(
            kill_reason(&["7".repeat(4301)]),
            Err(crate::pending::IntegerError::DigitLimit { digits: 4301 })
        ));
        assert!(matches!(
            kill_reason(&["18446744073709551616".into()]),
            Err(crate::pending::IntegerError::NumberOutOfRange)
        ));
        assert_eq!(
            kill_reason(&["0".repeat(4300)]).unwrap(),
            Some("kill 0 signals the caller's process group".into())
        );
        assert_eq!(kill_reason(&[u64::MAX.to_string()]).unwrap(), None);
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
            assert_eq!(kill_reason(&words(command)).unwrap(), None, "{command}");
        }
    }

    #[test]
    fn group_and_expansion_refusals() {
        for command in ["0", "-9 0", "٠٠"] {
            assert_eq!(
                kill_reason(&words(command)).unwrap().as_deref(),
                Some("kill 0 signals the caller's process group")
            );
        }
        for command in ["-- -42", "-9 -42", "-s TERM -42", "-TERM -42 -43", "-9 -²"] {
            assert_eq!(
                kill_reason(&words(command)).unwrap().as_deref(),
                Some("kill of a process group")
            );
        }
        for target in ["$PID", "${PID}", "%1", "abc", "$(…)"] {
            assert_eq!(
                kill_reason(&[target.into()]).unwrap(),
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
        let Some(expected): Option<Vec<Option<String>>> = crate::oracle::python(
            "import json,sys; from scripts.lane_safety_guard import _kill_reason; print(json.dumps([_kill_reason(a) for a in json.load(sys.stdin)]))", &json!(cases)
        ) else { return; };
        for (argv, expected) in cases.iter().zip(expected) {
            assert_eq!(kill_reason(argv).unwrap(), expected, "{argv:?}");
        }
    }
}
