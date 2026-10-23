//! Minimal receipt construction. Store locking and decision evidence belong to F/G.
use crate::{review, risk};
use provenant_core::{DeliveryId, Digest, ProjectId, UtcMicros};
use provenant_records::workflow::*;
use provenant_records::{CanonicalBytes, CanonicalV1, DecodeError, Limits, decode_v1};
use std::io::Read;

pub const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
pub const MAX_REVIEW_REFS: usize = 256;
const LIMITS: Limits = Limits {
    max_bytes: MAX_RECEIPT_BYTES,
    max_depth: 32,
    max_members: 8192,
};
#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error(transparent)]
    Risk(#[from] risk::RiskError),
    #[error(transparent)]
    Review(#[from] review::ReviewError),
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error("unsupported receipt schema version")]
    Version,
    #[error("receipt exceeds its byte or review-reference limit")]
    Limit,
}
#[derive(Debug, Clone)]
pub struct ReceiptInput {
    pub delivery_id: DeliveryId,
    pub project_id: ProjectId,
    pub subject: Subject,
    pub authors: Vec<Author>,
    pub risk: RiskRecord,
}
/// Produces revision one; the store compares expected revision zero before publishing.
pub fn new(
    input: ReceiptInput,
    now: UtcMicros,
    risk_policy: &RiskPolicy,
    review_policy: &ReviewPolicy,
    downgrade: Option<&UserApproval>,
) -> Result<DeliveryReceipt, ReceiptError> {
    risk::validate(&input.risk, risk_policy, downgrade)?;
    review::validate_policy(review_policy)?;
    let receipt = DeliveryReceipt::new(
        input.delivery_id,
        input.project_id,
        input.subject,
        input.authors,
        input.risk,
        now,
    )?;
    validate(&receipt)?;
    Ok(receipt)
}
/// Bounded stored-claim decoding, without claiming current verification or authority.
pub fn read(reader: impl Read) -> Result<DeliveryReceipt, ReceiptError> {
    let receipt = decode_v1(reader, LIMITS)?;
    validate(&receipt)?;
    Ok(receipt)
}
pub fn canonical(receipt: &DeliveryReceipt) -> Result<CanonicalBytes, ReceiptError> {
    validate_shape(receipt)?;
    let bytes = CanonicalV1::bytes(receipt)?;
    if bytes.as_bytes().len() as u64 > MAX_RECEIPT_BYTES {
        return Err(ReceiptError::Limit);
    }
    Ok(bytes)
}
pub fn digest(receipt: &DeliveryReceipt) -> Result<Digest, ReceiptError> {
    Ok(provenant_records::record_digest(
        "workflow.receipt",
        1,
        &canonical(receipt)?,
    )?)
}
pub fn validate(receipt: &DeliveryReceipt) -> Result<(), ReceiptError> {
    canonical(receipt).map(|_| ())
}
fn validate_shape(receipt: &DeliveryReceipt) -> Result<(), ReceiptError> {
    if receipt.schema_version() != 1 {
        return Err(ReceiptError::Version);
    }
    if receipt.review_attempts().len() > MAX_REVIEW_REFS {
        return Err(ReceiptError::Limit);
    }
    receipt.validate_shape()?;
    for author in receipt.authors() {
        if let Author::Agent { route, .. } = author {
            review::validate_provenance(route)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, AttemptId, GitOid, RelativePath, RepoId};
    use provenant_records::PathWire;
    fn time() -> UtcMicros {
        UtcMicros::from_micros(1_234_567).unwrap()
    }
    fn input() -> ReceiptInput {
        let factors = RiskFactors {
            blast_radius: BlastRadius::Local,
            reversibility: Reversibility::Easy,
            data_sensitivity: DataSensitivity::Public,
            migration: Migration::None,
            oracle_quality: OracleQuality::Strong,
            external_effects: ExternalEffects::None,
            critical_surface: CriticalSurface::None,
        };
        ReceiptInput {
            delivery_id: DeliveryId::from_bytes([1; 16]),
            project_id: ProjectId::from_bytes([2; 16]),
            subject: Subject::Pr {
                repository: RepoId::from_bytes([3; 16]),
                number: 42,
                head: GitOid::Sha256([4; 32]),
            },
            authors: vec![Author::User {
                actor_id: ActorId::from_bytes([5; 16]),
            }],
            risk: risk::assess(factors, RiskTier::Routine, &RiskPolicy::default(), None).unwrap(),
        }
    }
    fn receipt() -> DeliveryReceipt {
        new(
            input(),
            time(),
            &RiskPolicy::default(),
            &ReviewPolicy::default(),
            None,
        )
        .unwrap()
    }
    #[test]
    fn canonical_roundtrip_uses_exact_integer_time_and_both_git_widths() {
        for head in [GitOid::Sha1([4; 20]), GitOid::Sha256([4; 32])] {
            let mut input = input();
            input.subject = Subject::Pr {
                repository: RepoId::from_bytes([3; 16]),
                number: u64::MAX,
                head,
            };
            let receipt = new(
                input,
                time(),
                &RiskPolicy::default(),
                &ReviewPolicy::default(),
                None,
            )
            .unwrap();
            let bytes = canonical(&receipt).unwrap();
            assert!(!bytes.as_bytes().ends_with(b"\n"));
            let value: serde_json::Value = serde_json::from_slice(bytes.as_bytes()).unwrap();
            assert_eq!(value["created_at"], 1_234_567);
            assert_eq!(read(bytes.as_bytes()).unwrap(), receipt);
            let mut disk = bytes.as_bytes().to_vec();
            disk.push(b'\n');
            assert_eq!(
                digest(&read(disk.as_slice()).unwrap()).unwrap(),
                digest(&receipt).unwrap()
            );
        }
    }
    fn changed(change: impl FnOnce(&mut serde_json::Value)) -> DeliveryReceipt {
        let mut value = serde_json::to_value(receipt()).unwrap();
        change(&mut value);
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn stored_risk_requires_approval_exactly_for_a_downgrade() {
        for r in [
            changed(|v| v["risk"]["derived"] = "terminal".into()),
            changed(|v| {
                v["risk"]["downgrade_approval"] = serde_json::to_value(approval_ref()).unwrap()
            }),
        ] {
            assert!(validate(&r).is_err());
            assert!(read(serde_json::to_vec(&r).unwrap().as_slice()).is_err());
        }
        let r = changed(|v| {
            v["risk"]["derived"] = "terminal".into();
            v["risk"]["downgrade_approval"] = serde_json::to_value(approval_ref()).unwrap();
        });
        assert!(validate(&r).is_ok());
    }
    fn approval_ref() -> ApprovalRef {
        ApprovalRef {
            actor_id: ActorId::from_bytes([8; 16]),
            evidence: EvidenceRef {
                path: PathWire::from_relative(&RelativePath::parse("approval.md").unwrap())
                    .unwrap(),
                digest: Digest::from_bytes([9; 32]),
            },
        }
    }
    #[test]
    fn document_identity_binds_path_and_digest() {
        let r = changed(|v| {
            v["subject"] = serde_json::to_value(Subject::Document {
                path: PathWire::from_relative(&RelativePath::parse("docs/result.md").unwrap())
                    .unwrap(),
                digest: Digest::from_bytes([9; 32]),
            })
            .unwrap()
        });
        assert_eq!(read(canonical(&r).unwrap().as_bytes()).unwrap(), r);
        let mut value = serde_json::to_value(&r).unwrap();
        value["subject"]["digest"] = serde_json::to_value(Digest::from_bytes([8; 32])).unwrap();
        let other = serde_json::from_value(value).unwrap();
        assert_ne!(digest(&r).unwrap(), digest(&other).unwrap());
    }
    #[test]
    fn bounds_versions_unknown_tags_fields_and_duplicate_keys_fail_closed() {
        let bytes = canonical(&receipt()).unwrap();
        let text = std::str::from_utf8(bytes.as_bytes()).unwrap();
        for bad in [
            text.replace("\"schema_version\":1", "\"schema_version\":2"),
            text.replacen('{', "{\"extra\":0,", 1),
            text.replacen('{', "{\"revision\":1,", 1),
            text.replace("\"created_at\":1234567", "\"created_at\":1.5"),
            text.replace("\"revision\":1", "\"revision\":18446744073709551616"),
            text.replace("\"pending\"", "\"unknown\""),
        ] {
            assert!(read(bad.as_bytes()).is_err());
        }
        assert!(matches!(
            read(vec![b' '; MAX_RECEIPT_BYTES as usize + 1].as_slice()),
            Err(ReceiptError::Decode(DecodeError::TooLarge { .. }))
        ));
        let r = changed(|v| {
            v["review_attempts"] = serde_json::to_value(
                (0..257_u64)
                    .map(|i| {
                        let mut b = [0; 16];
                        b[..8].copy_from_slice(&i.to_be_bytes());
                        AttemptId::from_bytes(b)
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        });
        assert!(matches!(validate(&r), Err(ReceiptError::Limit)));
    }
    #[test]
    fn malformed_receipts_and_decisions_are_rejected() {
        for r in [
            changed(|v| {
                let author = v["authors"][0].clone();
                v["authors"].as_array_mut().unwrap().push(author);
            }),
            changed(|v| {
                v["review_attempts"] =
                    serde_json::to_value(vec![AttemptId::from_bytes([1; 16]); 2]).unwrap()
            }),
            changed(|v| v["revision"] = 0.into()),
            changed(|v| v["updated_at"] = 0.into()),
            changed(|v| v["authors"] = serde_json::json!([])),
        ] {
            assert!(validate(&r).is_err());
        }
        for at in [1_234_566, 1_234_568] {
            let r = changed(
                |v| v["decision"] = serde_json::json!({"status":"accepted", "actor_id": ActorId::from_bytes([1;16]), "at": at, "reason":"done", "user_approval":null}),
            );
            assert!(validate(&r).is_err());
        }
    }
    #[test]
    fn attested_and_unknown_authors_are_stored_without_promoting_lineage() {
        for route in [
            RouteProvenance::Unknown {},
            RouteProvenance::ChairAttested {
                route: "codex/gpt-6.1-sol@high".into(),
                tier: ModelTier::Principal,
                declared_family: None,
                evidence: approval_ref().evidence,
            },
        ] {
            let r = changed(|v| {
                v["authors"] = serde_json::to_value(vec![Author::Agent {
                    actor_id: ActorId::from_bytes([1; 16]),
                    route: route.clone(),
                }])
                .unwrap()
            });
            assert!(validate(&r).is_ok());
            assert_eq!(read(canonical(&r).unwrap().as_bytes()).unwrap(), r);
        }
        let r = changed(|v| {
            v["authors"] = serde_json::to_value(vec![Author::Agent {
                actor_id: ActorId::from_bytes([1; 16]),
                route: RouteProvenance::ChairAttested {
                    route: " ".into(),
                    tier: ModelTier::Principal,
                    declared_family: None,
                    evidence: approval_ref().evidence,
                },
            }])
            .unwrap()
        });
        assert!(validate(&r).is_err());
    }
}
