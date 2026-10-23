#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use provenant_core::{PathResolver, RootInputs};
use provenant_records::{SourceInput, source_digest};
use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
use std::{
    fs, io,
    os::unix::fs::{PermissionsExt, symlink},
};
struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, p: &Path) -> io::Result<PathBuf> {
        Ok(p.into())
    }
}
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
    targets: Targets,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        let source = base.join("staged");
        fs::create_dir(&home).unwrap();
        fs::create_dir_all(source.join("assets/skills/example")).unwrap();
        for (path, bytes, mode) in [
            ("provenant", b"fixture executable".as_slice(), 0o755),
            (
                "assets/skills/example/SKILL.md",
                b"---\nname: example\n---\n".as_slice(),
                0o600,
            ),
        ] {
            let p = source.join(path);
            fs::write(&p, bytes).unwrap();
            fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
        }
        let mut inputs = RootInputs::new(home.clone(), base.join("checkout"));
        // Explicit captured HOME/PROVENANT_STATE_DIR/harness inputs, no process
        // environment mutation or access to real config/state.
        inputs.environment_state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&Resolver).unwrap();
        let targets = Targets::new(roots, &home, &[Harness::Codex]);
        Self {
            _tmp: tmp,
            home,
            source,
            targets,
        }
    }
    fn release(&self) -> Release {
        let digest = source_digest(&[SourceInput {
            name: "src/lib.rs",
            bytes: b"fixture",
        }])
        .unwrap();
        crate::package_release(
            &self.source,
            digest,
            "fixture-target",
            "fixture-toolchain",
            &LocalFs,
        )
        .unwrap()
    }
    fn plan(&self) -> InstallPlan {
        let r = self.release();
        let s = Snapshots::capture(&r, &self.targets, &LocalFs).unwrap();
        plan_install(r, self.targets.clone(), s).unwrap()
    }
    fn link(&self) -> PathBuf {
        self.home.join(".codex/skills/example")
    }
    fn receipt(&self) -> PathBuf {
        self.targets
            .roots
            .state
            .as_path()
            .join("install/receipt.json")
    }
}
#[derive(Default)]
struct Reporter {
    seen: usize,
    fail: bool,
}
impl InstallReporter for Reporter {
    fn preview(&mut self, a: &[Action]) -> io::Result<()> {
        self.seen += a.len();
        if self.fail {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        } else {
            Ok(())
        }
    }
}
struct Smoke;
impl ReleaseSmoke for Smoke {
    fn check(&self, _: &Release, _: &CancelToken) -> io::Result<()> {
        Ok(())
    }
}
fn context<'a>(clock: &'a SystemClock, cancel: &'a CancelToken) -> InstallContext<'a> {
    InstallContext {
        fs: &LocalFs,
        clock,
        budget: WaitBudget::cli(clock.mono_now(), 1).unwrap(),
        cancel,
    }
}
#[test]
fn dry_run_and_broken_preview_create_nothing() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut reporter = Reporter::default();
    let outcome = install(
        &plan,
        &context(&clock, &cancel),
        &mut reporter,
        &Smoke,
        true,
    )
    .unwrap();
    assert!(outcome.dry_run);
    assert!(reporter.seen > 0);
    assert!(!f.targets.roots.state.as_path().exists());
    assert!(!f.targets.bin_dir.exists());
    assert!(!f.home.join(".codex").exists());
    let error = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter {
            fail: true,
            seen: 0,
        },
        &Smoke,
        false,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "preview output failed");
    assert!(!f.targets.roots.state.as_path().exists());
}
#[test]
fn occupied_skill_is_refused_without_touching_unmanaged_bytes() {
    let f = Fixture::new();
    fs::create_dir_all(f.link()).unwrap();
    fs::write(f.link().join("custom"), b"unmanaged").unwrap();
    let r = f.release();
    let s = Snapshots::capture(&r, &f.targets, &LocalFs).unwrap();
    let error = plan_install(r, f.targets.clone(), s).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("unmanaged destination conflicts: {}", f.link().display())
    );
    assert_eq!(fs::read(f.link().join("custom")).unwrap(), b"unmanaged");
    assert!(!f.receipt().exists());
}
#[test]
fn reapplication_and_post_file_crash_do_not_rewrite_exact_desired_entries() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(
        check_install(&f.targets, Some(&f.release()), "fixture-target", &LocalFs)
            .unwrap()
            .is_empty()
    );
    let binary = f.targets.bin_dir.join("provenant");
    let before = snapshot(&binary, &LocalFs).unwrap();
    let link_before = fs::symlink_metadata(f.link()).unwrap();
    assert!(f.plan().actions().is_empty());
    // Crash after every desired file/link has been published but before receipt.
    fs::remove_file(f.receipt()).unwrap();
    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions().first().unwrap().selector, "receipt");
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert_eq!(snapshot(&binary, &LocalFs).unwrap(), before);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::symlink_metadata(f.link()).unwrap().ino(),
        link_before.ino()
    );
    assert!(f.plan().actions().is_empty());
}
#[test]
fn crash_after_any_publication_reapplies_without_a_receipt() {
    let f = Fixture::new();
    let plan = f.plan();
    // A process can die after each individually durable write. Reproduce every
    // prefix using the same publication boundary, without a rollback journal.
    for action in plan.actions().iter().filter(|a| a.selector != "receipt") {
        let path = action.path.to_path_buf().unwrap();
        let desired = &plan.desired.get(&path).unwrap().value;
        publish(&path, desired, false, &LocalFs).unwrap();
        let recovery = f.plan();
        assert!(!recovery.actions().iter().any(|a| a.path == action.path));
        assert!(!f.receipt().exists());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
}
#[test]
fn bytes_and_modes_are_verified_and_existing_release_is_immutable() {
    let f = Fixture::new();
    f.release();
    fs::write(f.source.join("provenant"), b"tampered").unwrap();
    assert_eq!(
        Release::read(&f.source, "fixture-target", &LocalFs)
            .unwrap_err()
            .to_string(),
        format!(
            "release file bytes or mode differ: {}",
            f.source.join("provenant").display()
        )
    );
    let f = Fixture::new();
    f.release();
    fs::set_permissions(
        f.source.join("provenant"),
        fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(
        matches!(Release::read(&f.source, "fixture-target", &LocalFs), Err(InstallError::Tampered(p)) if p == f.source.join("provenant"))
    );
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let path = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string())
        .join("provenant");
    fs::write(&path, b"changed").unwrap();
    let r = f.release();
    let s = Snapshots::capture(&r, &f.targets, &LocalFs).unwrap();
    assert_eq!(
        plan_install(r, f.targets.clone(), s)
            .unwrap_err()
            .to_string(),
        format!("release file bytes or mode differ: {}", path.display())
    );
    assert_eq!(
        check_install(&f.targets, None, "fixture-target", &LocalFs)
            .unwrap()
            .first()
            .unwrap()
            .rule,
        "release_drift"
    );
}
#[test]
fn uninstall_keeps_edited_links_and_unmanaged_siblings_and_binary() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let sibling = f.link().with_file_name("personal");
    fs::write(&sibling, b"personal").unwrap();
    fs::remove_file(f.link()).unwrap();
    symlink("/foreign/skill", f.link()).unwrap();
    assert!(
        uninstall(
            &f.targets,
            &context(&clock, &cancel),
            &mut Reporter::default()
        )
        .unwrap()
        .actions
        .is_empty()
    );
    assert_eq!(
        fs::read_link(f.link()).unwrap(),
        PathBuf::from("/foreign/skill")
    );
    assert_eq!(fs::read(&sibling).unwrap(), b"personal");
    assert!(f.targets.bin_dir.join("provenant").is_file());
    fs::remove_file(f.link()).unwrap();
    let r = f.release();
    let path = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(r.manifest.source_digest.to_string())
        .join("assets/skills/example");
    symlink(path, f.link()).unwrap();
    let outcome = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert_eq!(outcome.actions.len(), 2);
    assert!(!f.link().exists());
    assert!(f.receipt().exists());
    assert!(f.targets.bin_dir.join("provenant").is_file());
}
#[test]
fn drift_between_preview_and_apply_refuses_before_file_writes() {
    let f = Fixture::new();
    let plan = f.plan();
    fs::create_dir_all(&f.targets.bin_dir).unwrap();
    fs::write(f.targets.bin_dir.join("provenant"), b"editor").unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let err = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "destination changed since preview: {}",
            f.targets.bin_dir.join("provenant").display()
        )
    );
    assert!(!f.receipt().exists());
    assert!(!f.targets.roots.state.as_path().join("releases").exists());
}
#[test]
fn explicit_harnesses_platform_links_and_schema_refuse_before_mutation() {
    let mut f = Fixture::new();
    let r = f.release();
    assert_eq!(
        Release::read(&f.source, "other", &LocalFs)
            .unwrap_err()
            .to_string(),
        "release platform does not match selected target"
    );
    f.targets.harnesses.clear();
    assert_eq!(
        Snapshots::capture(&r, &f.targets, &LocalFs)
            .unwrap_err()
            .to_string(),
        "select at least one harness explicitly, without duplicates"
    );
    assert!(!f.targets.roots.state.as_path().exists());
    let f = Fixture::new();
    f.release();
    fs::remove_file(f.source.join("provenant")).unwrap();
    symlink("/never/open", f.source.join("provenant")).unwrap();
    assert!(matches!(
        Release::read(&f.source, "fixture-target", &LocalFs),
        Err(InstallError::Tampered(_))
    ));
}
// Exercise the real filesystem seam so partial-apply accounting and recovery
// are tested without adding production-only failure hooks.
use provenant_core::{
    CancelToken as TestCancelToken, Clock as TestClock, WaitBudget as TestWaitBudget,
};
use provenant_supervisor::fs::{
    FsError as TestFsError, LockGuard, LockMode, Metadata, Publication, PublishMode,
    Result as TestFsResult, SafeDir, SafeFile, SafeFs,
};
use std::{
    ffi::{OsStr, OsString},
    sync::{Arc, atomic::Ordering},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultMode {
    ReceiptPublish,
    CancelAfterFirstPublication,
    SkillLinkSync,
}
struct FaultState {
    mode: FaultMode,
    fired: std::sync::atomic::AtomicBool,
    link_published: std::sync::atomic::AtomicBool,
    cancel: Option<TestCancelToken>,
}
struct FaultFs {
    state: Arc<FaultState>,
}
impl FaultFs {
    fn new(mode: FaultMode, cancel: Option<TestCancelToken>) -> Self {
        Self {
            state: Arc::new(FaultState {
                mode,
                fired: std::sync::atomic::AtomicBool::new(false),
                link_published: std::sync::atomic::AtomicBool::new(false),
                cancel,
            }),
        }
    }
    fn wrap(&self, dir: Box<dyn SafeDir>) -> Box<dyn SafeDir> {
        Box::new(FaultDir {
            inner: dir,
            state: self.state.clone(),
        })
    }
}
impl SafeFs for FaultFs {
    fn root(&self, absolute: bool) -> TestFsResult<Box<dyn SafeDir>> {
        Ok(self.wrap(LocalFs.root(absolute)?))
    }
    fn open_anchor(&self, path: &Path) -> TestFsResult<Box<dyn SafeDir>> {
        Ok(self.wrap(LocalFs.open_anchor(path)?))
    }
    fn open_source(&self, path: &Path) -> TestFsResult<Box<dyn SafeFile>> {
        LocalFs.open_source(path)
    }
    fn temporary_name(&self) -> TestFsResult<OsString> {
        LocalFs.temporary_name()
    }
}
struct FaultDir {
    inner: Box<dyn SafeDir>,
    state: Arc<FaultState>,
}
impl FaultDir {
    fn child(&self, dir: Box<dyn SafeDir>) -> Box<dyn SafeDir> {
        Box::new(Self {
            inner: dir,
            state: self.state.clone(),
        })
    }
    fn link_changed(&self, result: TestFsResult<()>) -> TestFsResult<()> {
        if result.is_ok() && self.state.mode == FaultMode::SkillLinkSync {
            self.state.link_published.store(true, Ordering::Release);
        }
        result
    }
}
impl SafeDir for FaultDir {
    fn metadata(&self) -> TestFsResult<Metadata> {
        self.inner.metadata()
    }
    fn metadata_at(&self, name: &OsStr, follow: bool) -> TestFsResult<Metadata> {
        self.inner.metadata_at(name, follow)
    }
    fn open_directory(&self, name: &OsStr, follow: bool) -> TestFsResult<Box<dyn SafeDir>> {
        Ok(self.child(self.inner.open_directory(name, follow)?))
    }
    fn open_verified_directory(&self, name: &OsStr) -> TestFsResult<Box<dyn SafeDir>> {
        Ok(self.child(self.inner.open_verified_directory(name)?))
    }
    fn open_read(&self, name: &OsStr) -> TestFsResult<Box<dyn SafeFile>> {
        self.inner.open_read(name)
    }
    fn create_new(&self, name: &OsStr) -> TestFsResult<Box<dyn SafeFile>> {
        self.inner.create_new(name)
    }
    fn mkdir(&self, name: &OsStr, mode: u32) -> TestFsResult<()> {
        self.inner.mkdir(name, mode)
    }
    fn entries(&self) -> TestFsResult<Box<dyn Iterator<Item = TestFsResult<OsString>>>> {
        self.inner.entries()
    }
    fn make_writable(&self) -> TestFsResult<()> {
        self.inner.make_writable()
    }
    fn lock(
        &self,
        name: &OsStr,
        mode: LockMode,
        budget: TestWaitBudget,
        cancel: &TestCancelToken,
        clock: &dyn TestClock,
    ) -> TestFsResult<LockGuard> {
        self.inner.lock(name, mode, budget, cancel, clock)
    }
    fn unlink(&self, name: &OsStr) -> TestFsResult<()> {
        self.inner.unlink(name)
    }
    fn rmdir(&self, name: &OsStr) -> TestFsResult<()> {
        self.inner.rmdir(name)
    }
    fn replace(&self, source: &OsStr, destination: &OsStr) -> TestFsResult<()> {
        self.link_changed(self.inner.replace(source, destination))
    }
    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> TestFsResult<()> {
        self.link_changed(self.inner.rename_noreplace(source, destination))
    }
    fn symlink(&self, target: &Path, name: &OsStr) -> TestFsResult<()> {
        self.inner.symlink(target, name)
    }
    fn read_link(&self, name: &OsStr) -> TestFsResult<PathBuf> {
        self.inner.read_link(name)
    }
    fn publish(
        &self,
        name: &OsStr,
        bytes: &[u8],
        permissions: u32,
        mode: PublishMode,
    ) -> TestFsResult<Publication> {
        if self.state.mode == FaultMode::ReceiptPublish
            && name == OsStr::new("receipt.json")
            && !self.state.fired.swap(true, Ordering::AcqRel)
        {
            return Err(TestFsError::Io {
                operation: "publish",
                path: PathBuf::from("receipt.json"),
                source: io::Error::other("injected receipt publication failure"),
            });
        }
        let publication = self.inner.publish(name, bytes, permissions, mode)?;
        if self.state.mode == FaultMode::CancelAfterFirstPublication
            && !self.state.fired.swap(true, Ordering::AcqRel)
            && let Some(cancel) = &self.state.cancel
        {
            cancel.cancel();
        }
        Ok(publication)
    }
    fn sync(&self) -> TestFsResult<()> {
        if self.state.mode == FaultMode::SkillLinkSync
            && self.state.link_published.swap(false, Ordering::AcqRel)
        {
            return Err(TestFsError::Io {
                operation: "sync",
                path: PathBuf::from("skill-link-parent"),
                source: io::Error::other("injected skill link directory sync failure"),
            });
        }
        self.inner.sync()
    }
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
    ) -> TestFsResult<Publication> {
        self.inner.atomic_write(name, prefix, bytes, mode)
    }
    fn replace_link(&self, name: &OsStr, target: &Path) -> TestFsResult<Publication> {
        self.inner.replace_link(name, target)
    }
}
fn fault_context<'a>(
    fs: &'a dyn SafeFs,
    clock: &'a SystemClock,
    cancel: &'a TestCancelToken,
) -> InstallContext<'a> {
    InstallContext {
        fs,
        clock,
        budget: TestWaitBudget::cli(clock.mono_now(), 1).unwrap(),
        cancel,
    }
}

