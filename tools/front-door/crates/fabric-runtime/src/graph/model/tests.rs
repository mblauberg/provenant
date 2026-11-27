use super::GraphDocument;

#[test]
fn document_without_subject_keeps_its_wire_bytes() {
    let original = r#"{"schema_version":1,"graph_id":"legacy","nodes":[]}"#;
    let document: GraphDocument = serde_json::from_str(original).unwrap();
    assert!(document.subject.is_none());
    assert_eq!(serde_json::to_string(&document).unwrap(), original);
}
