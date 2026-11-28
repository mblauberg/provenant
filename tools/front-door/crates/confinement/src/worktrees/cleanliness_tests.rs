use super::*;

#[test]
fn summary_includes_both_rename_paths() {
    let paths = [StatusPath {
        kind: StatusKind::RenameOrCopy,
        path: b"new name".to_vec(),
        original_path: Some(b"old name".to_vec()),
    }];
    let summary = dirty_paths_summary(&paths);
    assert!(summary.contains("new name"));
    assert!(summary.contains("old name"));
}

#[test]
fn summary_lists_dirty_paths_and_counts_the_rest() {
    let empty = dirty_paths_summary_bytes(&[]);
    assert!(empty.text.is_empty());
    assert_eq!((empty.listed, empty.omitted, empty.total), (0, 0, 0));
    let one = dirty_paths_summary_bytes(&[b"a.rs", b"b/c.rs"]);
    for path in ["a.rs", "b/c.rs"] {
        assert_eq!(one.text.matches(path).count(), 1, "{}", one.text);
    }
    let dedup = dirty_paths_summary_bytes(&[b"a.rs", b"a.rs", b"b.rs"]);
    for path in ["a.rs", "b.rs"] {
        assert_eq!(dedup.text.matches(path).count(), 1, "{}", dedup.text);
    }
    assert_eq!(dedup.total, 2);
    assert_eq!(dedup.listed + dedup.omitted, dedup.total);
    let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n:02}").into_bytes()).collect();
    let many_refs: Vec<&[u8]> = many.iter().map(Vec::as_slice).collect();
    let summary = dirty_paths_summary_bytes(&many_refs);
    assert!(summary.text.len() <= DIRTY_SUMMARY_BYTES);
    assert_eq!(summary.total, many.len());
    assert_eq!(summary.listed + summary.omitted, summary.total);
    let wide: Vec<Vec<u8>> = (0..12)
        .map(|n| format!("{n:02}{}", "y".repeat(200)).into_bytes())
        .collect();
    let wide_refs: Vec<&[u8]> = wide.iter().map(Vec::as_slice).collect();
    let summary = dirty_paths_summary_bytes(&wide_refs);
    assert_eq!(summary.total, wide.len());
    assert_eq!(summary.listed + summary.omitted, summary.total);
}
