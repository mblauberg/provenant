//! Tests for launch-pin reads and the sealed settlement-evidence record.

use super::*;
use provenant_core::GitOid;
use provenant_os::fs::Directory;
use provenant_records::{
    declaration::DeclarationEvidence,
    run::Outcome,
    settlement::{
        CustodyFacts, GitEvidence, HeadProgress, HostFacts, IdentityEvidence, IdentityPolicyEffect,
        LaneRole, PrimaryCheckout, SETTLEMENT_EVIDENCE_SCHEMA,
    },
};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn oid(fill: u8) -> GitOid {
    GitOid::parse(&format!("{fill:02x}").repeat(20)).expect("a 40-digit SHA-1 object id")
}

fn evidence(pinned: Option<GitOid>, head: Option<GitOid>) -> SettlementEvidence {
    SettlementEvidence {
        schema_version: SETTLEMENT_EVIDENCE_SCHEMA,
        lane: LaneRole::ModelWriter,
        host: HostFacts {
            outcome: Outcome::Succeeded,
            cause: None,
            complete: true,
            result_empty: false,
            reason: None,
            terminal_note: None,
            signals: vec![],
            denial: None,
            host_handoff: None,
        },
        handoff: DeclarationEvidence::Missing,
        declarations: Default::default(),
        git: Some(GitEvidence {
            input_head: pinned,
            writer_head: head,
            uncommitted: None,
            uncommitted_paths: Vec::new(),
            pushed_head: None,
            primary: PrimaryCheckout::NotChecked,
        }),
        custody: CustodyFacts {
            launched: true,
            cancelled: false,
            steered: false,
            cancellation_delivered: Some(true),
            import_cancelled: false,
            process_exit: true,
            stall: None,
            identity: IdentityEvidence {
                conflict: false,
                incomplete: IdentityPolicyEffect::Complete,
                eof_fragment_unsettles: false,
                route: provenant_records::settlement::RouteIdentityFact::Observed,
            },
        },
        policy: provenant_records::settlement::policy::SettlementPolicy::shipped(),
    }
}

fn seal_pins(dir: &Directory, base: GitOid) -> TestResult {
    let scope = provenant_records::custody::Scope {
        attempt: provenant_core::AttemptId::from_bytes([1; 16]),
        generation: 1,
    };
    let value = serde_json::json!({
        "schema_version": 1,
        "scope": serde_json::to_value(&scope)?,
        "base": base.to_string(),
    });
    let bytes = provenant_records::CanonicalV1::bytes(&value)?
        .as_bytes()
        .to_vec();
    crate::artifacts::seal(dir, "writer-git-pins.json", &bytes, crate::RECORD_LIMIT)?;
    Ok(())
}

fn seal_terminal(dir: &Directory, head: GitOid) -> TestResult {
    let value = serde_json::json!({ "schema_version": 1, "head": head.to_string() });
    let bytes = provenant_records::CanonicalV1::bytes(&value)?
        .as_bytes()
        .to_vec();
    crate::artifacts::seal(dir, "writer-terminal.json", &bytes, crate::RECORD_LIMIT)?;
    Ok(())
}

#[test]
fn pin_and_head_decide_progress() -> TestResult {
    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    let base = oid(0x11);
    let head = oid(0x22);
    seal_pins(&dir, base)?;
    seal_terminal(&dir, head)?;
    assert_eq!(launch_pin(&dir)?, Some(base));
    assert_eq!(terminal_head(&dir)?, Some(head));
    let git = evidence(Some(base), Some(head))
        .git
        .expect("writer evidence");
    assert_eq!(git.head_progress(), HeadProgress::Moved);

    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    seal_pins(&dir, base)?;
    seal_terminal(&dir, base)?;
    assert_eq!(launch_pin(&dir)?, Some(base));
    assert_eq!(terminal_head(&dir)?, Some(base));
    let git = evidence(Some(base), Some(base))
        .git
        .expect("writer evidence");
    assert_eq!(git.head_progress(), HeadProgress::Unchanged);
    Ok(())
}

#[test]
fn missing_pin_is_unknown() -> TestResult {
    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    assert_eq!(launch_pin(&dir)?, None);
    assert_eq!(terminal_head(&dir)?, None);
    let git = evidence(None, Some(oid(0x33)))
        .git
        .expect("writer evidence");
    assert_eq!(git.head_progress(), HeadProgress::Unknown);
    Ok(())
}

#[test]
fn read_evidence_is_absent_before_sealing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    assert!(read_evidence(&dir)?.is_none());
    Ok(())
}

#[test]
fn sealing_twice_with_same_bytes_keeps_one_record() -> TestResult {
    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    let record = evidence(Some(oid(0x44)), Some(oid(0x55)));
    let (first, first_digest) = seal_evidence(&dir, &record)?;
    let (second, second_digest) = seal_evidence(&dir, &record)?;
    assert_eq!(first, record);
    assert_eq!(second, first);
    assert_eq!(second_digest, first_digest);
    let (read, read_digest) = read_evidence(&dir)?.expect("sealed evidence");
    assert_eq!(read, first);
    assert_eq!(read_digest, first_digest);
    let raw = crate::artifacts::read_sealed(&dir, SETTLEMENT_EVIDENCE, crate::RECORD_LIMIT)?
        .expect("sealed record");
    assert_eq!(first_digest, CanonicalV1::file_digest(&raw));
    Ok(())
}

#[test]
fn differing_redrive_keeps_sealed_evidence_and_warns() -> TestResult {
    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    let first = evidence(Some(oid(0x66)), Some(oid(0x77)));
    let (sealed, digest) = seal_evidence(&dir, &first)?;

    let changed = evidence(Some(oid(0x88)), Some(oid(0x99)));
    let ((kept, kept_digest), warnings) =
        provenant_records::warning::capture(|| seal_evidence(&dir, &changed).expect("re-drive"));
    assert_eq!(kept, sealed);
    assert_eq!(kept_digest, digest);
    assert!(
        warnings
            .iter()
            .any(|w| { w.message == "settlement evidence differs on re-drive; sealed copy kept" }),
        "a differing re-drive must warn: {warnings:?}"
    );
    Ok(())
}

#[test]
fn restart_cannot_change_the_sealed_pin() -> TestResult {
    let temp = tempfile::tempdir()?;
    let dir = Directory::open_anchor(temp.path())?;
    let base = oid(0xaa);
    let head = oid(0xbb);
    let (sealed, digest) = seal_evidence(&dir, &evidence(Some(base), Some(head)))?;

    // A restart re-reads HEAD and re-prepares the writer at a different base,
    // but the sealed record owns the pin, so the fact cannot drift.
    seal_pins(&dir, oid(0xcc))?;
    let (kept, kept_digest) = seal_evidence(&dir, &evidence(Some(oid(0xcc)), Some(oid(0xdd))))?;
    assert_eq!(kept, sealed);
    assert_eq!(kept_digest, digest);
    let git = kept.git.as_ref().expect("writer evidence");
    assert_eq!(git.input_head, Some(base));
    assert_eq!(git.head_progress(), HeadProgress::Moved);
    Ok(())
}