#[test]
fn receipt_publish_failure_reports_partial_actions_and_reapplies() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let fault_fs = FaultFs::new(FaultMode::ReceiptPublish, None);
    let error = install(
        &plan,
        &fault_context(&fault_fs, &clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    let InstallError::Partial {
        completed,
        remaining,
        source,
    } = error
    else {
        panic!("expected partial install")
    };
    assert_eq!(completed, plan.actions()[..plan.actions().len() - 1]);
    assert_eq!(remaining, vec![plan.actions().last().unwrap().clone()]);
    assert_eq!(remaining[0].selector, "receipt");
    assert_eq!(
        source.to_string(),
        "filesystem publish failed for \"receipt.json\""
    );
    assert!(!f.receipt().exists());

    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions()[0].selector, "receipt");
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
    assert!(
        check_install(&f.targets, Some(&f.release()), "fixture-target", &LocalFs)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancelled_apply_reports_completed_and_remaining_actions() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let fault_fs = FaultFs::new(FaultMode::CancelAfterFirstPublication, Some(cancel.clone()));
    let error = install(
        &plan,
        &fault_context(&fault_fs, &clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    let InstallError::Partial {
        completed,
        remaining,
        source,
    } = error
    else {
        panic!("expected partial install")
    };
    assert_eq!(completed, vec![plan.actions()[0].clone()]);
    assert_eq!(remaining, plan.actions()[1..]);
    assert!(matches!(
        *source,
        InstallError::Fs(TestFsError::Cancelled { .. })
    ));
    assert!(!f.receipt().exists());

    let recovery = f.plan();
    let retry_cancel = TestCancelToken::new();
    install(
        &recovery,
        &context(&clock, &retry_cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
}

#[test]
fn visible_skill_link_with_failed_directory_sync_is_reconciled_on_retry() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let fault_fs = FaultFs::new(FaultMode::SkillLinkSync, None);
    let error = install(
        &plan,
        &fault_context(&fault_fs, &clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    let InstallError::Partial {
        completed,
        remaining,
        source,
    } = error
    else {
        panic!("expected partial install")
    };
    assert!(completed.iter().any(|action| action.selector == "binary"));
    assert_eq!(remaining.first().unwrap().selector, "skill");
    assert_eq!(remaining.last().unwrap().selector, "receipt");
    assert!(matches!(
        *source,
        InstallError::Fs(TestFsError::Io {
            operation: "sync",
            ..
        })
    ));
    assert!(
        fs::symlink_metadata(f.link())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!f.receipt().exists());

    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions()[0].selector, "receipt");
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(f.plan().actions().is_empty());
}

#[test]
fn adopt_existing_binary_does_not_adopt_a_symlink() {
    let f = Fixture::new();
    fs::create_dir_all(&f.targets.bin_dir).unwrap();
    let binary = f.targets.bin_dir.join("provenant");
    symlink("/foreign/provenant", &binary).unwrap();
    let mut targets = f.targets.clone();
    targets.adopt_existing = true;
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &targets, &LocalFs).unwrap();
    let error = plan_install(release, targets, snapshots).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("unmanaged destination conflicts: {}", binary.display())
    );
    assert_eq!(
        fs::read_link(&binary).unwrap(),
        PathBuf::from("/foreign/provenant")
    );
    assert!(!f.receipt().exists());
}

#[test]
fn check_install_compares_the_requested_source_digest() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let other_digest = source_digest(&[SourceInput {
        name: "src/lib.rs",
        bytes: b"different fixture",
    }])
    .unwrap();
    let other = Fixture::new();
    let other_source = crate::package_release(
        &other.source,
        other_digest,
        "fixture-target",
        "fixture-toolchain",
        &LocalFs,
    )
    .unwrap();
    let findings =
        check_install(&f.targets, Some(&other_source), "fixture-target", &LocalFs).unwrap();
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.rule == "source_digest")
            .count(),
        1
    );
    assert_eq!(
        findings
            .iter()
            .find(|finding| finding.rule == "source_digest")
            .unwrap()
            .path
            .to_path_buf()
            .unwrap(),
        f.receipt()
    );
}

