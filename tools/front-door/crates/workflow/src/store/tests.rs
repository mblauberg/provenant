#![allow(clippy::unwrap_used)]
use super::*;
use crate::{receipt, risk};
use provenant_core::{ActorId, GitOid, MonoTime, RepoId, RootInputs, TestClock, UtcMicros};
use provenant_records::workflow::*;
use std::io;
struct Resolver;
impl provenant_core::PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &std::path::Path) -> io::Result<std::path::PathBuf> {
        Ok(path.into())
    }
}
fn setup() -> (tempfile::TempDir, ReceiptStore, DeliveryReceipt) {
    let temp = crate::test_support::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home");
    let state = base.join("state");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&state).unwrap();
    // All roots are explicit, including HOME; no environment/harness config is read.
    let mut inputs = RootInputs::new(home, base.join("checkout"));
    inputs.state_dir = Some(state);
    let root = StateRoot::resolve(&inputs, &Resolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let delivery = DeliveryId::from_bytes([2; 16]);
    let factors = RiskFactors {
        blast_radius: BlastRadius::Local,
        reversibility: Reversibility::Easy,
        data_sensitivity: DataSensitivity::Public,
        migration: Migration::None,
        oracle_quality: OracleQuality::Strong,
        external_effects: ExternalEffects::None,
        critical_surface: CriticalSurface::None,
    };
    let input = receipt::ReceiptInput {
        project_id: project,
        delivery_id: delivery,
        subject: Subject::Pr {
            repository: RepoId::from_bytes([3; 16]),
            number: 1,
            head: GitOid::Sha1([4; 20]),
        },
        authors: vec![Author::User {
            actor_id: ActorId::from_bytes([5; 16]),
        }],
        risk: risk::assess(factors, RiskTier::Routine, &RiskPolicy::default(), None).unwrap(),
    };
    let receipt = receipt::new(
        input,
        time(10),
        &RiskPolicy::default(),
        &ReviewPolicy::default(),
        None,
    )
    .unwrap();
    (temp, ReceiptStore::new(root, project, delivery), receipt)
}
fn time(value: i64) -> UtcMicros {
    UtcMicros::from_micros(value).unwrap()
}
fn clock(value: i64) -> TestClock {
    TestClock::new(time(value), MonoTime::from_micros(0))
}
fn save(
    store: &ReceiptStore,
    r: &DeliveryReceipt,
    revision: u64,
    now: i64,
) -> Result<DeliveryReceipt, StoreError> {
    let clock = clock(now);
    store
        .save(
            r,
            revision,
            SaveContext {
                risk_policy: &RiskPolicy::default(),
                downgrade_approval: None,
                subject: r.subject(),
                valid_until: time(1000),
            },
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock,
        )
        .map(|saved| saved.receipt)
}
#[test]
fn saves_one_complete_receipt_and_rejects_a_stale_revision() {
    let (_temp, store, r) = setup();
    assert!(store.read().unwrap().is_none());
    assert!(!store.root.as_path().join("workflow").exists());
    let first = save(&store, &r, 0, 20).unwrap();
    assert_eq!(first.revision(), 1);
    assert_eq!(first.updated_at(), time(20));
    assert_eq!(store.read().unwrap(), Some(first.clone()));
    assert!(save(&store, &first, 0, 30).is_err());
    // A pre-rename crash can leave an unrelated staging file. Saves ignore it.
    let orphan = directory(&store).join(".receipt.crashed");
    std::fs::write(&orphan, b"partial receipt").unwrap();
    let second = save(&store, &first, 1, 30).unwrap();
    assert_eq!(std::fs::read(orphan).unwrap(), b"partial receipt");
    assert_eq!(second.revision(), 2);
    assert_eq!(second.created_at(), first.created_at());
    assert_eq!(store.read().unwrap(), Some(second));
}

/// Test-only fault seam: production always uses supervisor's held handles.
pub(super) trait FilesystemFaults {
    fn read(&self) -> Result<(), FsError> {
        Ok(())
    }
    fn lock(&self) -> Result<(), FsError> {
        Ok(())
    }
    fn atomic_write(&self, dir: &Directory, bytes: &[u8]) -> Result<Publication, FsError> {
        dir.atomic_write(
            OsStr::new("receipt.json"),
            ".receipt.",
            bytes,
            PublishMode::Replace,
        )
    }
}

