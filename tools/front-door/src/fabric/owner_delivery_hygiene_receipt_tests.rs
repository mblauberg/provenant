#[test]
fn recovery_receipt_requires_operation_marker_and_one_expected_parent() {
    use super::delivery_hygiene_receipt_matches;
    let head = "b".repeat(40);
    let expected = "a".repeat(40);
    let receipt = format!("{head}\0Apply formatter\n\nDelivery-Queue-Operation: hygiene-1\n");
    let lineage = format!("{head} {expected}\n");
    assert!(delivery_hygiene_receipt_matches(
        &head,
        &expected,
        "hygiene-1",
        &receipt,
        &lineage
    ));
    assert!(!delivery_hygiene_receipt_matches(
        &head,
        &expected,
        "hygiene-2",
        &receipt,
        &lineage
    ));
    let wrong_parent = format!("{head} {}\n", "c".repeat(40));
    assert!(!delivery_hygiene_receipt_matches(
        &head,
        &expected,
        "hygiene-1",
        &receipt,
        &wrong_parent
    ));
    let merge = format!("{head} {expected} {}\n", "c".repeat(40));
    assert!(!delivery_hygiene_receipt_matches(
        &head,
        &expected,
        "hygiene-1",
        &receipt,
        &merge
    ));
}
