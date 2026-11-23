use super::WriterOwnership;

#[test]
fn writer_ownership_requires_owner_member_but_accepts_unknown_owner() {
    let current = serde_json::json!({
        "schema_version": 1,
        "lane_owned": false,
        "owner": null,
    });
    let parsed: WriterOwnership = serde_json::from_value(current.clone()).unwrap();
    assert_eq!(parsed.owner, None);

    let mut absent = current;
    absent.as_object_mut().unwrap().remove("owner");
    assert!(serde_json::from_value::<WriterOwnership>(absent).is_err());
}
