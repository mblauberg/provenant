use super::*;
use serde::{Serialize, de::DeserializeOwned};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn unknown_field_survives<T>(record: T) -> TestResult
where
    T: DeserializeOwned + Serialize,
{
    let mut payload = serde_json::to_value(record)?;
    let future = serde_json::json!({"value": 2});
    payload
        .as_object_mut()
        .ok_or("stored record object")?
        .insert("future_minor_field".into(), future.clone());
    let bytes = serde_json::to_vec(&payload)?;
    let decoded: T =
        provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())?;
    let encoded = provenant_records::encode(&decoded)?;
    let round_trip: serde_json::Value =
        provenant_records::decode(encoded.as_slice(), provenant_records::Limits::default())?;
    assert_eq!(round_trip.get("future_minor_field"), Some(&future));
    Ok(())
}

#[test]
fn persisted_writer_records_preserve_fields_from_newer_minors() -> TestResult {
    let scope = provenant_records::custody::Scope {
        attempt: provenant_core::AttemptId::from_bytes([1; 16]),
        generation: 1,
    };
    let oid = GitOid::parse("1111111111111111111111111111111111111111")?;
    unknown_field_survives(GitPins {
        extra: Default::default(),
        schema_version: 1,
        scope,
        base: oid,
    })?;
    unknown_field_survives(PrivateMergeRetained {
        extra: Default::default(),
        schema_version: 1,
        scope,
    })?;
    Ok(())
}
