//! Marked-document projection with Python-compatible unified reports.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(
        "{name} needs exactly one {start_marker} / {end_marker} pair, found {starts} start and {ends} end markers"
    )]
    MarkerCount {
        name: String,
        start_marker: String,
        end_marker: String,
        starts: usize,
        ends: usize,
    },
    #[error("{name} has {end_marker} before {start_marker}")]
    MarkerOrder {
        name: String,
        start_marker: String,
        end_marker: String,
    },
    #[error("projection UTF-8 decode at {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("projection I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn split_marked_region<'a>(
    text: &'a str,
    marker: &str,
    name: &str,
) -> Result<(&'a str, &'a str, &'a str), Error> {
    let start_marker = format!("<!-- {marker}:start -->");
    let end_marker = format!("<!-- {marker}:end -->");
    let starts = text.matches(&start_marker).count();
    let ends = text.matches(&end_marker).count();
    if starts != 1 || ends != 1 {
        return Err(Error::MarkerCount {
            name: name.into(),
            start_marker,
            end_marker,
            starts,
            ends,
        });
    }
    let start = text.find(&start_marker).unwrap_or(0) + start_marker.len();
    let end = text.find(&end_marker).unwrap_or(0);
    if end < start {
        return Err(Error::MarkerOrder {
            name: name.into(),
            start_marker,
            end_marker,
        });
    }
    Ok((&text[..start], &text[start..end], &text[end..]))
}

pub fn project(
    path: &Path,
    marker: &str,
    rendered: &str,
    check: bool,
    source: Option<&str>,
) -> Result<Vec<String>, Error> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    let current = String::from_utf8(bytes)
        .map_err(|source| Error::Decode {
            path: path.into(),
            source,
        })?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    split_marked_region(&current, marker, &name)?;
    split_marked_region(rendered, marker, &format!("rendered {name}"))?;
    if current == rendered {
        return Ok(Vec::new());
    }
    let rendered_label = source
        .filter(|s| !s.is_empty())
        .map(|s| format!("(rendered from {s})"))
        .unwrap_or_else(|| "(rendered)".into());
    let report = unified_diff(
        &current,
        rendered,
        &format!("{name} (on disk)"),
        &format!("{name} {rendered_label}"),
    );
    if !check {
        fs::write(path, rendered).map_err(|source| Error::Io {
            path: path.into(),
            source,
        })?;
    }
    Ok(report)
}

fn lines(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            let mut end = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, c)| *c == '\n') {
                iter.next();
                end += 1;
            }
            result.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        result.push(&text[start..]);
    }
    result
}

#[derive(Clone, Copy, Debug)]
struct Opcode {
    tag: char,
    a: usize,
    ae: usize,
    b: usize,
    be: usize,
}

/// SequenceMatcher's longest contiguous match, including its popular-line rule.
/// This determines observable hunk selection when repeated lines admit many diffs.
fn opcodes(a: &[&str], b: &[&str]) -> Vec<Opcode> {
    let mut index: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (j, line) in b.iter().enumerate() {
        index.entry(line).or_default().push(j);
    }
    if b.len() >= 200 {
        index.retain(|_, positions| positions.len() <= b.len() / 100 + 1);
    }
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut matches = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (mut ai, mut bj, mut size) = (alo, blo, 0);
        let mut previous = BTreeMap::new();
        for (i, line) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut current = BTreeMap::new();
            if let Some(positions) = index.get(line) {
                for &j in positions.iter().filter(|&&j| j >= blo && j < bhi) {
                    let length = j
                        .checked_sub(1)
                        .and_then(|j| previous.get(&j))
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    current.insert(j, length);
                    if length > size {
                        ai = i + 1 - length;
                        bj = j + 1 - length;
                        size = length;
                    }
                }
            }
            previous = current;
        }
        while ai > alo && bj > blo && a[ai - 1] == b[bj - 1] {
            ai -= 1;
            bj -= 1;
            size += 1;
        }
        while ai + size < ahi && bj + size < bhi && a[ai + size] == b[bj + size] {
            size += 1;
        }
        if size > 0 {
            matches.push((ai, bj, size));
            if alo < ai && blo < bj {
                queue.push((alo, ai, blo, bj));
            }
            if ai + size < ahi && bj + size < bhi {
                queue.push((ai + size, ahi, bj + size, bhi));
            }
        }
    }
    matches.sort_unstable();
    let mut merged: Vec<(usize, usize, usize)> = Vec::new();
    for (ai, bj, size) in matches {
        if let Some(last) = merged.last_mut() {
            if last.0 + last.2 == ai && last.1 + last.2 == bj {
                last.2 += size;
                continue;
            }
        }
        merged.push((ai, bj, size));
    }
    merged.push((a.len(), b.len(), 0));
    let (mut i, mut j) = (0, 0);
    let mut codes = Vec::new();
    for (ai, bj, size) in merged {
        let tag = if i < ai && j < bj {
            'r'
        } else if i < ai {
            'd'
        } else {
            'i'
        };
        if i < ai || j < bj {
            codes.push(Opcode {
                tag,
                a: i,
                ae: ai,
                b: j,
                be: bj,
            });
        }
        if size > 0 {
            codes.push(Opcode {
                tag: '=',
                a: ai,
                ae: ai + size,
                b: bj,
                be: bj + size,
            });
        }
        i = ai + size;
        j = bj + size;
    }
    codes
}