#[test]
fn uninstall_reconciles_entries_removed_before_receipt_publication() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    fs::remove_file(f.link()).unwrap();
    let result = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert_eq!(result.actions.len(), 1);
    assert_eq!(result.actions[0].selector, "receipt");
    assert!(
        read_receipt(&f.targets, &LocalFs)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .all(|e| e.harness != Some(Harness::Codex))
    );
    assert!(
        check_install(&f.targets, None, "fixture-target", &LocalFs)
            .unwrap()
            .is_empty()
    );
    // Once removal is reconciled, a later recreated matching link is unmanaged.
    let release = f.release();
    let path = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string())
        .join("assets/skills/example");
    symlink(&path, f.link()).unwrap();
    assert!(
        uninstall(
            &f.targets,
            &context(&clock, &cancel),
            &mut Reporter::default()
        )
        .unwrap()
        .actions
        .is_empty()
    );
    assert_eq!(fs::read_link(f.link()).unwrap(), path);
}

#[test]
fn regular_user_instructions_are_preserved_with_manual_include_advice() {
    let f = Fixture::new();
    let assets = f.source.join("assets/instructions");
    fs::create_dir_all(&assets).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let p = assets.join(name);
        fs::write(&p, b"release instructions").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let instruction = f.home.join(".codex/AGENTS.md");
    fs::create_dir_all(instruction.parent().unwrap()).unwrap();
    fs::write(&instruction, b"my instructions").unwrap();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let result = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert_eq!(fs::read(&instruction).unwrap(), b"my instructions");
    assert_eq!(result.manual_includes.len(), 1);
    assert_eq!(
        result.manual_includes[0].path.to_path_buf().unwrap(),
        instruction
    );
    assert!(
        !read_receipt(&f.targets, &LocalFs)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .any(|e| e.path.to_path_buf().unwrap() == instruction)
    );
}

