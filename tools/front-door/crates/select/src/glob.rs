//! A tiny, conservative glob matcher for `global_paths`.
//!
//! Supported syntax is `*` (within one path component), `**` (across
//! components), `?` (one non-separator character) and literals. Any other
//! metacharacter is refused so a pattern cannot be silently under-matched; the
//! caller then forces a full run instead.

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Literal(char),
    AnyChar,
    Star,
    GlobStar,
}

#[derive(Clone, Debug)]
pub(super) struct Glob {
    tokens: Vec<Token>,
}

impl Glob {
    pub(super) fn parse(pattern: &str) -> Result<Self, String> {
        let mut input = pattern.chars().peekable();
        let mut tokens = Vec::new();
        while let Some(character) = input.next() {
            match character {
                '*' => {
                    if input.peek() == Some(&'*') {
                        input.next();
                        while input.peek() == Some(&'*') {
                            input.next();
                        }
                        tokens.push(Token::GlobStar);
                    } else {
                        tokens.push(Token::Star);
                    }
                }
                '?' => tokens.push(Token::AnyChar),
                '[' | ']' | '{' | '}' | '\\' => {
                    return Err(format!("unsupported glob metacharacter {character:?}"));
                }
                literal => tokens.push(Token::Literal(literal)),
            }
        }
        Ok(Self { tokens })
    }

    /// Whether the whole path matches; a partial match is not a match.
    pub(super) fn is_match(&self, path: &str) -> bool {
        let characters: Vec<char> = path.chars().collect();
        matches(&self.tokens, &characters)
    }
}

fn matches(tokens: &[Token], text: &[char]) -> bool {
    match tokens.split_first() {
        None => text.is_empty(),
        Some((Token::Literal(expected), rest)) => {
            text.first() == Some(expected) && matches(rest, &text[1..])
        }
        Some((Token::AnyChar, rest)) => {
            text.first().is_some_and(|character| *character != '/') && matches(rest, &text[1..])
        }
        Some((Token::Star, rest)) => {
            let mut end = 0;
            loop {
                if matches(rest, &text[end..]) {
                    return true;
                }
                if end == text.len() || text[end] == '/' {
                    return false;
                }
                end += 1;
            }
        }
        Some((Token::GlobStar, rest)) => (0..=text.len()).any(|end| matches(rest, &text[end..])),
    }
}

#[cfg(test)]
mod tests {
    use super::Glob;

    fn matches(pattern: &str, path: &str) -> bool {
        Glob::parse(pattern)
            .expect("supported pattern")
            .is_match(path)
    }

    #[test]
    fn literals_match_the_whole_path_only() {
        assert!(matches("ci/pipeline.yml", "ci/pipeline.yml"));
        assert!(!matches("ci/pipeline.yml", "x/ci/pipeline.yml"));
        assert!(!matches("ci/pipeline.yml", "ci/pipeline.yaml"));
    }

    #[test]
    fn star_stays_within_one_component() {
        assert!(matches("src/*.rs", "src/lib.rs"));
        assert!(!matches("src/*.rs", "src/sub/lib.rs"));
        assert!(matches("src/a?c.rs", "src/abc.rs"));
        assert!(!matches("src/a?c.rs", "src/a/c.rs"));
    }

    #[test]
    fn glob_star_crosses_components() {
        assert!(matches("ci/**", "ci/pipeline.yml"));
        assert!(matches("ci/**", "ci/a/b/pipeline.yml"));
        assert!(matches("**/*.yml", "ci/pipeline.yml"));
        assert!(!matches("ci/**.rs", "ci/a/b/pipeline.yml"));
    }

    #[test]
    fn unsupported_syntax_is_rejected() {
        for pattern in ["src/[ab].rs", "src/{a,b}.rs", "src\\a.rs", "a]b"] {
            assert!(Glob::parse(pattern).is_err(), "{pattern}");
        }
    }
}