#[cfg(test)]
#[path = "concurrency.rs"]
mod concurrency;
fn directory(store: &ReceiptStore) -> std::path::PathBuf {
    store.root.as_path().join(format!(
        "workflow/projects/{}/deliveries/{}",
        store.project, store.delivery
    ))
}
fn changed(r: &DeliveryReceipt, change: impl FnOnce(&mut serde_json::Value)) -> DeliveryReceipt {
    let mut value = serde_json::to_value(r).unwrap();
    change(&mut value);
    serde_json::from_value(value).unwrap()
}
#[test]
fn refuses_stale_subject_identity_versions_and_bounds_without_publication() {
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    let original = std::fs::read(directory(&store).join("receipt.json")).unwrap();
    for (field, value) in [
        (
            "project_id",
            serde_json::to_value(ProjectId::from_bytes([8; 16])).unwrap(),
        ),
        (
            "delivery_id",
            serde_json::to_value(DeliveryId::from_bytes([8; 16])).unwrap(),
        ),
    ] {
        let invalid = changed(&first, |v| v[field] = value);
        assert!(matches!(
            save(&store, &invalid, 1, 30),
            Err(StoreError::Identity)
        ));
    }
    let other = changed(&first, |v| {
        v["subject"]["head"] = serde_json::to_value(GitOid::Sha256([7; 32])).unwrap()
    });
    let stale_clock = clock(30);
    assert!(matches!(
        store.save(
            &other,
            1,
            SaveContext {
                subject: first.subject(),
                valid_until: time(1000),
                risk_policy: &RiskPolicy::default(),
                downgrade_approval: None
            },
            WaitBudget::cli(stale_clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &stale_clock
        ),
        Err(StoreError::StaleSubject)
    ));
    assert!(matches!(
        save(
            &store,
            &changed(&first, |v| v["schema_version"] = 2.into()),
            1,
            30
        ),
        Err(StoreError::Version)
    ));
    let over_refs = changed(&first, |v| {
        v["review_attempts"] = serde_json::to_value(
            (0..257_u64)
                .map(|i| {
                    let mut id = [0; 16];
                    id[..8].copy_from_slice(&i.to_be_bytes());
                    provenant_core::AttemptId::from_bytes(id)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    });
    assert!(matches!(
        save(&store, &over_refs, 1, 30),
        Err(StoreError::Limit)
    ));
    let huge = changed(
        &first,
        |v| v["decision"] = serde_json::json!({"status":"abandoned","actor_id":ActorId::from_bytes([6;16]),"at":20,"reason":"x".repeat(65536),"user_approval":null}),
    );
    assert!(matches!(save(&store, &huge, 1, 30), Err(StoreError::Limit)));
    assert_eq!(
        std::fs::read(directory(&store).join("receipt.json")).unwrap(),
        original
    );
    let file = directory(&store).join("receipt.json");
    for (bytes, kind) in [
        (b"{".to_vec(), "malformed"),
        (original.to_vec(), "valid"),
        (vec![b' '; 65537], "limit"),
    ] {
        std::fs::write(&file, bytes).unwrap();
        match kind {
            "malformed" => {
                assert!(matches!(store.read(), Err(StoreError::Malformed(_))));
                assert!(matches!(
                    save(&store, &first, 1, 30),
                    Err(StoreError::Malformed(_))
                ));
                assert_eq!(std::fs::read(&file).unwrap(), b"{");
            }
            "limit" => assert!(matches!(store.read(), Err(StoreError::Limit))),
            _ => assert!(store.read().is_ok()),
        }
    }
    let version = changed(&first, |v| v["schema_version"] = 2.into());
    std::fs::write(&file, serde_json::to_vec(&version).unwrap()).unwrap();
    assert!(matches!(store.read(), Err(StoreError::Version)));
}
#[test]
fn lock_inode_is_stable_and_cancellation_deadline_contention_are_distinct() {
    use std::os::unix::fs::MetadataExt;
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    let path = directory(&store);
    let inode = std::fs::metadata(path.join("receipt.lock")).unwrap().ino();
    let clock = clock(30);
    let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let policy = RiskPolicy::default();
    let context = || SaveContext {
        risk_policy: &policy,
        downgrade_approval: None,
        subject: first.subject(),
        valid_until: time(1000),
    };
    assert!(matches!(
        store.save(&first, 1, context(), budget, &cancel, &clock),
        Err(StoreError::Cancelled)
    ));
    let expired = TestClock::new(time(30), MonoTime::from_micros(1_000_000));
    assert!(matches!(
        store.save(&first, 1, context(), budget, &CancelToken::new(), &expired),
        Err(StoreError::Deadline)
    ));
    // A tiny remaining budget exercises supervisor's nonblocking lock poll.
    let almost_expired = TestClock::new(time(30), MonoTime::from_micros(999_999));
    let dir = Directory::open_anchor(&path).unwrap();
    let guard = dir
        .lock(
            OsStr::new("receipt.lock"),
            LockMode::Exclusive,
            budget,
            &CancelToken::new(),
            &clock,
        )
        .unwrap();
    assert!(matches!(
        store.save(
            &first,
            1,
            context(),
            budget,
            &CancelToken::new(),
            &almost_expired
        ),
        Err(StoreError::Contention)
    ));
    struct ExpiringClock(std::cell::Cell<u32>);
    impl Clock for ExpiringClock {
        fn wall_now(&self) -> Result<UtcMicros, provenant_core::TimeError> {
            Ok(time(30))
        }
        fn mono_now(&self) -> MonoTime {
            let call = self.0.get();
            self.0.set(call + 1);
            MonoTime::from_micros(if call < 2 { 999_999 } else { 1_000_000 })
        }
        fn wait_until(&self, _deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            Ok(())
        }
    }
    assert!(matches!(
        store.save(
            &first,
            1,
            context(),
            budget,
            &CancelToken::new(),
            &ExpiringClock(std::cell::Cell::new(0))
        ),
        Err(StoreError::Deadline)
    ));
    let other = ReceiptStore::new(
        store.root.clone(),
        store.project,
        DeliveryId::from_bytes([9; 16]),
    );
    let other_receipt = changed(&r, |v| {
        v["delivery_id"] = serde_json::to_value(other.delivery).unwrap()
    });
    save(&other, &other_receipt, 0, 30).unwrap();
    let session = crate::session::SessionStore::new(
        store.root.clone(),
        store.project,
        crate::session::HostSessionId::parse("host-1").unwrap(),
    );
    session
        .save(b"independent", budget, &CancelToken::new(), &clock)
        .unwrap();
    drop(guard);
    save(&store, &first, 1, 30).unwrap();
    assert_eq!(
        std::fs::metadata(path.join("receipt.lock")).unwrap().ino(),
        inode
    );
}
#[test]
fn injected_read_and_lock_errors_are_redacted_and_preserve_the_receipt() {
    struct Fault(bool);
    fn failure() -> FsError {
        FsError::Io {
            operation: "injected read/lock",
            path: "/private/secret-root".into(),
            source: io::Error::other("secret value"),
        }
    }
    impl FilesystemFaults for Fault {
        fn read(&self) -> Result<(), FsError> {
            if self.0 { Err(failure()) } else { Ok(()) }
        }
        fn lock(&self) -> Result<(), FsError> {
            if self.0 { Ok(()) } else { Err(failure()) }
        }
    }
    let (_temp, mut store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    for read in [true, false] {
        store.faults = Some(Box::new(Fault(read)));
        let error = save(&store, &first, 1, 30).unwrap_err();
        assert!(matches!(error, StoreError::Io { .. }));
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains(store.root.as_path().to_str().unwrap()));
    }
    store.faults = None;
    assert_eq!(store.read().unwrap(), Some(first));
}
#[test]
fn clock_regression_is_diagnosed_and_expiry_uses_unclamped_time() {
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    let clock = clock(15);
    let result = store
        .save(
            &first,
            1,
            SaveContext {
                risk_policy: &RiskPolicy::default(),
                downgrade_approval: None,
                subject: first.subject(),
                valid_until: time(18),
            },
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock,
        )
        .unwrap();
    assert_eq!(
        result.clock_regression,
        Some(ClockRegression {
            previous: time(20),
            observed: time(15)
        })
    );
    assert_eq!(result.receipt.updated_at(), time(20));
    assert_eq!(result.receipt.revision(), 2);
    assert!(matches!(
        store.save(
            &result.receipt,
            2,
            SaveContext {
                risk_policy: &RiskPolicy::default(),
                downgrade_approval: None,
                subject: first.subject(),
                valid_until: time(15)
            },
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock
        ),
        Err(StoreError::Expired)
    ));
    // Stored creation/revision metadata cannot be replaced by a caller's values.
    let tampered = changed(&result.receipt, |v| {
        v["created_at"] = 0.into();
        v["updated_at"] = 900.into();
        v["revision"] = 999.into();
    });
    let next = save(&store, &tampered, 2, 30).unwrap();
    assert_eq!(next.created_at(), first.created_at());
    assert_eq!(next.updated_at(), time(30));
    assert_eq!(next.revision(), 3);
}
#[test]
fn time_is_read_under_lock_and_cancellation_before_staging_has_no_effect() {
    struct FaultClock<'a> {
        directory: std::path::PathBuf,
        cancel: &'a CancelToken,
        fail: bool,
    }
    impl Clock for FaultClock<'_> {
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(0)
        }
        fn wall_now(&self) -> Result<UtcMicros, provenant_core::TimeError> {
            let zero = TestClock::new(time(30), MonoTime::from_micros(1_000_000));
            assert!(matches!(
                Directory::open_anchor(&self.directory).unwrap().lock(
                    OsStr::new("receipt.lock"),
                    LockMode::Exclusive,
                    WaitBudget::cli(MonoTime::from_micros(0), 1).unwrap(),
                    &CancelToken::new(),
                    &zero
                ),
                Err(FsError::LockTimeout { .. })
            ));
            if self.fail {
                return Err(provenant_core::TimeError::WallRange);
            }
            self.cancel.cancel();
            Ok(time(30))
        }
        fn wait_until(&self, _deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            Ok(())
        }
    }
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    for fail in [true, false] {
        let cancel = CancelToken::new();
        let clock = FaultClock {
            directory: directory(&store),
            cancel: &cancel,
            fail,
        };
        let error = store
            .save(
                &first,
                1,
                SaveContext {
                    risk_policy: &RiskPolicy::default(),
                    downgrade_approval: None,
                    subject: first.subject(),
                    valid_until: time(1000),
                },
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &cancel,
                &clock,
            )
            .unwrap_err();
        if fail {
            assert!(matches!(error, StoreError::Clock(_)));
        } else {
            assert!(matches!(error, StoreError::Cancelled));
        }
        assert_eq!(store.read().unwrap(), Some(first.clone()));
    }
}
#[test]
fn refuses_symlinks_nonregular_receipts_and_overflow_without_touching_targets() {
    use std::os::unix::fs::symlink;
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    let path = directory(&store);
    let external = store.root.as_path().join("external");
    std::fs::write(&external, b"private").unwrap();
    for leaf in ["receipt.json", "receipt.lock"] {
        let target = path.join(leaf);
        let old = std::fs::read(&target).unwrap();
        std::fs::remove_file(&target).unwrap();
        symlink(&external, &target).unwrap();
        assert!(save(&store, &first, 1, 30).is_err());
        assert_eq!(std::fs::read(&external).unwrap(), b"private");
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&target, old).unwrap();
    }
    std::fs::remove_file(path.join("receipt.json")).unwrap();
    std::fs::create_dir(path.join("receipt.json")).unwrap();
    assert!(save(&store, &first, 1, 30).is_err());
    std::fs::remove_dir(path.join("receipt.json")).unwrap();
    let max = changed(&first, |v| v["revision"] = u64::MAX.into());
    std::fs::write(
        path.join("receipt.json"),
        receipt::canonical(&max).unwrap().as_bytes(),
    )
    .unwrap();
    assert!(matches!(
        save(&store, &max, u64::MAX, 30),
        Err(StoreError::RevisionOverflow)
    ));
    assert_eq!(store.read().unwrap(), Some(max));
    let moved = path.with_extension("held");
    std::fs::rename(&path, &moved).unwrap();
    symlink(&moved, &path).unwrap();
    assert!(store.read().is_err());
    assert!(save(&store, &first, 1, 30).is_err());
}
#[test]
fn changing_subject_resets_decision_and_requires_fresh_review() {
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    let accepted = changed(&first, |v| {
        v["decision"] = serde_json::json!({"status":"accepted","actor_id":ActorId::from_bytes([6;16]),"at":20,"reason":"done","user_approval":null});
        v["review_attempts"] =
            serde_json::to_value(vec![provenant_core::AttemptId::from_bytes([7; 16])]).unwrap();
    });
    let accepted = save(&store, &accepted, 1, 30).unwrap();
    let refreshed = changed(&accepted, |v| {
        v["subject"]["head"] = serde_json::to_value(GitOid::Sha256([8; 32])).unwrap()
    });
    let updated = save(&store, &refreshed, 2, 40).unwrap();
    assert_eq!(updated.subject(), refreshed.subject());
    assert_eq!(updated.decision().status(), DecisionStatus::Pending);
    assert!(updated.review_attempts().is_empty());
    assert_eq!(updated.revision(), 3);
    assert_eq!(updated.created_at(), first.created_at());
    let clock = clock(50);
    assert!(matches!(
        store.save(
            &refreshed,
            3,
            SaveContext {
                risk_policy: &RiskPolicy::default(),
                downgrade_approval: None,
                subject: first.subject(),
                valid_until: time(1000)
            },
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock
        ),
        Err(StoreError::StaleSubject)
    ));
}
#[test]
fn each_save_rechecks_current_risk_and_originating_downgrade_approval() {
    use provenant_core::{Digest, RelativePath};
    use provenant_records::PathWire;
    let (_temp, store, r) = setup();
    let first = save(&store, &r, 0, 20).unwrap();
    let policy = RiskPolicy {
        minimum: RiskTier::Terminal,
    };
    let clock = clock(30);
    let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
    let context = SaveContext {
        subject: first.subject(),
        valid_until: time(1000),
        risk_policy: &policy,
        downgrade_approval: None,
    };
    assert!(matches!(
        store.save(&first, 1, context, budget, &CancelToken::new(), &clock),
        Err(StoreError::Risk(_))
    ));
    let approval = UserApproval {
        actor_id: ActorId::from_bytes([9; 16]),
        evidence: EvidenceRef {
            path: PathWire::from_relative(&RelativePath::parse("approval.md").unwrap()).unwrap(),
            digest: Digest::from_bytes([6; 32]),
        },
    };
    let approved_risk = risk::assess(
        first.risk().factors().clone(),
        RiskTier::Routine,
        &policy,
        Some(&approval),
    )
    .unwrap();
    let downgraded = changed(&first, |v| {
        v["risk"] = serde_json::to_value(approved_risk).unwrap()
    });
    let mismatched = UserApproval {
        actor_id: ActorId::from_bytes([8; 16]),
        ..approval.clone()
    };
    for supplied in [None, Some(&mismatched)] {
        let context = SaveContext {
            subject: first.subject(),
            valid_until: time(1000),
            risk_policy: &policy,
            downgrade_approval: supplied,
        };
        assert!(matches!(
            store.save(&downgraded, 1, context, budget, &CancelToken::new(), &clock),
            Err(StoreError::Risk(_))
        ));
    }
    let context = SaveContext {
        subject: first.subject(),
        valid_until: time(1000),
        risk_policy: &policy,
        downgrade_approval: Some(&approval),
    };
    assert_eq!(
        store
            .save(&downgraded, 1, context, budget, &CancelToken::new(), &clock)
            .unwrap()
            .receipt
            .revision(),
        2
    );
}

