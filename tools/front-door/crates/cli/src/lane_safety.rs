//! Pure wrapper and literal-PID policy. Shell parsing and hook entry remain Python-owned.

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
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
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
                crate::pending::python_repr(target)
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

#[cfg(test)]
mod tests {
    use super::*;
    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
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
            "99999999999999999999999999999999999999999999999999",
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
}
