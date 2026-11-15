//! Composition of bounded, host-owned inbox pages.
use provenant_core::{HostId, HumanRequestId};
use provenant_fabric_runtime::inbox::{
    InboxPeerCoverage, InboxPeerState, InboxProject, InboxProjectPath, InboxResult,
};
use provenant_records::inbox::HumanRequest;
use serde_json::Value;
use std::collections::BTreeSet;

/// The result of a peer read, supplied by the transport adapter.
#[derive(Debug)]
pub enum InboxObservation {
    Page(Value),
    /// The peer answered, but contributed no usable page; the reason says why.
    Refused(&'static str),
    /// No answer in time or at all; the reason says which.
    Unreachable(&'static str),
    Unsupported,
}

/// Unicode format characters (category Cf) that can reorder or hide text.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{890}'..='\u{891}'
        | '\u{8e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

/// A peer-supplied project label is display text only: a string, with control
/// and Unicode format characters (such as bidi overrides) removed and at most
/// 128 characters. Anything else is null.
fn peer_label(value: &Value) -> Option<String> {
    let label: String = value
        .as_str()
        .map(|text| {
            text.chars()
                .filter(|c| !c.is_control() && !is_format(*c))
                .take(128)
                .collect()
        })
        .unwrap_or_default();
    (!label.is_empty()).then_some(label)
}

/// The configured host is authoritative for the identities in its reply.
#[derive(Debug)]
pub struct PeerInbox {
    pub host: HostId,
    pub label: String,
    pub observation: InboxObservation,
}

/// Compose peer first pages after the local listing. Local rows, the local
/// cursor and the string `coverage` are untouched. Peer rows are bounded to
/// 100 per peer; a row is valid only when its owner is the peer and it names
/// the project key that routes an answer. Invalid and duplicate rows are
/// counted, and a peer continuation is evidence of incomplete coverage.
pub fn compose_inbox(
    local: InboxResult,
    peers: Vec<PeerInbox>,
) -> Result<InboxResult, &'static str> {
    let InboxResult::Listed {
        extra,
        mut items,
        next_cursor,
        truncated,
        coverage,
        mut projects,
        peers: mut report,
        ..
    } = local
    else {
        return Err("invalid local inbox");
    };
    let mut seen: BTreeSet<(HostId, HumanRequestId)> = items
        .iter()
        .map(|item| (item.owner_host, item.id))
        .collect();
    let mut hosts = BTreeSet::new();
    for peer in peers {
        let (host, label) = (peer.host, peer.label);
        let entry =
            |state, counts: [usize; 3], truncated: bool, reason: Option<&str>| InboxPeerCoverage {
                host,
                label: label.clone(),
                state,
                included: counts[0],
                invalid_rows: counts[1],
                duplicate_rows: counts[2],
                truncated,
                reason: reason.map(str::to_owned),
            };
        if !hosts.insert(host) {
            report.push(entry(
                InboxPeerState::Partial,
                [0; 3],
                false,
                Some("duplicate_host"),
            ));
            continue;
        }
        let page = match peer.observation {
            InboxObservation::Refused(reason) => {
                report.push(entry(InboxPeerState::Partial, [0; 3], false, Some(reason)));
                continue;
            }
            InboxObservation::Unreachable(reason) => {
                report.push(entry(
                    InboxPeerState::Unreachable,
                    [0; 3],
                    false,
                    Some(reason),
                ));
                continue;
            }
            InboxObservation::Unsupported => {
                report.push(entry(InboxPeerState::Unsupported, [0; 3], false, None));
                continue;
            }
            InboxObservation::Page(page) => page,
        };
        let Some(rows) = page["items"].as_array() else {
            report.push(entry(
                InboxPeerState::Partial,
                [0; 3],
                false,
                Some("invalid_page"),
            ));
            continue;
        };
        let (mut invalid, mut duplicates, mut included) = (0usize, 0usize, 0usize);
        for row in rows.iter().take(100) {
            let Ok(request) = serde_json::from_value::<HumanRequest>(row.clone()) else {
                invalid += 1;
                continue;
            };
            if request.owner_host != host || request.project_key.is_none() {
                invalid += 1;
            } else if !seen.insert((request.owner_host, request.id)) {
                duplicates += 1;
            } else {
                projects.insert(
                    format!("{host}:{}", request.project_id),
                    InboxProject {
                        label: peer_label(
                            &page["projects"][request.project_id.to_string()]["label"],
                        ),
                        path: InboxProjectPath::Omitted,
                        host: Some(host),
                    },
                );
                items.push(request);
                included += 1;
            }
        }
        let peer_truncated = rows.len() > 100 || page["truncated"] != false;
        let incomplete = peer_truncated || invalid != 0 || duplicates != 0;
        report.push(entry(
            if incomplete {
                InboxPeerState::Partial
            } else {
                InboxPeerState::Complete
            },
            [included, invalid, duplicates],
            peer_truncated,
            None,
        ));
    }
    Ok(InboxResult::Listed {
        extra,
        items,
        next_cursor,
        truncated,
        coverage,
        projects,
        peers: report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::ProjectId;
    use serde_json::json;

    fn request(host: HostId, id: u8, key: Option<&str>) -> Value {
        json!({
            "schema_version":1,"id":HumanRequestId::from_bytes([id;16]),"revision":1,
            "operation_id":"op","created_at":"2026-01-01T00:00:00.000000Z",
            "updated_at":"2026-01-01T00:00:00.000000Z",
            "project_id":ProjectId::from_bytes([9;16]),"project_key":key,
            "owner_host":host,"chair_session_id":"s",
            "chair_actor":provenant_core::ActorId::from_bytes([8;16]),
            "request":{"kind":"decision","title":"t","body":"b"},
            "state":"open"
        })
    }
    fn local(host: HostId) -> InboxResult {
        let item = serde_json::from_value(request(host, 1, Some("k"))).unwrap();
        InboxResult::Listed {
            extra: Default::default(),
            items: vec![item],
            next_cursor: None,
            truncated: true,
            coverage: "local_only".into(),
            projects: Default::default(),
            peers: Default::default(),
        }
    }
    fn peer(host: HostId, observation: InboxObservation) -> PeerInbox {
        PeerInbox {
            host,
            label: format!("peer-{}", host.to_string().len()),
            observation,
        }
    }

    #[test]
    fn peer_inbox_rejects_a_non_listing() {
        let saved: InboxResult = serde_json::from_value(json!({
            "result":"saved","operation_id":"op","request_id":HumanRequestId::from_bytes([1;16]),
            "revision":1,"state":"open","answer_id":null,"delivery":null
        }))
        .unwrap();
        assert!(compose_inbox(saved, vec![]).is_err());
    }

    #[test]
    fn peer_inbox_reports_bad_pages_reasons_and_duplicate_hosts() {
        let remote = HostId::from_bytes([2; 16]);
        let other = HostId::from_bytes([3; 16]);
        let listed = || InboxResult::Listed {
            extra: Default::default(),
            items: vec![],
            next_cursor: None,
            truncated: false,
            coverage: String::new(),
            projects: Default::default(),
            peers: Default::default(),
        };
        let item = compose_inbox(
            listed(),
            vec![
                peer(remote, InboxObservation::Page(Value::Null)),
                peer(remote, InboxObservation::Unreachable("timeout")),
                peer(other, InboxObservation::Refused("project_unknown")),
            ],
        )
        .unwrap();
        let value = serde_json::to_value(item).unwrap();
        assert_eq!(value["peers"][0]["state"], "partial");
        assert_eq!(value["peers"][0]["reason"], "invalid_page");
        assert_eq!(value["peers"][1]["reason"], "duplicate_host");
        assert_eq!(value["peers"][2]["state"], "partial");
        assert_eq!(value["peers"][2]["reason"], "project_unknown");
        let item = compose_inbox(
            listed(),
            vec![peer(remote, InboxObservation::Unreachable("timeout"))],
        )
        .unwrap();
        let value = serde_json::to_value(item).unwrap();
        assert_eq!(value["peers"][0]["state"], "unreachable");
        assert_eq!(value["peers"][0]["reason"], "timeout");
    }

    #[test]
    fn peer_project_labels_are_bounded_display_text() {
        let remote = HostId::from_bytes([2; 16]);
        let project = ProjectId::from_bytes([9; 16]).to_string();
        let label = |label: Value| {
            let page = InboxObservation::Page(json!({"items":[request(remote, 2, Some("k"))],
                "truncated":false,"projects":{project.clone():{"label":label}}}));
            let result =
                compose_inbox(local(HostId::from_bytes([1; 16])), vec![peer(remote, page)])
                    .unwrap();
            serde_json::to_value(result).unwrap()["projects"][format!("{remote}:{project}")]
                ["label"]
                .clone()
        };
        assert!(label(json!({"nested":"object"})).is_null());
        assert!(label(json!(7)).is_null());
        assert!(label(json!("\u{1b}\n\t\u{0}")).is_null());
        assert_eq!(label(json!("re\u{1b}[31mpo\n")), "re[31mpo");
        // Bidi overrides and other format characters are removed too.
        assert_eq!(label(json!("a\u{202e}b\u{200b}c\u{2066}d\u{feff}")), "abcd");
        assert!(label(json!("\u{202e}\u{200f}")).is_null());
        // 130 two-byte characters are cut on a character boundary.
        let long = "é".repeat(130);
        assert_eq!(label(json!(long)), json!("é".repeat(128)));
    }
}