fn range(start: usize, end: usize) -> String {
    match end - start {
        0 => format!("{start},0"),
        1 => (start + 1).to_string(),
        length => format!("{},{length}", start + 1),
    }
}

fn unified_diff(before: &str, after: &str, from: &str, to: &str) -> Vec<String> {
    let a = lines(before);
    let b = lines(after);
    let mut codes = opcodes(&a, &b);
    if let Some(first) = codes.first_mut().filter(|c| c.tag == '=') {
        first.a = first.ae.saturating_sub(3);
        first.b = first.be.saturating_sub(3);
    }
    if let Some(last) = codes.last_mut().filter(|c| c.tag == '=') {
        last.ae = (last.a + 3).min(last.ae);
        last.be = (last.b + 3).min(last.be);
    }
    let mut groups = Vec::new();
    let mut group = Vec::new();
    for mut code in codes {
        if code.tag == '=' && code.ae - code.a > 6 {
            group.push(Opcode {
                ae: code.a + 3,
                be: code.b + 3,
                ..code
            });
            groups.push(std::mem::take(&mut group));
            code.a = code.ae - 3;
            code.b = code.be - 3;
        }
        group.push(code);
    }
    if !(group.is_empty() || group.len() == 1 && group[0].tag == '=') {
        groups.push(group);
    }
    let mut report = Vec::new();
    if groups.is_empty() {
        return report;
    }
    report.push(format!("--- {from}\n"));
    report.push(format!("+++ {to}\n"));
    for group in groups {
        let first = group[0];
        let last = group[group.len() - 1];
        report.push(format!(
            "@@ -{} +{} @@\n",
            range(first.a, last.ae),
            range(first.b, last.be)
        ));
        for code in group {
            if code.tag == '=' {
                report.extend(a[code.a..code.ae].iter().map(|line| format!(" {line}")));
            }
            if matches!(code.tag, 'r' | 'd') {
                report.extend(a[code.a..code.ae].iter().map(|line| format!("-{line}")));
            }
            if matches!(code.tag, 'r' | 'i') {
                report.extend(b[code.b..code.be].iter().map(|line| format!("+{line}")));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    const DOC: &str = "# Title\n\n<!-- sample:start -->\nold body\n<!-- sample:end -->\n\ntail\n";

    #[test]
    fn invalid_utf8_is_a_decode_error_and_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        fs::write(&path, b"\xff").unwrap();
        let error = project(&path, "sample", DOC, false, None).unwrap_err();
        assert!(!matches!(error, Error::Io { .. }));
        assert_eq!(fs::read(path).unwrap(), b"\xff");
    }

    #[test]
    fn split_partitions_and_requires_exactly_one_ordered_pair() {
        let (head, body, tail) = split_marked_region(DOC, "sample", "document").unwrap();
        assert_eq!(format!("{head}{body}{tail}"), DOC);
        assert_eq!(body, "\nold body\n");
        for text in [
            DOC.replace("sample:", "other:"),
            DOC.replace("<!-- sample:start -->", ""),
            DOC.replace("<!-- sample:end -->", ""),
            format!("{DOC}{DOC}"),
            "<!-- sample:end --><!-- sample:start -->".into(),
        ] {
            assert!(split_marked_region(&text, "sample", "README").is_err());
        }
    }

    #[test]
    fn check_does_not_write_and_invalid_rendering_cannot_destroy_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        fs::write(&path, DOC).unwrap();
        assert!(
            project(&path, "sample", DOC, true, None)
                .unwrap()
                .is_empty()
        );
        let rendered = DOC.replace("old body", "new body");
        let report = project(&path, "sample", &rendered, true, Some("skills/")).unwrap();
        assert_eq!(report[0], "--- doc.md (on disk)\n");
        assert_eq!(report[1], "+++ doc.md (rendered from skills/)\n");
        assert!(report.contains(&"-old body\n".into()));
        assert!(report.contains(&"+new body\n".into()));
        assert_eq!(fs::read_to_string(&path).unwrap(), DOC);
        assert!(project(&path, "sample", "missing", false, None).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), DOC);
        project(&path, "sample", &rendered, false, None).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), rendered);
    }

    #[test]
    fn missing_disk_markers_never_opt_out_of_drift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        fs::write(&path, "missing").unwrap();
        for check in [true, false] {
            assert!(project(&path, "sample", DOC, check, None).is_err());
        }
        assert_eq!(fs::read_to_string(path).unwrap(), "missing");
    }

    #[test]
    fn reports_match_python_difflib_on_repeated_lines_and_hunk_boundaries() {
        let mut cases = vec![
            ("".to_owned(), "x".to_owned()),
            ("x\ny\n".into(), "y\nx\n".into()),
            ("a\u{2028}b\n".into(), "a\u{2028}c\n".into()),
        ];
        for size in [1, 7, 20, 199, 200, 250] {
            let before = (0..size)
                .map(|i| format!("{}\n", i % 5))
                .collect::<String>();
            for pos in [0, size / 2, size - 1] {
                let mut after: Vec<_> = before.split_inclusive('\n').map(str::to_owned).collect();
                after[pos] = "changed\n".into();
                cases.push((before.clone(), after.concat()));
            }
        }
        // Deterministic generated fixtures exercise non-minimal SequenceMatcher choices.
        let mut seed = 17u64;
        for _ in 0..100 {
            let mut make = || {
                (0..30)
                    .map(|_| {
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                        format!("{}\n", seed >> 61)
                    })
                    .collect::<String>()
            };
            cases.push((make(), make()));
        }
        let Some(expected): Option<Vec<Vec<String>>> = crate::oracle::python(
            "import difflib,json,sys; print(json.dumps([list(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='old',tofile='new')) for a,b in json.load(sys.stdin)]))",
            &serde_json::json!(cases),
        ) else {
            return;
        };
        for ((before, after), expected) in cases.iter().zip(expected) {
            assert_eq!(
                unified_diff(before, after, "old", "new"),
                expected,
                "{before:?} => {after:?}"
            );
        }
    }
    #[test]
    fn project_matches_python_source_for_validation_reporting_and_write_effects() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        for current in [DOC.to_owned(), DOC.replace('\n', "\r\n"), "missing".into()] {
            for rendered in [
                DOC.to_owned(),
                DOC.replace("old body", "é new body"),
                "missing".into(),
            ] {
                for check in [true, false] {
                    fs::write(&path, &current).unwrap();
                    let Some(expected): Option<serde_json::Value> = crate::oracle::python(
                        "import json,sys; from pathlib import Path; from scripts.lib.projection import project,ProjectionError; x=json.load(sys.stdin); p=Path(x['path']); p.write_bytes(x['current'].encode());\ntry: r={'report':project(p,'sample',x['rendered'],x['check'],source='skills/')}\nexcept ProjectionError as e: r={'error':str(e)}\nr['bytes']=list(p.read_bytes()); print(json.dumps(r))",
                        &json!({"path":path, "current":current, "rendered":rendered, "check":check}),
                    ) else {
                        return;
                    };
                    fs::write(&path, &current).unwrap();
                    let actual = match project(&path, "sample", &rendered, check, Some("skills/")) {
                        Ok(report) => json!({"report":report,"bytes":fs::read(&path).unwrap()}),
                        Err(error) => {
                            json!({"error":error.to_string(),"bytes":fs::read(&path).unwrap()})
                        }
                    };
                    assert_eq!(actual, expected);
                }
            }
        }
    }
}
