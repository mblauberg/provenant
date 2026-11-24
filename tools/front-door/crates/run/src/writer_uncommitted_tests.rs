use super::*;

#[test]
fn dirty_work_projection_keeps_paths_and_omitted_count_structured() {
    assert_eq!(uncommitted_reason(&[]), None);
    let one = uncommitted_reason(&[b"a.rs".to_vec(), b"b/c.rs".to_vec()]).unwrap();
    assert_eq!(
        one.paths.iter().map(String::as_str).collect::<Vec<_>>(),
        ["a.rs", "b/c.rs"]
    );
    assert_eq!(one.omitted, 0);
    let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n:02}").into_bytes()).collect();
    let reason = uncommitted_reason(&many).unwrap();
    assert_eq!(reason.paths.len(), UNCOMMITTED_LISTED);
    assert_eq!(reason.omitted, many.len() - reason.paths.len());
    assert!(reason.render().len() <= 1024);
    let long = uncommitted_reason(&[vec![b'x'; 4000]]).unwrap();
    assert!(long.render().len() <= 1024);
}
