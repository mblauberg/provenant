use provenant_core::RelativePath;
use provenant_records::{CanonicalV1, FileDigest, PathWire, record_digest};

#[test]
fn path_wire_accepts_only_checked_canonical_encodings() {
    let path = RelativePath::parse("artifacts/result.md").expect("relative path");
    let wire = PathWire::from_relative(&path).expect("UTF-8 path encoding");
    let encoded = serde_json::to_value(&wire).expect("serialize path wire");
    assert_eq!(
        encoded,
        serde_json::json!({"encoding": "utf8", "path": "artifacts/result.md"})
    );
    let decoded: PathWire = serde_json::from_value(encoded).expect("decode checked path wire");
    assert_eq!(
        decoded.to_relative_path().expect("relative path").as_path(),
        path.as_path()
    );

    for invalid in [
        serde_json::json!({"encoding": "native_bytes", "path": "6172746966616374732f726573756c742e6d64"}),
        serde_json::json!({"encoding": "native_bytez", "path": "ff"}),
        serde_json::json!({"encoding": "utf8", "path": "../outside"}),
        serde_json::json!({"encoding": "native_bytes", "path": "GG"}),
    ] {
        assert!(
            serde_json::from_value::<PathWire>(invalid).is_err(),
            "invalid path wire must be refused"
        );
    }
}

#[cfg(unix)]
#[test]
fn path_wire_uses_native_bytes_only_for_non_utf8_paths() {
    use std::os::unix::ffi::OsStrExt;

    let native = std::ffi::OsStr::from_bytes(b"artifacts/\xff");
    let path = RelativePath::parse(native).expect("relative native path");
    let wire = PathWire::from_relative(&path).expect("native byte encoding");
    let encoded = serde_json::to_value(&wire).expect("serialize path wire");
    assert_eq!(
        encoded,
        serde_json::json!({"encoding": "native_bytes", "path": "6172746966616374732fff"})
    );
    let decoded: PathWire = serde_json::from_value(encoded).expect("decode native bytes");
    assert_eq!(
        decoded
            .to_relative_path()
            .expect("relative native path")
            .as_path(),
        path.as_path()
    );
}

#[test]
fn file_digest_has_a_checked_persistent_text_form() {
    let digest = FileDigest::from_bytes([0xab; 32]);
    let text = digest.to_string();
    assert_eq!(FileDigest::parse(&text).expect("parse file digest"), digest);
    assert_eq!(
        serde_json::from_str::<FileDigest>(&format!("\"{text}\""))
            .expect("deserialize file digest"),
        digest
    );
    for invalid in [
        format!("sha256:{}", "AB".repeat(32)),
        format!("sha256:{}", "a".repeat(63)),
        format!("sha1:{}", "a".repeat(64)),
    ] {
        assert!(FileDigest::parse(&invalid).is_err());
    }
}

#[test]
fn only_canonical_encoder_output_can_be_hashed_as_record_bytes() {
    let canonical =
        CanonicalV1::bytes(&serde_json::json!({"b": 2, "a": 1})).expect("canonical encoding");
    assert_eq!(canonical.as_bytes(), br#"{"a":1,"b":2}"#);
    let raw_digest = record_digest("attempt", 1, &canonical).expect("hash canonical bytes");
    let encoded_digest =
        CanonicalV1::record_digest("attempt", 1, &serde_json::json!({"b": 2, "a": 1}))
            .expect("hash typed value");
    assert_eq!(raw_digest, encoded_digest);
}
