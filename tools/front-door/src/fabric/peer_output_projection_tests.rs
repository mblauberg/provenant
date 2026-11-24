// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn graph_result_projects_typed_credentials_before_paging() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    use super::*;
    let projector = provenant_confinement::publication::Projector::new([]).unwrap();
    let target: AttemptTarget =
        serde_json::from_value(serde_json::json!({"attempt":"11111111111111111111111111111111"}))
            .unwrap();
    let input = OutputInput::Read {
        extra: Default::default(),
        target: target.clone(),
        part: OutputPart::Result,
        offset: 0,
        max_bytes: 4096,
    };
    let raw = r#"{"token":"peer-private-value","result":"ok"}"#;
    let output = project_peer_output_with(&input, &projector, |_| {
        serde_json::from_value(serde_json::json!({
            "kind":"chunk", "target":target, "part":"result", "state":"succeeded", "reason":null,
            "offset":0, "next_offset":raw.len(), "eof":true,
            "digest":provenant_core::Digest::from_bytes(*provenant_records::CanonicalV1::file_digest(raw.as_bytes()).bytes()),
            "bytes":{"encoding":"utf8","data":raw}, "length":raw.len()
        })).map_err(|_| FacadeError::StateCorrupt)
    }).unwrap();
    let OutputResult::Chunk {
        bytes: OutputBytes::Utf8(text),
        length,
        digest,
        ..
    } = output
    else {
        panic!("projected chunk")
    };
    assert!(!text.contains("peer-private-value"));
    assert!(text.contains("[REDACTED]"));
    assert_eq!(length, Some(text.len() as u64));
    assert_eq!(
        digest,
        provenant_core::Digest::from_bytes(
            *provenant_records::CanonicalV1::file_digest(text.as_bytes()).bytes()
        )
    );
}

