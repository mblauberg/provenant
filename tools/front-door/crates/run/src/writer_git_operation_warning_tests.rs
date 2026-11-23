use super::*;

fn directory_with(head: &[u8]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        temp.path().join("MERGE_HEAD"),
        format!("{}\n", "a".repeat(40)),
    )
    .expect("merge marker");
    std::fs::write(temp.path().join("HEAD"), head).expect("HEAD");
    temp
}

#[test]
fn non_utf8_head_degrades_to_unknown_commit() {
    let temp = directory_with(&[0xff, 0xfe, 0x80]);
    let directory = Directory::open_anchor(temp.path()).expect("directory");
    let warning = lane_git_operation_warning(&directory, Path::new("git-common"), |_| {
        Err(RunError::Validation("unresolvable"))
    })
    .expect("a warning is still produced");
    assert!(warning.contains("commit unknown"), "{warning}");
    assert!(warning.contains("merge"), "{warning}");
}

#[test]
fn unresolvable_symbolic_ref_degrades_to_unknown_commit() {
    let temp = directory_with(b"ref: refs/heads/lane\n");
    let directory = Directory::open_anchor(temp.path()).expect("directory");
    let warning = lane_git_operation_warning(&directory, Path::new("git-common"), |_| {
        Err(RunError::Validation("unborn branch"))
    })
    .expect("a warning is still produced");
    assert!(warning.contains("commit unknown"), "{warning}");
}

#[test]
fn undecodable_request_refuses_instead_of_dropping_the_destination() {
    let temp = tempfile::tempdir().expect("temp dir");
    let attempt = Directory::open_anchor(temp.path()).expect("directory");
    crate::artifacts::seal(&attempt, "request.json", b"not json", crate::RECORD_LIMIT)
        .expect("seal undecodable request");
    assert!(
        sealed_delivery_destination(&attempt).is_err(),
        "an undecodable sealed request must refuse, not drop the destination into a warning"
    );
}

#[test]
fn sealed_destination_is_namespaced_or_absent() {
    let temp = tempfile::tempdir().expect("temp dir");
    let attempt = Directory::open_anchor(temp.path()).expect("directory");
    assert!(
        sealed_delivery_destination(&attempt)
            .expect("a missing request has no destination")
            .is_none()
    );
    let mut request = crate::tests::input().request;
    request.task.deliver = Some(provenant_records::run::WriterDeliveryRequest {
        extra: Default::default(),
        mode: Default::default(),
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
        fast_forward_only: false,
    });
    crate::artifacts::seal(
        &attempt,
        "request.json",
        &crate::db::bytes(&request).expect("encode request"),
        crate::RECORD_LIMIT,
    )
    .expect("seal request");
    assert_eq!(
        sealed_delivery_destination(&attempt).expect("destination"),
        Some("refs/heads/published/lane".to_owned())
    );
}

#[test]
fn an_unresolvable_warning_path_degrades_to_unknown() {
    assert_eq!(
        warning_path(Err::<PathBuf, _>("path gone")),
        PathBuf::from("an unknown path")
    );
    let known = PathBuf::from("/git/common");
    assert_eq!(warning_path(Ok::<_, &str>(known.clone())), known);
}

#[test]
fn warning_is_recorded_once_and_uses_the_catalogued_code() {
    GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    let ((), captured) = provenant_records::warning::capture(|| {
        let mut warnings = Vec::new();
        let message = "a retained operation".to_owned();
        record_git_operation_warning(&mut warnings, message.clone());
        record_git_operation_warning(&mut warnings, message.clone());
        assert_eq!(warnings, vec![message]);
    });
    assert_eq!(captured.len(), 1, "{captured:?}");
    assert_eq!(captured[0].code.as_ref(), "GIT_OPERATION_RETAINED");
    let emitted = GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow().clone());
    assert_eq!(emitted.len(), 1, "{emitted:?}");
}
