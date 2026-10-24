#![allow(clippy::unwrap_used)]
use super::*;
fn matcher(set: RuleSet) -> Matcher {
    let bytes = match set {
        RuleSet::Legal => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../skills/legal-writing/lint-rules.json"
        ))
        .as_slice(),
        _ => include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../skills/natural-writing/lint-rules.json"
        ))
        .as_slice(),
    };
    Matcher::from_json(bytes, set).unwrap()
}
#[test]
fn rule_sets_retain_vocabulary_and_legal_severity() {
    for set in [RuleSet::General, RuleSet::Academic, RuleSet::Legal] {
        let found = matcher(set)
            .scan(Path::new("draft.md"), "é — robust\n")
            .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].line, found[0].column, found[0].start, found[0].end),
            (1, 3, 3, 6)
        );
        assert_eq!(
            found[0].severity,
            if set == RuleSet::Legal {
                Severity::Blocker
            } else {
                Severity::Warning
            }
        );
        assert_eq!(found[1].severity, Severity::Warning);
    }
    let m = matcher(RuleSet::Academic);
    assert_eq!(
        m.scan(Path::new("a.tex"), "this thesis report ---\n")
            .unwrap()
            .len(),
        2
    );
    assert!(
        matcher(RuleSet::General)
            .scan(
                Path::new("a.md"),
                "state-of-the-art highly-fledged robustness\n"
            )
            .unwrap()
            .is_empty()
    );
}
#[test]
fn exclusions_preserve_positions_and_surrounding_prose() {
    let m = matcher(RuleSet::Academic);
    for source in [
        "```rust\nrobust\n```\n",
        "~~~~\nrobust\n~~~~\n",
        "`robust`",
        "``a ` robust``",
        "---\ntitle: robust\n---\n",
        "[label](https://robust)",
        "https://example/robust",
        r"\cite[robust]{nested{robust}}",
        r"\texttt{robust}",
        r"\verb|robust|",
        r"\(robust\)",
        r"\[robust\]",
        "$robust$",
        "$$robust$$",
        r"\begin{align*}robust\end{align*}",
        r"\begin{verbatim}robust\end{verbatim}",
        "% robust\n",
    ] {
        assert!(
            m.scan(Path::new("a.tex"), source).unwrap().is_empty(),
            "{source}"
        );
        let tail = format!("{source}\nrobust");
        assert_eq!(
            m.scan(Path::new("a.tex"), &tail).unwrap().len(),
            1,
            "{source}"
        );
    }
    assert_eq!(
        m.scan(Path::new("a.tex"), r"\emph{robust} \href{robust}{robust}")
            .unwrap()
            .len(),
        2
    );
    assert!(m.scan(Path::new("a.md"), "```\nrobust").unwrap().is_empty());
    assert_eq!(
        m.scan(Path::new("a.md"), "😀é\nα robust").unwrap()[0].column,
        3
    );
}
#[test]
fn legal_structural_blockers_and_quoted_examples_are_data() {
    let m = matcher(RuleSet::Legal);
    let text = "Prose.\n# Heading\n\n<!-- /Users/example/private -->\n\n> TODO\n";
    let hits = m.scan(Path::new("filing.md"), text).unwrap();
    assert!(
        hits.iter()
            .any(|f| f.rule == "legal.heading-gap" && f.severity == Severity::Blocker)
    );
    assert!(
        hits.iter()
            .any(|f| f.rule == "legal.comment.3" && f.severity == Severity::Blocker)
    );
    let examples = m
        .scan(
            Path::new("skills/legal-writing/references/examples.md"),
            text,
        )
        .unwrap();
    assert_eq!(examples.len(), 1);
    let long = format!("{}end.", "word ".repeat(61));
    assert!(
        m.scan(Path::new("filing.md"), &long)
            .unwrap()
            .iter()
            .any(|f| f.rule == "legal.long-sentence" && f.severity == Severity::Warning)
    );
}
#[test]
fn malformed_latex_and_excluded_words_cannot_hide_structural_findings() {
    let academic = matcher(RuleSet::Academic);
    for source in [r"\begin{é", r"\cite{robust", r"\(robust", r"\texttt{robust"] {
        assert!(
            academic
                .scan(Path::new("x.tex"), source)
                .unwrap()
                .is_empty()
        );
    }
    let legal = matcher(RuleSet::Legal);
    let source = format!("Prose.\n# `Title`\n\n{} `robust` end.", "word ".repeat(61));
    let found = legal.scan(Path::new("draft.md"), &source).unwrap();
    assert!(found.iter().any(|f| f.rule == "legal.heading-gap"));
    assert!(found.iter().any(|f| f.rule == "legal.long-sentence"));
}
#[test]
fn legal_quoted_examples_exclude_fenced_structural_checks() {
    let source = "```markdown\nProse.\n# Heading\n<!-- /Users/example/private -->\n```\n";
    assert!(
        matcher(RuleSet::Legal)
            .scan(
                Path::new("skills/legal-writing/references/examples.md"),
                source
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn quoted_example_path_prefix_requires_a_path_component_boundary() {
    let mut m = matcher(RuleSet::Legal);
    m.quoted_paths = vec!["docs/ex".into()];
    let source = "<!-- /Users/example/private -->";
    assert!(
        m.scan(Path::new("docs/examples-x/draft.md"), source)
            .unwrap()
            .iter()
            .any(|finding| finding.rule == "legal.comment.3")
    );
}

#[test]
fn declared_minted_delimiters_exclude_the_payload_and_keep_following_prose() {
    let m = matcher(RuleSet::Academic);
    for source in [
        r"\mintinline{python}|robust|",
        r"\mintinline[breaklines]{rust}+robust+",
        r"\verb*|robust|",
        r"\lstinline[language=rust]!robust!",
    ] {
        let text = format!("{source} robust");
        let findings = m.scan(Path::new("chapter.tex"), &text).unwrap();
        assert_eq!(findings.len(), 1, "{source}");
        assert_eq!(findings[0].start, source.len() + 1);
    }
}

#[test]
fn academic_unescaped_percent_warns_only_when_number_starts_in_prose() {
    let m = matcher(RuleSet::Academic);
    let found = m
        .scan(Path::new("chapter.tex"), "At 50% confidence.\n")
        .unwrap();
    assert_eq!(found.len(), 1);
    for source in [
        r"At 50\% confidence.",
        "$50%$",
        r"\texttt{50%}",
        "`50%`",
        "% 50% comment\n",
    ] {
        assert!(
            m.scan(Path::new("chapter.tex"), source).unwrap().is_empty(),
            "{source}"
        );
    }
}

#[test]
fn inline_code_cannot_hide_following_lines_or_open_a_fence() {
    let m = matcher(RuleSet::General);
    for source in [
        "Press the ` key.\n\nThis is robust.\n\nAnother ` here.",
        "```inline``` code.\n\nThis is robust.",
        "    ```\nThis is robust.",
    ] {
        assert_eq!(
            m.scan(Path::new("draft.md"), source).unwrap().len(),
            1,
            "{source}"
        );
    }
}

#[test]
fn ordinary_legal_files_report_blockers_inside_markdown() {
    let m = matcher(RuleSet::Legal);
    let source =
        "See `.work/notes.md`. A dash `—`.\n\n```\nTODO fix\n```\n\n<!-- `/Users/me/secret` -->";
    let hits = m.scan(Path::new("filing.md"), source).unwrap();
    for id in [
        "legal.blocker.013",
        "legal.blocker.001",
        "legal.blocker.016",
        "legal.comment.3",
    ] {
        assert!(hits.iter().any(|f| f.rule == id), "{id}");
    }
    assert!(
        m.scan(
            Path::new("skills/legal-writing/references/examples.md"),
            source
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn match_start_controls_exclusions_even_when_a_url_is_inside_the_match() {
    let hits = matcher(RuleSet::Legal)
        .scan(
            Path::new("skills/legal-writing/references/draft.md"),
            "The protection order at https://x.au/o proves domestic violence.",
        )
        .unwrap();
    assert!(hits.iter().any(|f| f.rule == "legal.blocker.022"));
}

#[test]
fn legal_crlf_line_endings_preserve_end_anchored_blockers() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("skills/legal-writing")).unwrap();
    std::fs::write(
        temp.path().join("skills/legal-writing/lint-rules.json"),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../skills/legal-writing/lint-rules.json"
        )),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("draft.md"),
        b"Draft notes in .work\r\nNext line.\r\n",
    )
    .unwrap();
    let scan = inspect(
        temp.path(),
        temp.path(),
        &["draft.md".into()],
        RuleSet::Legal,
    )
    .unwrap();
    assert!(
        scan.findings
            .iter()
            .any(|f| f.finding.rule == "legal.blocker.013")
    );
}