#[test]
fn output_projects_lane_prose_and_preserves_metadata() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    use super::*;
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let projector = provenant_confinement::publication::Projector::new([]).unwrap();
    let target: AttemptTarget =
        serde_json::from_value(serde_json::json!({"attempt":"11111111111111111111111111111111"}))
            .unwrap();
    let input = OutputInput::Read {
        extra: Default::default(),
        target: target.clone(),
        part: OutputPart::Result,
        offset: 0,
        max_bytes: 4096,
    };
    let output = project_peer_output_with(&input, &projector, |_| {
        serde_json::from_value(serde_json::json!({
            "kind":"chunk", "target":target, "part":"result", "state":"succeeded", "reason":null,
            "offset":0, "next_offset":2, "eof":true,
            "digest":provenant_core::Digest::from_bytes(*provenant_records::CanonicalV1::file_digest(b"ok").bytes()),
            "bytes":{"encoding":"utf8","data":"ok"}, "length":2,
            "title":token, "model":token, "skipped_routes":[{"detail":token,"name":token}],
            "applied_route":{"model":token,"launch_warnings":[token]},
            "handoff":{"Ok":{"note":token,"spec_concerns":[{"criterion":token,"claim":token,"evidence":token,"proposed_change":token}]}}
        })).map_err(|_| FacadeError::StateCorrupt)
    }).unwrap();
    let row = serde_json::to_value(output).unwrap();
    assert_eq!(row["model"], token);
    assert_eq!(row["applied_route"]["model"], token);
    assert_eq!(row["skipped_routes"][0]["name"], token);
    for text in [
        &row["title"],
        &row["skipped_routes"][0]["detail"],
        &row["applied_route"]["launch_warnings"][0],
        &row["handoff"]["Ok"]["note"],
        &row["handoff"]["Ok"]["spec_concerns"][0]["criterion"],
        &row["handoff"]["Ok"]["spec_concerns"][0]["claim"],
        &row["handoff"]["Ok"]["spec_concerns"][0]["evidence"],
        &row["handoff"]["Ok"]["spec_concerns"][0]["proposed_change"],
    ] {
        assert_eq!(text, "[REDACTED]");
    }
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn paged_projection_survives_every_mask_boundary_and_response_encoding() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    use super::*;
    use provenant_fabric_runtime::hosts::{PeerResponse, PeerResult, encode_peer_response};
    let limit = provenant_records::run::OUTPUT_LIMIT as usize;
    let secret = "long-private-value";
    let projector =
        provenant_confinement::publication::Projector::new([secret.as_bytes().to_vec()]).unwrap();
    let target: AttemptTarget =
        serde_json::from_value(serde_json::json!({"attempt":"11111111111111111111111111111111"}))
            .unwrap();
    let mask = "[REDACTED]";
    let mut cases: Vec<_> = (0..=mask.len())
        .map(|split| {
            let padding = "x".repeat(limit - split - " token=".len());
            (
                format!("{padding} token={secret} tail"),
                format!("{padding} token={mask} tail"),
            )
        })
        .collect();
    // A page starting mid-word must not be scanned as a new assignment.
    let text = format!("{}notatoken=public tail", "x".repeat(limit - 4));
    cases.push((text.clone(), text));
    let padding = "x".repeat(limit - 1);
    cases.push((
        format!("{padding}é{secret}tail"),
        format!("{padding}é{mask}tail"),
    ));
    for (raw, expected) in cases {
        assert!(raw.len() > limit);
        let raw_digest = provenant_core::Digest::from_bytes(
            *provenant_records::CanonicalV1::file_digest(raw.as_bytes()).bytes(),
        );
        let expected_digest = provenant_core::Digest::from_bytes(
            *provenant_records::CanonicalV1::file_digest(expected.as_bytes()).bytes(),
        );
        let mut offset = 0;
        let mut received = Vec::new();
        loop {
            let input = OutputInput::Read {
                extra: Default::default(),
                target: target.clone(),
                part: OutputPart::Result,
                offset,
                max_bytes: limit as u64,
            };
            let page = project_peer_output_with(&input, &projector, |request| {
                let OutputInput::Read { offset, max_bytes, .. } = request else { unreachable!() };
                let start = offset as usize;
                let end = (start + max_bytes as usize).min(raw.len());
                serde_json::from_value(serde_json::json!({
                    "kind":"chunk", "target":target, "part":"result", "state":"succeeded", "reason":null,
                    "offset":start, "next_offset":end, "eof":end == raw.len(), "digest":raw_digest,
                    "bytes":OutputBytes::from_bytes(raw.as_bytes()[start..end].to_vec()), "length":raw.len(),
                    "extension_identifier":"long-private-value"
                })).map_err(|_| FacadeError::StateCorrupt)
            }).unwrap();
            assert_eq!(
                serde_json::to_value(&page).unwrap()["extension_identifier"],
                secret
            );
            let response = PeerResponse {
                extra: Default::default(),
                version: 1,
                protocol: provenant_fabric_runtime::hosts::PROTOCOL,
                host: provenant_core::HostId::from_bytes([1; 16]),
                project: provenant_fabric_runtime::identity::CrossHostProjectKey::from_home(
                    Path::new("/h"),
                    Path::new("/h/repo/.git"),
                )
                .unwrap(),
                result: PeerResult::Output {
                    data: Box::new(page.clone()),
                    extra: Default::default(),
                },
            };
            let encoded = encode_peer_response(response, &projector).unwrap();
            let response: PeerResponse = serde_json::from_slice(&encoded).unwrap();
            let PeerResult::Output { data, .. } = response.result else {
                panic!("output frame")
            };
            assert_eq!(*data, page);
            let OutputResult::Chunk {
                bytes,
                digest,
                length,
                next_offset,
                eof,
                ..
            } = *data
            else {
                panic!("chunk")
            };
            let bytes = match bytes {
                OutputBytes::Utf8(text) => text.into_bytes(),
                OutputBytes::Base64(text) => {
                    provenant_fabric_runtime::handoff::decode_base64(&text).unwrap()
                }
            };
            assert_eq!(digest, expected_digest);
            assert_eq!(length, Some(expected.len() as u64));
            assert_eq!(
                bytes,
                expected.as_bytes()[offset as usize..next_offset as usize]
            );
            received.extend(bytes);
            if eof {
                break;
            }
            assert!(next_offset > offset);
            offset = next_offset;
        }
        assert_eq!(received, expected.as_bytes());
    }
}

