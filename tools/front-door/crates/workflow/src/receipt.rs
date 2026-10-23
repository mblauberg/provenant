//! Minimal receipt construction. Store locking and decision evidence belong to F/G.
use crate::{review, risk};
use provenant_core::{DeliveryId, Digest, ProjectId, UtcMicros};
use provenant_records::workflow::*;
use provenant_records::{CanonicalBytes, CanonicalV1, DecodeError, Limits, decode_v1};
use std::{collections::BTreeSet, io::Read};

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
    #[error("unsupported receipt schema version")]
    Version,
    #[error("receipt exceeds its byte or review-reference limit")]
    Limit,
    #[error("receipt requires unique authors and review attempt references")]
    Duplicate,
    #[error("receipt has an invalid subject, revision, timestamp or decision")]
    Malformed,
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
    let receipt = DeliveryReceipt {
        schema_version: 1,
        delivery_id: input.delivery_id,
        project_id: input.project_id,
        revision: 1,
        subject: input.subject,
        authors: input.authors,
        risk: input.risk,
        review_attempts: vec![],
        decision: Decision::Pending,
        created_at: now,
        updated_at: now,
    };
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
    if receipt.schema_version != 1 {
        return Err(ReceiptError::Version);
    }
    if receipt.review_attempts.len() > MAX_REVIEW_REFS {
        return Err(ReceiptError::Limit);
    }
    if receipt.authors.is_empty()
        || receipt.revision == 0
        || receipt.updated_at < receipt.created_at
        || matches!(&receipt.subject, Subject::Pr { number: 0, .. })
    {
        return Err(ReceiptError::Malformed);
    }
    let authors: BTreeSet<_> = receipt.authors.iter().map(Author::actor_id).collect();
    let reviews: BTreeSet<_> = receipt.review_attempts.iter().collect();
    if authors.len() != receipt.authors.len() || reviews.len() != receipt.review_attempts.len() {
        return Err(ReceiptError::Duplicate);
    }
    for author in &receipt.authors {
        if let Author::Agent { route, .. } = author {
            review::validate_provenance(route)?;
        }
    }
    match &receipt.decision {
        Decision::Pending => {}
        Decision::Accepted { at, reason, .. } | Decision::Abandoned { at, reason, .. } => {
            if *at < receipt.created_at || *at > receipt.updated_at || reason.trim().is_empty() {
                return Err(ReceiptError::Malformed);
            }
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
    #[test]
    fn document_identity_binds_path_and_digest_without_blob_storage() {
        let mut receipt = receipt();
        receipt.subject = Subject::Document {
            path: PathWire::from_relative(&RelativePath::parse("docs/result.md").unwrap()).unwrap(),
            digest: Digest::from_bytes([9; 32]),
        };
        let previous = digest(&receipt).unwrap();
        let mut changed = receipt.clone();
        changed.subject = Subject::Document {
            path: PathWire::from_relative(&RelativePath::parse("docs/result.md").unwrap()).unwrap(),
            digest: Digest::from_bytes([8; 32]),
        };
        assert_ne!(previous, digest(&changed).unwrap());
        assert_eq!(
            read(canonical(&receipt).unwrap().as_bytes()).unwrap(),
            receipt
        );
    }
    #[test]
    fn bounds_versions_unknown_fields_and_duplicate_keys_fail_closed() {
        let bytes = canonical(&receipt()).unwrap();
        let text = std::str::from_utf8(bytes.as_bytes()).unwrap();
        for bad in [
            text.replace("\"schema_version\":1", "\"schema_version\":2"),
            text.replacen('{', "{\"extra\":0,", 1),
            text.replacen('{', "{\"revision\":1,", 1),
            text.replace("\"created_at\":1234567", "\"created_at\":1.5"),
            text.replace("\"revision\":1", "\"revision\":18446744073709551616"),
        ] {
            assert!(read(bad.as_bytes()).is_err());
        }
        assert!(matches!(
            read(vec![b' '; MAX_RECEIPT_BYTES as usize + 1].as_slice()),
            Err(ReceiptError::Decode(DecodeError::TooLarge { .. }))
        ));
        let mut r = receipt();
        r.review_attempts = (0..256_u64)
            .map(|i| {
                let mut b = [0; 16];
                b[..8].copy_from_slice(&i.to_be_bytes());
                AttemptId::from_bytes(b)
            })
            .collect();
        assert!(canonical(&r).is_ok());
        r.review_attempts.push(AttemptId::from_bytes([255; 16]));
        assert!(matches!(canonical(&r), Err(ReceiptError::Limit)));
    }
    #[test]
    fn malformed_receipts_and_duplicate_identities_are_rejected() {
        let mut r = receipt();
        r.authors.push(r.authors[0].clone());
        assert!(matches!(validate(&r), Err(ReceiptError::Duplicate)));
        let mut r = receipt();
        r.review_attempts = vec![AttemptId::from_bytes([1; 16]); 2];
        assert!(matches!(validate(&r), Err(ReceiptError::Duplicate)));
        let mut r = receipt();
        r.updated_at = UtcMicros::from_micros(0).unwrap();
        assert!(matches!(validate(&r), Err(ReceiptError::Malformed)));
        let mut r = receipt();
        r.revision = 0;
        assert!(matches!(validate(&r), Err(ReceiptError::Malformed)));
        let mut i = input();
        i.authors.clear();
        assert!(
            new(
                i,
                time(),
                &RiskPolicy::default(),
                &ReviewPolicy::default(),
                None
            )
            .is_err()
        );
        let mut i = input();
        i.risk.derived = RiskTier::Terminal;
        assert!(matches!(
            new(
                i,
                time(),
                &RiskPolicy::default(),
                &ReviewPolicy::default(),
                None
            ),
            Err(ReceiptError::Risk(risk::RiskError::Mismatch))
        ));
        let mut r = receipt();
        r.decision = Decision::Abandoned {
            actor_id: ActorId::from_bytes([1; 16]),
            at: time(),
            reason: "x".repeat(65536),
        };
        assert!(matches!(canonical(&r), Err(ReceiptError::Limit)));
    }
}
