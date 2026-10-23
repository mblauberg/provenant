//! Textual JS/TS timebomb policy. Python AST analysis is intentionally pending.
use std::collections::BTreeSet;
pub struct JavascriptPolicy {
    date: regex::Regex,
    test: regex::Regex,
}
impl JavascriptPolicy {
    pub fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            date: regex::Regex::new(
                r#"['"](\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?)['"]"#,
            )?,
            test: regex::Regex::new(
                r"(?:it|test)[\s\x1c-\x1f]*(?:\.each\([^\n]*\)[\s\x1c-\x1f]*)?\(",
            )?,
        })
    }
    pub fn scan(&self, source: &str) -> Vec<(usize, String)> {
        if !source.contains("Date.now(") && !source.contains("new Date()") {
            return vec![];
        }
        crate::pending::python_splitlines(source)
            .into_iter()
            .enumerate()
            .filter(|(_, line)| !line.contains("timebomb: allow"))
            .flat_map(|(i, line)| {
                self.date.captures_iter(line).filter_map(move |capture| {
                    let value = capture.get(1)?.as_str();
                    // Pin Python's Unicode decimal property, independently of the
                    // Unicode version compiled into regex.
                    value
                        .chars()
                        .all(|c| c.is_ascii() || crate::pending::decimal_digit(c).is_some())
                        .then(|| (i + 1, value.to_owned()))
                })
            })
            .collect()
    }
    pub fn context_changed(&self, source: &str, line: usize, added: &BTreeSet<usize>) -> bool {
        let lines = crate::pending::python_splitlines(source);
        let starts: Vec<_> = lines
            .iter()
            .enumerate()
            .filter(|(_, text)| {
                self.test.find_iter(text).any(|m| {
                    text[..m.start()]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !crate::pending::python_word_character(c))
                })
            })
            .map(|(i, _)| i + 1)
            .collect();
        let start = starts
            .iter()
            .copied()
            .filter(|&s| s <= line)
            .max()
            .unwrap_or(0);
        let end = starts
            .iter()
            .copied()
            .filter(|&s| s > line)
            .min()
            .map_or(lines.len(), |s| s - 1);
        if start == 0 {
            !added.is_empty()
        } else {
            added.range(start..=end.max(start)).next().is_some()
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn javascript_date_and_context_policy_matches_source() {
        let dir = tempfile::tempdir().unwrap();
        let sources = [
            "const fixture = '2026-10-23';\n",
            "// new Date()\nconst fixture = '2026-99-99';\n",
            "const now = Date.now();\nconst a='2026-10-23T12:34:56.1+0100'; const b=\"2026-10-24\";\n",
            "const now=Date.now();\nconst allowed='2026-10-23'; // timebomb: allow fixed\nconst unicode='٢٠٢٦-١٠-٢٣';\n",
            "const date='2026-10-23';\nit('clock', () => {\n Date.now();\n});\ntest.each([1,2])('next',()=>{\n const next='2026-10-24';\n});\n",
            "Date.now();\nétest('not boundary', ()=>{});\n_test('not boundary',()=>{});\ntest .each([1])(\"date\",()=>{});\n",
        ];
        let policy = JavascriptPolicy::new().unwrap();
        for source in sources {
            let path = dir.path().join("clock.test.ts");
            std::fs::write(&path, source).unwrap();
            let mut added = vec![BTreeSet::new()];
            for i in 1..=8 {
                added.push(BTreeSet::from([i]));
            }
            added.push(BTreeSet::from([1, 5, 7]));
            type Oracle = (Vec<(usize, String)>, Vec<bool>);
            let Some(expected): Option<Oracle> = crate::oracle::python(
                r#"
import json,sys,importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location('timebomb_oracle',Path(sys.argv[1])/'scripts/lint-timebomb-fixtures.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
x=json.load(sys.stdin);p=Path(x['path']);print(json.dumps([m.scan_file(p),[m._context_changed(p,line,set(added)) for added in x['added'] for line in range(1,9)]]))
"#,
                &json!({"path":path,"added":added}),
            ) else {
                return;
            };
            assert_eq!(policy.scan(source), expected.0);
            let actual: Vec<_> = added
                .iter()
                .flat_map(|a| (1..9).map(|line| policy.context_changed(source, line, a)))
                .collect();
            assert_eq!(actual, expected.1, "{source}");
        }
    }
}