#[test]
fn atomic_write_failures_through_save_preserve_complete_bytes_and_redact_errors() {
    use provenant_supervisor::fs::{AtomicWriteOperations, FileHandle};
    use std::{io::Write as _, path::Path};
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Fault {
        Write,
        NoSpace,
        FileSync,
        Rename,
        DirectorySync,
    }
    fn failure(fault: Fault, path: &Path) -> FsError {
        FsError::Io {
            operation: "injected atomic write",
            path: path.into(),
            source: if fault == Fault::NoSpace {
                io::Error::from_raw_os_error(28) // ENOSPC on both supported hosts.
            } else {
                io::Error::other("secret value")
            },
        }
    }
    impl AtomicWriteOperations for Fault {
        fn write(
            &mut self,
            file: &mut FileHandle,
            bytes: &[u8],
            path: &Path,
        ) -> Result<(), FsError> {
            if matches!(self, Self::Write | Self::NoSpace) {
                file.write_all(&bytes[..bytes.len() / 2]).unwrap();
                Err(failure(*self, path))
            } else {
                file.write_all(bytes).map_err(|_| failure(*self, path))
            }
        }
        fn sync_file(&mut self, file: &FileHandle) -> Result<(), FsError> {
            if *self == Self::FileSync {
                Err(failure(*self, Path::new("/private/secret-root")))
            } else {
                file.sync()
            }
        }
        fn rename(
            &mut self,
            dir: &Directory,
            temp: &OsStr,
            name: &OsStr,
            mode: PublishMode,
        ) -> Result<(), FsError> {
            if *self == Self::Rename {
                Err(FsError::Rename {
                    from_path: "/private/secret-root/temp".into(),
                    to_path: "/private/secret-root/receipt.json".into(),
                    source: io::Error::other("secret value"),
                })
            } else {
                match mode {
                    PublishMode::Replace => dir.replace(temp, dir, name),
                    PublishMode::NoReplace => dir.rename_noreplace(temp, dir, name),
                }
            }
        }
        fn sync_directory(&mut self, dir: &Directory) -> Result<(), FsError> {
            if *self == Self::DirectorySync {
                Err(failure(*self, Path::new("/private/secret-root")))
            } else {
                dir.sync()
            }
        }
    }
    impl FilesystemFaults for Fault {
        fn atomic_write(&self, dir: &Directory, bytes: &[u8]) -> Result<Publication, FsError> {
            let mut fault = *self;
            dir.atomic_write_with_faults(
                OsStr::new("receipt.json"),
                ".receipt.",
                bytes,
                PublishMode::Replace,
                &mut fault,
            )
        }
    }
    for fault in [
        Fault::Write,
        Fault::NoSpace,
        Fault::FileSync,
        Fault::Rename,
        Fault::DirectorySync,
    ] {
        let (_temp, mut store, candidate) = setup();
        let first = save(&store, &candidate, 0, 20).unwrap();
        let old_bytes = std::fs::read(directory(&store).join("receipt.json")).unwrap();
        let next = changed(&first, |v| {
            v["revision"] = 2.into();
            v["updated_at"] = 30.into();
        });
        store.faults = Some(Box::new(fault));
        let error = save(&store, &first, 1, 30).unwrap_err();
        if fault == Fault::DirectorySync {
            assert!(matches!(error, StoreError::Durability(_)));
        } else {
            assert!(matches!(error, StoreError::Io { .. }));
        }
        if fault == Fault::NoSpace {
            assert!(matches!(&error, StoreError::Io { cause, .. } if cause.os_code == Some(28)));
        }
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains(store.root.as_path().to_str().unwrap()));
        store.faults = None;
        let expected = if fault == Fault::DirectorySync {
            next
        } else {
            first
        };
        assert_eq!(store.read().unwrap(), Some(expected.clone()));
        let bytes = std::fs::read(directory(&store).join("receipt.json")).unwrap();
        assert_eq!(bytes, receipt::canonical(&expected).unwrap().as_bytes());
        assert_eq!(bytes == old_bytes, fault != Fault::DirectorySync);
        let mut entries = std::fs::read_dir(directory(&store))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, ["receipt.json", "receipt.lock"]);
    }
}