#[test]
fn projects_before_paging_and_binds_published_bytes() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    use super::*;
    let secret = "a-long-private-value";
    let projector =
        provenant_confinement::publication::Projector::new([secret.as_bytes().to_vec()]).unwrap();
    let target: AttemptTarget =
        serde_json::from_value(serde_json::json!({"attempt":"11111111111111111111111111111111"}))
            .unwrap();
    let input = OutputInput::Read {
        extra: Default::default(),
        target: target.clone(),
        part: OutputPart::Result,
        offset: 65520,
        max_bytes: 100,
    };
    for (raw, expected) in [
        (
            format!("{}{secret}tail", "x".repeat(65530)),
            format!("{}[REDACTED]tail", "x".repeat(65530)),
        ),
        (
            format!("{}é{secret}tail", "x".repeat(65535)),
            format!("{}é[REDACTED]tail", "x".repeat(65535)),
        ),
    ] {
        let actual_digest = provenant_core::Digest::from_bytes(
            *provenant_records::CanonicalV1::file_digest(raw.as_bytes()).bytes(),
        );
        let projected = project_peer_output_with(&input, &projector, |request| {
            let OutputInput::Read { offset, .. } = request else {
                unreachable!()
            };
            let start = offset as usize;
            let end = (start + 65536).min(raw.len());
            serde_json::from_value(serde_json::json!({
            "kind":"chunk", "target":target, "part":"result", "state":"succeeded", "reason":null,
            "offset":start, "next_offset":end, "eof":end == raw.len(), "digest":actual_digest,
            "bytes":OutputBytes::from_bytes(raw.as_bytes()[start..end].to_vec()), "length":raw.len()
        })).map_err(|_| FacadeError::StateCorrupt)
        })
        .unwrap();
        let OutputResult::Chunk {
            bytes: OutputBytes::Utf8(page),
            length,
            digest,
            offset,
            next_offset,
            ..
        } = projected
        else {
            panic!("projected chunk")
        };
        assert_eq!(page, expected[65520..]);
        assert!(!page.contains(secret));
        assert_eq!(length, Some(expected.len() as u64));
        assert_eq!(offset, 65520);
        assert_eq!(next_offset, expected.len() as u64);
        assert_eq!(
            digest,
            provenant_core::Digest::from_bytes(
                *provenant_records::CanonicalV1::file_digest(expected.as_bytes()).bytes()
            )
        );
    }
    for max_bytes in [0, provenant_records::run::OUTPUT_LIMIT + 1] {
        let mut invalid = input.clone();
        if let OutputInput::Read { max_bytes: max, .. } = &mut invalid {
            *max = max_bytes;
        }
        let error = project_peer_output_with(&invalid, &projector, |_| unreachable!()).unwrap_err();
        assert_eq!(
            error.code(),
            super::super::run_error(provenant_run::RunError::TooLarge {
                limit: provenant_records::run::OUTPUT_LIMIT,
            })
            .code()
        );
    }
    let mut past_end = input.clone();
    if let OutputInput::Read { offset, .. } = &mut past_end {
        *offset = u64::MAX;
    }
    assert!(project_peer_output_with(&past_end, &projector, |_| {
        serde_json::from_value(serde_json::json!({
            "kind":"chunk", "target":target, "part":"result", "state":"succeeded", "reason":null,
            "offset":0, "next_offset":4, "eof":true, "digest":provenant_core::Digest::from_bytes(*provenant_records::CanonicalV1::file_digest(b"test").bytes()),
            "bytes":{"encoding":"utf8","data":"test"}, "length":4
        })).map_err(|_| FacadeError::StateCorrupt)
    }).is_err());
}
