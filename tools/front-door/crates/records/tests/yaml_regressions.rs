use std::fs;
use std::path::{Path, PathBuf};

use provenant_records::{parse_sidecar, YamlErrorKind, YamlValue};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../")
        .canonicalize()
        .expect("repository root")
}

fn refusal_fixtures(directory: &Path, fixtures: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read refusal fixtures") {
        let path = entry.expect("fixture entry").path();
        if path.is_dir() {
            refusal_fixtures(&path, fixtures);
        } else if path.extension().is_some_and(|extension| extension == "yaml") {
            fixtures.push(path);
        }
    }
}

fn scalar<'a>(mapping: &'a std::collections::BTreeMap<String, provenant_records::SpannedYaml>, key: &str) -> &'a str {
    match &mapping.get(key).expect("scalar key").value {
        YamlValue::String(value) => value,
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn malformed_plain_scalar_indicators_are_refused_by_named_fixtures() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/yaml/refusals");
    let mut fixtures = Vec::new();
    refusal_fixtures(&fixture_root, &mut fixtures);
    fixtures.sort();
    assert_eq!(fixtures.len(), 20, "keep one refusal fixture per case");

    for path in fixtures {
        let source = fs::read_to_string(&path).expect("fixture source");
        assert!(
            parse_sidecar(&source).is_err(),
            "accepted refusal fixture {}: {source:?}",
            path.display()
        );
    }
}

#[test]
fn directive_indicators_inside_later_tokens_remain_plain_text() {
    let parsed = parse_sidecar("a: Tom & Jerry\nb: use *only* this\n")
        .expect("later tokens are ordinary scalar text");
    let YamlValue::Mapping(mapping) = parsed.value else {
        panic!("mapping expected");
    };
    assert_eq!(scalar(&mapping, "a"), "Tom & Jerry");
    assert_eq!(scalar(&mapping, "b"), "use *only* this");
}

#[test]
fn folded_blank_before_more_indented_text_preserves_both_line_breaks() {
    let parsed = parse_sidecar("a: >\n  a\n\n   b\n").expect("folded scalar");
    let YamlValue::Mapping(mapping) = parsed.value else {
        panic!("mapping expected");
    };
    assert_eq!(scalar(&mapping, "a"), "a\n\n b\n");
}

#[test]
fn clipped_empty_block_scalar_discards_trailing_blank_lines() {
    let parsed = parse_sidecar("a: |\n\n\nnext: value\n").expect("empty clipped scalar");
    let YamlValue::Mapping(mapping) = parsed.value else {
        panic!("mapping expected");
    };
    assert_eq!(scalar(&mapping, "a"), "");
    assert_eq!(scalar(&mapping, "next"), "value");
}

#[test]
fn sequence_after_empty_mapping_value_has_its_own_error_kind() {
    let error = parse_sidecar("a:\n- x\n").expect_err("sequence is not a mapping child");
    assert_eq!(error.kind, YamlErrorKind::BlockSequenceAfterMapping);
}