#[test]
fn completed_immutable_release_missing_a_file_is_not_repaired() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let missing = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string())
        .join("provenant");
    fs::remove_file(&missing).unwrap();
    assert_eq!(
        Snapshots::capture(&f.release(), &f.targets, &LocalFs)
            .unwrap_err()
            .to_string(),
        format!("release file bytes or mode differ: {}", missing.display())
    );
    assert!(!missing.exists());
}
#[test]
fn receipt_cannot_omit_binary_ownership_or_bless_a_foreign_binary_digest() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let mut receipt = read_receipt(&f.targets, &LocalFs).unwrap().unwrap();
    let original = receipt.clone();
    receipt.entries.clear();
    fs::write(f.receipt(), provenant_records::encode(&receipt).unwrap()).unwrap();
    assert_eq!(
        check_install(&f.targets, None, "fixture-target", &LocalFs)
            .unwrap_err()
            .to_string(),
        "invalid release manifest: receipt requires one binary entry"
    );
    let mut receipt = original;
    let binary = f.targets.bin_dir.join("provenant");
    fs::write(&binary, b"foreign").unwrap();
    for entry in &mut receipt.entries {
        if entry.selector == "binary" {
            entry.expected = OwnedValue::File {
                digest: CanonicalV1::file_digest(b"foreign"),
                mode: 0o755,
            };
        }
    }
    fs::write(f.receipt(), provenant_records::encode(&receipt).unwrap()).unwrap();
    assert_eq!(
        check_install(&f.targets, None, "fixture-target", &LocalFs)
            .unwrap_err()
            .to_string(),
        "invalid release manifest: receipt binary differs from release"
    );
}
