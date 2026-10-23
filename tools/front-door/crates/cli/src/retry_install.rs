//! npm retry policy and character-bounded output tail. Streaming child ownership
//! and sleeping remain with the supervisor/process edge; this module is inert.
use std::collections::VecDeque;
use thiserror::Error;

pub const MAX_OUTPUT_TAIL: usize = 64 * 1024;
pub const DELAYS: [u64; 2] = [2, 4];

#[derive(Debug, Default)]
pub struct OutputTail {
    characters: VecDeque<char>,
}
impl OutputTail {
    /// Python counts Unicode characters, rather than UTF-8 bytes, in its tail.
    pub fn append(&mut self, text: &str) {
        for c in text.chars() {
            self.characters.push_back(c);
            if self.characters.len() > MAX_OUTPUT_TAIL {
                self.characters.pop_front();
            }
        }
    }
    pub fn text(&self) -> String {
        self.characters.iter().collect()
    }
}

#[derive(Debug, Error)]
pub enum RetryError {
    #[error(transparent)]
    Pattern(#[from] regex::Error),
    #[error("npm retry attempt {0} is outside 0..=2")]
    InvalidAttempt(usize),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Exit(i32),
    Retry {
        delay_seconds: u64,
        diagnostic: String,
    },
}

pub struct RetryPolicy {
    reset: regex::Regex,
    error_prefix: regex::Regex,
    fetch_prefix: regex::Regex,
    get_status: regex::Regex,
}
impl RetryPolicy {
    pub fn new() -> Result<Self, RetryError> {
        let space = r"[\s\x1c-\x1f]";
        Ok(Self {
            reset: regex::Regex::new(&format!(
                r"npm{space}+(?:err!|error){space}+code{space}+econnreset"
            ))?,
            error_prefix: regex::Regex::new(&format!(r"npm{space}+(?:err!|error)"))?,
            fetch_prefix: regex::Regex::new(&format!(r"npm{space}+http{space}+fetch"))?,
            get_status: regex::Regex::new(&format!(r"get{space}+503"))?,
        })
    }
    fn bounded_token(text: &str, start: usize, end: usize) -> bool {
        text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !crate::pending::python_word_character(c))
            && text[end..]
                .chars()
                .next()
                .is_none_or(|c| !crate::pending::python_word_character(c))
    }
    pub fn retryable(&self, text: &str) -> bool {
        // Python ASCII IGNORECASE also accepts dotted/dotless I, long S and Kelvin K.
        let folded: String = text
            .chars()
            .map(|c| match c {
                'İ' | 'ı' => 'i',
                'ſ' => 's',
                'K' => 'k',
                c => c.to_ascii_lowercase(),
            })
            .collect();
        if self.reset.is_match(&folded) {
            return true;
        }
        for (prefix, fetch) in [(&self.error_prefix, false), (&self.fetch_prefix, true)] {
            for found in prefix.find_iter(&folded) {
                let rest = &folded[found.end()..];
                let tail = rest.split_once('\n').map_or(rest, |(line, _)| line);
                if tail.contains("503 service unavailable") || tail.contains("status 503") {
                    return true;
                }
                for (at, _) in tail.match_indices("e503") {
                    if Self::bounded_token(&folded, found.end() + at, found.end() + at + 4) {
                        return true;
                    }
                }
                if fetch {
                    for status in self.get_status.find_iter(tail) {
                        if Self::bounded_token(
                            &folded,
                            found.end() + status.start(),
                            found.end() + status.end(),
                        ) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
    pub fn decide(
        &self,
        attempt: usize,
        returncode: i32,
        output_tail: &str,
    ) -> Result<Decision, RetryError> {
        if attempt > DELAYS.len() {
            return Err(RetryError::InvalidAttempt(attempt));
        }
        if returncode == 0 || attempt == DELAYS.len() || !self.retryable(output_tail) {
            return Ok(Decision::Exit(returncode));
        }
        let delay = DELAYS[attempt];
        Ok(Decision::Retry {
            delay_seconds: delay,
            diagnostic: format!(
                "npm install hit a transient registry error; retrying in {delay}s\n"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tail_counts_characters_and_retains_the_latest_output() {
        let mut tail = OutputTail::default();
        tail.append(&"é".repeat(MAX_OUTPUT_TAIL));
        tail.append("🦀tail\n");
        assert_eq!(tail.text().chars().count(), MAX_OUTPUT_TAIL);
        assert_eq!(
            tail.text(),
            format!("{}🦀tail\n", "é".repeat(MAX_OUTPUT_TAIL - 6))
        );
        tail.append(&"x".repeat(MAX_OUTPUT_TAIL + 1));
        assert_eq!(tail.text(), "x".repeat(MAX_OUTPUT_TAIL));
    }
    #[test]
    fn retry_budget_and_source_signatures_match_python() {
        let policy = RetryPolicy::new().unwrap();
        let mut cases = vec![
            "npm ERR! code ECONNRESET".into(),
            "npm ERR! EACCES".into(),
            "package version 1.503.0".into(),
            "npm error 503 Service Unavailable".into(),
            "npm error code E503".into(),
            "npm error status 503".into(),
            "npm http fetch GET 503 https://registry.npmjs.org/x".into(),
            "npm error package version 1.503.0".into(),
        ];
        for prefix in [
            "npm error ",
            "npm ERR! ",
            "npm http fetch ",
            "NPM ERROR ",
            "npm\u{1c}error ",
            "npm ERR!",
        ] {
            for signature in [
                "E503",
                "E503\u{300}",
                "E503\u{345}",
                "E503x",
                "xE503",
                "status 503",
                "503 Servİce Unavaılable",
                "GET 503",
                "GET 503\u{300}",
                "GET 503x",
                "\nE503",
                "code ECONNRESET",
            ] {
                cases.push(format!("{prefix}{signature}"));
            }
        }
        let Some(expected): Option<Vec<bool>> = crate::oracle::python(
            r#"
import json,sys,importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location('retry_oracle',Path('scripts/retry-npm-install.py'));m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
print(json.dumps([bool(m.RETRYABLE.search(s)) for s in json.load(sys.stdin)]))
"#,
            &serde_json::json!(cases),
        ) else {
            return;
        };
        assert_eq!(cases.len(), expected.len());
        for (case, expected) in cases.iter().zip(expected) {
            assert_eq!(policy.retryable(case), expected, "{case:?}");
        }
        for attempt in 0..=2 {
            assert_eq!(
                policy
                    .decide(attempt, 0, "npm ERR! code ECONNRESET")
                    .unwrap(),
                Decision::Exit(0)
            );
            assert_eq!(
                policy.decide(attempt, 1, "npm ERR! EACCES").unwrap(),
                Decision::Exit(1)
            );
        }
        assert_eq!(
            policy.decide(2, 17, "npm ERR! code ECONNRESET").unwrap(),
            Decision::Exit(17)
        );
        for (attempt, delay) in DELAYS.into_iter().enumerate() {
            assert_eq!(
                policy
                    .decide(attempt, 1, "npm ERR! code ECONNRESET")
                    .unwrap(),
                Decision::Retry {
                    delay_seconds: delay,
                    diagnostic: format!(
                        "npm install hit a transient registry error; retrying in {delay}s\n"
                    )
                }
            );
        }
        assert!(matches!(
            policy.decide(3, 1, ""),
            Err(RetryError::InvalidAttempt(3))
        ));
    }
}
