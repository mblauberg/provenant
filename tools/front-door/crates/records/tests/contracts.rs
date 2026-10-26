use std::io::Cursor;

use provenant_records::{
    AttemptOutcome, CanonicalError, CanonicalV1, DecodeError, FileDigest, Limits, PathWire,
    RecordHeader, YamlValue, decode, decode_v1, parse_frontmatter, parse_sidecar,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u64,
    count: u64,
}

#[test]
fn bounded_decode_accepts_v1_and_preserves_unsigned_integers() {
    let bytes = br#"{"schema_version":1,"count":18446744073709551615}"#;
    let decoded: Record =
        decode_v1(Cursor::new(bytes), Limits::default()).expect("valid v1 record");
    assert_eq!(decoded.count, u64::MAX);
    assert_eq!(RecordHeader::V1.schema_version(), 1);
    assert!(RecordHeader::new(u64::MAX).is_err());
    assert!(serde_json::from_str::<RecordHeader>(r#"{"schema_version":2}"#).is_err());
    assert!(serde_json::from_str::<RecordHeader>(r#"{"schema_version":1,"extra":0}"#).is_err());
}

#[test]
fn bounded_decode_normalises_negative_zero_to_an_integer() {
    let decoded: serde_json::Value =
        decode(Cursor::new(b"-0"), Limits::default()).expect("negative zero is an integer token");
    let number = decoded.as_number().expect("number");
    assert_eq!(number.as_i64(), Some(0));
    assert!(!number.is_f64());

    let typed: i64 = decode(Cursor::new(b"-0"), Limits::default()).expect("typed integer");
    assert_eq!(typed, 0);
}

#[test]
fn bounded_decode_fails_closed_for_duplicates_utf8_trailing_and_integer_overflow() {
    for (bytes, expected) in [
        (
            br#"{"schema_version":1,"count":1,"count":2}"#.as_slice(),
            "duplicate",
        ),
        (
            b"{\"schema_version\":1,\"count\":1} {}".as_slice(),
            "trailing",
        ),
        (
            b"{\"schema_version\":1,\"count\":18446744073709551616}".as_slice(),
            "range",
        ),
        (
            b"{\"schema_version\":1,\"count\":-9223372036854775809}".as_slice(),
            "range",
        ),
        (
            b"{\"schema_version\":1,\"x\":1e400}".as_slice(),
            "nonfinite",
        ),
        (
            br#"{"schema_version":1,"schema\u005fversion":1}"#.as_slice(),
            "duplicate",
        ),
        (
            b"{\"schema_version\":1,\"count\":1,\"x\":\"\xff\"}".as_slice(),
            "utf8",
        ),
    ] {
        let error = decode::<serde_json::Value, _>(Cursor::new(bytes), Limits::default())
            .expect_err("malformed record must be rejected");
        match expected {
            "duplicate" => assert!(matches!(error, DecodeError::DuplicateMember { .. })),
            "trailing" => assert!(matches!(error, DecodeError::TrailingInput { .. })),
            "range" => assert!(matches!(error, DecodeError::IntegerOutOfRange { .. })),
            "nonfinite" => assert!(matches!(error, DecodeError::NonFinite { .. })),
            "utf8" => assert!(matches!(error, DecodeError::InvalidUtf8 { .. })),
            _ => unreachable!(),
        }
    }
}

#[test]
fn typed_decode_errors_do_not_disclose_rejected_values() {
    #[derive(Debug, Deserialize)]
    enum State {
        Ready,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RecordWithState {
        #[serde(rename = "schema_version")]
        _schema_version: u64,
        #[serde(rename = "state")]
        _state: State,
    }

    let error = decode_v1::<RecordWithState, _>(
        Cursor::new(br#"{"schema_version":1,"state":"TOP_SECRET_VALUE"}"#),
        Limits::default(),
    )
    .expect_err("unknown enum variant");
    assert!(!error.to_string().contains("TOP_SECRET_VALUE"));
    assert!(!format!("{error:?}").contains("TOP_SECRET_VALUE"));
}

#[test]
fn bounded_decode_enforces_byte_depth_member_and_version_limits() {
    let too_large = Limits {
        max_bytes: 4,
        ..Limits::default()
    };
    assert!(matches!(
        decode::<serde_json::Value, _>(Cursor::new(b"[1234]"), too_large),
        Err(DecodeError::TooLarge { .. })
    ));

    let too_deep = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    assert!(matches!(
        decode::<serde_json::Value, _>(Cursor::new(b"[[[]]]"), too_deep),
        Err(DecodeError::DepthExceeded { .. })
    ));

    let hostile_depth = Limits {
        max_depth: usize::MAX,
        ..Limits::default()
    };
    let deeply_nested = format!("{}0{}", "[".repeat(70), "]".repeat(70));
    assert!(matches!(
        decode::<serde_json::Value, _>(Cursor::new(deeply_nested.as_bytes()), hostile_depth),
        Err(DecodeError::DepthExceeded { limit: 64, .. })
    ));

    let too_many = Limits {
        max_members: 2,
        ..Limits::default()
    };
    assert!(matches!(
        decode::<serde_json::Value, _>(Cursor::new(b"[1,2,3]"), too_many),
        Err(DecodeError::MembersExceeded { .. })
    ));

    assert!(matches!(
        decode_v1::<Record, _>(
            Cursor::new(br#"{"schema_version":2,"count":1}"#),
            Limits::default()
        ),
        Err(DecodeError::UnsupportedVersion { found: 2, .. })
    ));
    let error = decode_v1::<serde_json::Value, _>(
        Cursor::new(b"{\n  \"description\":\"schema_version\",\n  \"schema_version\":2\n}"),
        Limits::default(),
    )
    .expect_err("unsupported top-level version");
    assert!(matches!(
        error,
        DecodeError::UnsupportedVersion {
            line: 3,
            found: 2,
            ..
        }
    ));
}

#[test]
fn canonical_v1_sorts_keys_uses_required_escapes_and_separates_digest_domains() {
    let value = json!({"z": 2, "a": {"é": "line\n\u{0001}", "b": -0}});
    let bytes = CanonicalV1::bytes(&value).expect("integer-only canonical value");
    assert_eq!(
        bytes.as_bytes(),
        r#"{"a":{"b":0,"é":"line\n\u0001"},"z":2}"#.as_bytes()
    );

    let attempt = CanonicalV1::record_digest("attempt", 1, &value).expect("digest");
    let other_kind = CanonicalV1::record_digest("receipt", 1, &value).expect("digest");
    let other_version = CanonicalV1::record_digest("attempt", 2, &value).expect("digest");
    assert_ne!(attempt, other_kind);
    assert_ne!(attempt, other_version);
    assert!(attempt.to_string().starts_with("sha256:"));

    let file_digest = FileDigest::from_bytes([0xab; 32]);
    assert_eq!(
        serde_json::to_string(&file_digest).expect("file digest wire form"),
        format!("\"sha256:{}\"", "ab".repeat(32))
    );
    assert_eq!(
        serde_json::to_string(&AttemptOutcome::InputRequired).expect("outcome wire form"),
        "\"input_required\""
    );
    assert_eq!(
        serde_json::to_string(&AttemptOutcome::Uncommitted).expect("outcome wire form"),
        "\"uncommitted\""
    );
}

#[test]
fn canonical_v1_rejects_float_values() {
    assert!(matches!(
        CanonicalV1::bytes(&json!({"price": 1.5})),
        Err(CanonicalError::Float)
    ));
    assert!(matches!(
        CanonicalV1::bytes(&f64::NAN),
        Err(CanonicalError::Float)
    ));
    assert!(matches!(
        CanonicalV1::bytes(&f64::INFINITY),
        Err(CanonicalError::Float)
    ));
    assert!(matches!(
        CanonicalV1::bytes(&u128::MAX),
        Err(CanonicalError::IntegerOutOfRange)
    ));
}

#[test]
fn yaml_subset_parses_frontmatter_and_sidecars_and_refuses_ambiguous_values() {
    let frontmatter = "---\nname: example\ndescription: >-\n  A folded\n  description.\nallowed-tools: [\"Read\", 'Write']\n---\nbody\n";
    let parsed = parse_frontmatter(frontmatter).expect("valid skill frontmatter");
    assert!(matches!(parsed.value, YamlValue::Mapping(_)));
    assert!(parse_sidecar("name: example\nmetadata:\n  auto: true\n").is_ok());

    let plain_comment =
        parse_sidecar("Don't: quote # this is a comment\n").expect("plain apostrophe");
    let YamlValue::Mapping(plain_comment) = plain_comment.value else {
        panic!("mapping expected");
    };
    assert_eq!(
        plain_comment
            .get("Don't")
            .and_then(|entry| match &entry.value {
                YamlValue::String(value) => Some(value.as_str()),
                _ => None,
            }),
        Some("quote")
    );

    let quoted_hash =
        parse_sidecar("description: \"keep # this\" # comment\n").expect("quoted hash is data");
    let YamlValue::Mapping(quoted_hash) = quoted_hash.value else {
        panic!("mapping expected");
    };
    assert_eq!(
        quoted_hash
            .get("description")
            .and_then(|entry| match &entry.value {
                YamlValue::String(value) => Some(value.as_str()),
                _ => None,
            }),
        Some("keep # this")
    );

    let folded = parse_sidecar("description: >\n  first\n    code\n  last\n")
        .expect("folded scalar with an indented line");
    let YamlValue::Mapping(folded) = folded.value else {
        panic!("mapping expected");
    };
    assert_eq!(
        folded
            .get("description")
            .and_then(|entry| match &entry.value {
                YamlValue::String(value) => Some(value.as_str()),
                _ => None,
            }),
        Some("first\n  code\nlast\n")
    );

    for (source, expected) in [
        ("x: >\n  first\n\n  last\n", "first\nlast\n"),
        ("x: >\n\n  first\n  last\n", "\nfirst last\n"),
        ("x: >\n  first\n\n\n  last\n", "first\n\nlast\n"),
        (
            "x: >\n  first\n    code\n\n  last\n",
            "first\n  code\n\nlast\n",
        ),
        ("x: >-\n  first\n\n", "first"),
    ] {
        let parsed = parse_sidecar(source).expect("folded blank-line scalar");
        let YamlValue::Mapping(mapping) = parsed.value else {
            panic!("mapping expected");
        };
        assert_eq!(
            mapping.get("x").and_then(|entry| match &entry.value {
                YamlValue::String(value) => Some(value.as_str()),
                _ => None,
            }),
            Some(expected)
        );
    }

    for invalid in [
        "name: example\nname: duplicate\n",
        "name: yes\n",
        "name: 12\n",
        "name: null\n",
        "name: &anchor value\n",
        "name: *alias\n",
        "name: !tag value\n",
        "name: value\n---\nname: second\n",
        "name: first\n  continued plain text\n",
    ] {
        assert!(
            parse_sidecar(invalid).is_err(),
            "accepted invalid YAML: {invalid:?}"
        );
    }

    assert!(parse_frontmatter("body\n---\nname: missing opening\n").is_err());
    assert!(parse_frontmatter("---\nname: unclosed\nbody\n").is_err());
    let oversized_line = format!("---\n{}\n---\n", "x".repeat(256 * 1024));
    assert!(matches!(
        parse_frontmatter(&oversized_line),
        Err(provenant_records::YamlError {
            kind: provenant_records::YamlErrorKind::LimitExceeded,
            ..
        })
    ));
}

#[test]
fn path_wire_is_tagged_and_rejects_uncontained_or_unrepresentable_paths() {
    let utf8 = PathWire::from_relative(
        &provenant_core::RelativePath::parse("artifacts/result.md").expect("contained path"),
    )
    .expect("UTF-8 wire");
    assert_eq!(
        serde_json::to_string(&utf8).expect("path wire"),
        r#"{"encoding":"utf8","path":"artifacts/result.md"}"#
    );
    assert_eq!(
        utf8.to_relative_path().expect("relative path").as_path(),
        std::path::Path::new("artifacts/result.md")
    );

    for invalid in [
        r#"{"encoding":"utf8","path":"../outside"}"#,
        r#"{"encoding":"native_bytes","path":"GG"}"#,
    ] {
        assert!(serde_json::from_str::<PathWire>(invalid).is_err());
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let native = std::ffi::OsStr::from_bytes(b"artifact/\xff");
        let relative = provenant_core::RelativePath::parse(native).expect("contained native path");
        let encoded = PathWire::from_relative(&relative).expect("native encoding");
        assert_eq!(
            serde_json::to_string(&encoded).expect("native wire"),
            r#"{"encoding":"native_bytes","path":"61727469666163742fff"}"#
        );
        assert_eq!(
            encoded.to_relative_path().expect("native path roundtrip"),
            relative
        );
    }
}

#[test]
fn json_encode_rejects_nonfinite_values_without_losing_finite_floats() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(provenant_records::encode(&value).is_err());
        assert!(provenant_records::encode(&vec![value]).is_err());
    }
    assert_eq!(provenant_records::encode(&1.5).unwrap(), b"1.5");
}

#[test]
fn jsonc_rejects_a_byte_order_mark() {
    assert!(
        provenant_records::JsoncDocument::read(Cursor::new(b"\xef\xbb\xbf{}"), Limits::CONFIG)
            .is_err()
    );
}
