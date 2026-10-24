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
fn unsupported_patterns_duplicate_ids_and_bounds_are_refused() {
    let mut value: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../skills/natural-writing/lint-rules.json"
    )))
    .unwrap();
    value["shared"][0]["kind"] = "regex".into();
    for pattern in ["(?=lookahead)", r"(a)\1", "a*"] {
        value["shared"][0]["pattern"] = pattern.into();
        assert!(matches!(
            Matcher::from_json(&serde_json::to_vec(&value).unwrap(), RuleSet::General),
            Err(WritingError::Rules)
        ));
    }
    value["shared"][0]["pattern"] = "valid".into();
    value["shared"][1]["id"] = value["shared"][0]["id"].clone();
    assert!(matches!(
        Matcher::from_json(&serde_json::to_vec(&value).unwrap(), RuleSet::General),
        Err(WritingError::Rules)
    ));
    assert!(matches!(
        Matcher::from_json(&vec![b' '; MAX_RULE_BYTES + 1], RuleSet::General),
        Err(WritingError::RulesLimit)
    ));
    assert!(matches!(
        matcher(RuleSet::General).scan(Path::new("x"), &"x".repeat(MAX_TEXT_BYTES + 1)),
        Err(WritingError::EvidenceLimit)
    ));
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
fn confined_reads_refuse_links_and_bound_file_and_rule_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let owner = root.join("skills/natural-writing");
    std::fs::create_dir_all(&owner).unwrap();
    let rulefile = owner.join("lint-rules.json");
    std::fs::write(
        &rulefile,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../skills/natural-writing/lint-rules.json"
        )),
    )
    .unwrap();
    std::fs::write(root.join("a.md"), "robust").unwrap();
    let scan = inspect(
        root,
        root,
        &["a.md".into(), "a.md".into()],
        RuleSet::General,
    )
    .unwrap();
    assert_eq!(scan.findings.len(), 1);
    std::os::unix::fs::symlink(root.join("a.md"), root.join("link.md")).unwrap();
    assert!(matches!(
        inspect(root, root, &["link.md".into()], RuleSet::General),
        Err(WritingError::Evidence)
    ));
    assert!(matches!(
        inspect(root, root, &["../a.md".into()], RuleSet::General),
        Err(WritingError::Invalid(_))
    ));
    std::fs::write(root.join("a.md"), vec![b'x'; MAX_TEXT_BYTES + 1]).unwrap();
    assert!(inspect(root, root, &["a.md".into()], RuleSet::General).is_err());
    std::fs::write(&rulefile, vec![b' '; MAX_RULE_BYTES + 1]).unwrap();
    assert!(matches!(
        inspect(root, root, &["a.md".into()], RuleSet::General),
        Err(WritingError::RulesLimit)
    ));
}

#[test]
fn too_many_rules_and_compilation_complexity_are_refused() {
    let mut value: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../skills/natural-writing/lint-rules.json"
    )))
    .unwrap();
    value["shared"] = (0..257).map(|id|serde_json::json!({"id":format!("rule.{id}"),"message":"review","severity":"warning","kind":"literal","pattern":"word"})).collect::<Vec<_>>().into();
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() < MAX_RULE_BYTES);
    assert!(matches!(
        Matcher::from_json(&bytes, RuleSet::General),
        Err(WritingError::RulesLimit)
    ));
    value["shared"] = serde_json::json!([{"id":"large","message":"review","severity":"warning","kind":"regex","pattern":"a{1000000}"}]);
    assert!(matches!(
        Matcher::from_json(&serde_json::to_vec(&value).unwrap(), RuleSet::General),
        Err(WritingError::Rules)
    ));
}

#[test]
fn legal_structural_checks_also_exclude_fenced_examples() {
    let source = "```markdown\nProse.\n# Heading\n<!-- /Users/example/private -->\n```\n";
    assert!(
        matcher(RuleSet::Legal)
            .scan(Path::new("filing.md"), source)
            .unwrap()
            .is_empty()
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
