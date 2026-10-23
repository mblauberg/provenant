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
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("skills/example")).unwrap();
        for (path, bytes, mode) in [
            ("bin/provenant", b"fixture executable".as_slice(), 0o755),
            (
                "skills/example/SKILL.md",
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
        package_fixture_release(&self.source, digest)
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
fn package_fixture_release(
    source: &Path,
    source_digest: provenant_records::SourceDigest,
) -> Release {
    let mut files = BTreeMap::new();
    for relative in crate::release::inventory(source, &LocalFs).unwrap() {
        let path = source.join(&relative);
        let bytes = fs::read(&path).unwrap();
        let actual_mode = fs::metadata(&path).unwrap().permissions().mode();
        let mode = if relative == Path::new("bin/provenant") || actual_mode & 0o111 != 0 {
            0o700
        } else {
            0o600
        };
        files.insert(relative, (bytes, mode));
    }
    let expected = VersionInfo {
        source_digest,
        asset_digest: crate::release::asset_inventory_digest(&files).unwrap(),
        target: "fixture-target".into(),
        toolchain: "fixture-toolchain".into(),
        record_schema_version: 1,
        config_schema_version: 1,
    };
    crate::package_release(source, &expected, &LocalFs, &Smoke, &CancelToken::new()).unwrap()
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
    fn check(&self, release: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        Ok(VersionInfo {
            source_digest: release.manifest.source_digest,
            asset_digest: release.manifest.asset_digest,
            target: release.manifest.target.clone(),
            toolchain: release.manifest.toolchain.clone(),
            record_schema_version: 1,
            config_schema_version: 1,
        })
    }
}
fn check_install_for(
    targets: &Targets,
    source: Option<&Release>,
    target: &str,
    fs: &dyn SafeFs,
) -> Result<Vec<InstallFinding>, InstallError> {
    let cancel = CancelToken::new();
    check_install(targets, source, target, fs, &Smoke, &cancel)
}
#[derive(Clone, Copy)]
enum IdentityField {
    SourceDigest,
    AssetDigest,
    Target,
    Toolchain,
}
struct MismatchedSmoke {
    digest: provenant_records::SourceDigest,
    field: IdentityField,
}
impl ReleaseSmoke for MismatchedSmoke {
    fn check(&self, release: &Release, cancel: &CancelToken) -> io::Result<VersionInfo> {
        let mut version = Smoke.check(release, cancel)?;
        if release.manifest.source_digest == self.digest {
            match self.field {
                IdentityField::SourceDigest => {
                    version.source_digest = source_digest(&[SourceInput {
                        name: "src/other.rs",
                        bytes: b"other",
                    }])
                    .unwrap();
                }
                IdentityField::AssetDigest => {
                    version.asset_digest = source_digest(&[SourceInput {
                        name: "assets/other",
                        bytes: b"other",
                    }])
                    .unwrap();
                }
                IdentityField::Target => version.target = "wrong-target".into(),
                IdentityField::Toolchain => version.toolchain = "wrong-toolchain".into(),
            }
        }
        Ok(version)
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
        check_install_for(&f.targets, Some(&f.release()), "fixture-target", &LocalFs)
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
    fs::write(f.source.join("bin/provenant"), b"tampered").unwrap();
    assert_eq!(
        Release::read(&f.source, "fixture-target", &LocalFs)
            .unwrap_err()
            .to_string(),
        format!(
            "release file bytes or mode differ: {}",
            f.source.join("bin/provenant").display()
        )
    );
    let f = Fixture::new();
    f.release();
    fs::set_permissions(
        f.source.join("bin/provenant"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        matches!(Release::read(&f.source, "fixture-target", &LocalFs), Err(InstallError::Tampered(p)) if p == f.source.join("bin/provenant"))
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
        .join("bin/provenant");
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
        check_install_for(&f.targets, None, "fixture-target", &LocalFs)
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
    let outcome = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(
        outcome
            .actions
            .iter()
            .all(|action| { action.disposition != Disposition::Remove })
    );
    assert!(
        outcome
            .actions
            .iter()
            .any(|action| action.selector == "receipt")
    );
    assert_eq!(
        fs::read_link(f.link()).unwrap(),
        PathBuf::from("/foreign/skill")
    );
    assert_eq!(fs::read(&sibling).unwrap(), b"personal");
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
    fs::remove_file(f.source.join("bin/provenant")).unwrap();
    symlink("/never/open", f.source.join("bin/provenant")).unwrap();
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
    InterruptedReleasePublish,
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
    fn link_changed(&self, name: &OsStr, result: TestFsResult<()>) -> TestFsResult<()> {
        if result.is_ok()
            && self.state.mode == FaultMode::SkillLinkSync
            && self
                .inner
                .read_link(name)
                .is_ok_and(|target| target.components().any(|part| part.as_os_str() == "skills"))
        {
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
        self.link_changed(destination, self.inner.replace(source, destination))
    }
    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> TestFsResult<()> {
        self.link_changed(
            destination,
            self.inner.rename_noreplace(source, destination),
        )
    }
    fn symlink(&self, target: &Path, name: &OsStr) -> TestFsResult<()> {
        self.inner.symlink(target, name)
    }
    fn read_link(&self, name: &OsStr) -> TestFsResult<PathBuf> {
        self.inner.read_link(name)
    }
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        permissions: u32,
        mode: PublishMode,
    ) -> TestFsResult<Publication> {
        if self.state.mode == FaultMode::InterruptedReleasePublish
            && name == OsStr::new("release.json")
            && !self.state.fired.swap(true, Ordering::AcqRel)
        {
            // Leave the actual supervisor staging name and bytes behind, as a
            // process interrupted between staging and rename would do.
            let temporary = LocalFs.temporary_name()?;
            let mut file = self.inner.create_new(&temporary)?;
            std::io::Write::write_all(&mut file, bytes).unwrap();
            file.sync()?;
            return Err(TestFsError::Io {
                operation: "publish",
                path: PathBuf::from(name),
                source: io::Error::other("interrupted release publication"),
            });
        }
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
        let publication = self
            .inner
            .atomic_write(name, prefix, bytes, permissions, mode)?;
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
        check_install_for(&f.targets, Some(&f.release()), "fixture-target", &LocalFs)
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
fn check_install_smokes_installed_and_requested_release_identity() {
    let f = Fixture::new();
    let installed = f.release();
    let installed_digest = installed.manifest.source_digest;
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

    for field in [
        IdentityField::SourceDigest,
        IdentityField::AssetDigest,
        IdentityField::Target,
        IdentityField::Toolchain,
    ] {
        let smoke = MismatchedSmoke {
            digest: installed_digest,
            field,
        };
        let error = check_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &smoke,
            &cancel,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid release manifest: binary identity differs from release manifest"
        );
    }

    let other = Fixture::new();
    let other_digest = source_digest(&[SourceInput {
        name: "src/lib.rs",
        bytes: b"different fixture",
    }])
    .unwrap();
    let other_source = package_fixture_release(&other.source, other_digest);
    for field in [
        IdentityField::SourceDigest,
        IdentityField::AssetDigest,
        IdentityField::Target,
        IdentityField::Toolchain,
    ] {
        let smoke = MismatchedSmoke {
            digest: other_source.manifest.source_digest,
            field,
        };
        let error = check_install(
            &f.targets,
            Some(&other_source),
            "fixture-target",
            &LocalFs,
            &smoke,
            &cancel,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid release manifest: binary identity differs from release manifest"
        );
    }
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
    let other_source = package_fixture_release(&other.source, other_digest);
    let findings =
        check_install_for(&f.targets, Some(&other_source), "fixture-target", &LocalFs).unwrap();
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
        check_install_for(&f.targets, None, "fixture-target", &LocalFs)
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
        .join("skills/example");
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
    let assets = f.source.join("instructions");
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
        .join("bin/provenant");
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
        check_install_for(&f.targets, None, "fixture-target", &LocalFs)
            .unwrap_err()
            .to_string(),
        "invalid release manifest: receipt requires one binary entry"
    );
    let mut receipt = original;
    for entry in &mut receipt.entries {
        if entry.selector == "binary" {
            entry.expected = OwnedValue::Link {
                target: AbsolutePath::new(Path::new("/foreign/provenant")).unwrap(),
            };
        }
    }
    fs::write(f.receipt(), provenant_records::encode(&receipt).unwrap()).unwrap();
    assert_eq!(
        check_install_for(&f.targets, None, "fixture-target", &LocalFs)
            .unwrap_err()
            .to_string(),
        "invalid release manifest: receipt entry outside resolved targets"
    );
}

impl Fixture {
    fn release_variant(&self, name: &str, skills: &[&str]) -> Release {
        let source = self._tmp.path().join(format!("staged-{name}"));
        for skill in skills {
            fs::create_dir_all(source.join("skills").join(skill)).unwrap();
        }
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(
            source.join("bin/provenant"),
            format!("fixture executable {name}"),
        )
        .unwrap();
        fs::set_permissions(
            source.join("bin/provenant"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        for skill in skills {
            let path = source.join("skills").join(skill).join("SKILL.md");
            fs::write(&path, format!("---\nname: {skill}\n---\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let digest = source_digest(&[SourceInput {
            name: "src/lib.rs",
            bytes: name.as_bytes(),
        }])
        .unwrap();
        package_fixture_release(&source, digest)
    }

    fn plan_for(&self, release: Release) -> InstallPlan {
        let snapshots = Snapshots::capture(&release, &self.targets, &LocalFs).unwrap();
        plan_install(release, self.targets.clone(), snapshots).unwrap()
    }

    fn skill_link(&self, name: &str) -> PathBuf {
        self.home.join(".codex/skills").join(name)
    }
}

#[test]
fn upgrade_removes_only_unchanged_dropped_links_and_recovers_after_first_replacement() {
    let f = Fixture::new();
    let release_a = f.release_variant("a", &["common", "obsolete", "edited"]);
    let release_b = f.release_variant("b", &["common", "added"]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let plan_a = f.plan_for(release_a.clone());
    install(
        &plan_a,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    fs::remove_file(f.skill_link("edited")).unwrap();
    symlink("/foreign/edited", f.skill_link("edited")).unwrap();
    let unmanaged = f.skill_link("personal");
    fs::write(&unmanaged, b"personal").unwrap();

    let plan_b = f.plan_for(release_b.clone());
    let obsolete = AbsolutePath::new(&f.skill_link("obsolete")).unwrap();
    assert!(
        plan_b
            .actions()
            .iter()
            .any(|action| { action.path == obsolete && action.disposition == Disposition::Remove })
    );
    assert!(!plan_b.actions().iter().any(|action| {
        action.path == AbsolutePath::new(&f.skill_link("edited")).unwrap()
            && action.disposition == Disposition::Remove
    }));

    // Publish through the same atomic boundary until the first harness link is
    // replaced, then leave the old receipt behind as a crashed process would.
    let first_replacement = plan_b
        .actions
        .iter()
        .position(|action| {
            action.selector == "skill"
                && action.path.to_path_buf().unwrap() == f.skill_link("common")
        })
        .unwrap();
    for action in &plan_b.actions[..=first_replacement] {
        let path = action.path.to_path_buf().unwrap();
        if action.disposition == Disposition::Remove {
            break;
        }
        let desired = &plan_b.desired.get(&path).unwrap().value;
        let replace = plan_b.before.entries.get(&path).unwrap().is_some();
        publish(&path, desired, replace, &LocalFs).unwrap();
    }
    let receipt_after_crash: InstallReceipt =
        provenant_records::decode_v1(fs::read(f.receipt()).unwrap().as_slice(), Limits::CONFIG)
            .unwrap();
    assert_eq!(
        receipt_after_crash.source_digest,
        release_a.manifest.source_digest
    );

    let recovery = f.plan_for(release_b.clone());
    install(
        &recovery,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(fs::symlink_metadata(f.skill_link("obsolete")).is_err());
    assert_eq!(
        fs::read_link(f.skill_link("edited")).unwrap(),
        PathBuf::from("/foreign/edited")
    );
    assert_eq!(fs::read(&unmanaged).unwrap(), b"personal");
    let receipt: InstallReceipt =
        provenant_records::decode_v1(fs::read(f.receipt()).unwrap().as_slice(), Limits::CONFIG)
            .unwrap();
    assert_eq!(receipt.source_digest, release_b.manifest.source_digest);
    assert!(receipt.entries.iter().all(|entry| entry.path != obsolete));
    assert!(f.plan_for(release_b).actions().is_empty());
}

#[test]
fn bin_dir_upgrade_removes_only_the_unchanged_previous_shim() {
    for changed in [false, true] {
        let mut f = Fixture::new();
        let release_a = f.release_variant("a", &["common"]);
        let release_b = f.release_variant("b", &["common"]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        install(
            &f.plan_for(release_a),
            &context(&clock, &cancel),
            &mut Reporter::default(),
            &Smoke,
            false,
        )
        .unwrap();
        let old_binary = f.targets.bin_dir.join("provenant");
        if changed {
            fs::remove_file(&old_binary).unwrap();
            symlink("/personal/provenant", &old_binary).unwrap();
        }
        f.targets.bin_dir = f.home.join("new-bin");
        let plan = f.plan_for(release_b.clone());
        assert_eq!(
            plan.actions().iter().any(|action| {
                action.path == AbsolutePath::new(&old_binary).unwrap()
                    && action.disposition == Disposition::Remove
            }),
            !changed
        );
        install(
            &plan,
            &context(&clock, &cancel),
            &mut Reporter::default(),
            &Smoke,
            false,
        )
        .unwrap();
        if changed {
            assert_eq!(
                fs::read_link(&old_binary).unwrap(),
                PathBuf::from("/personal/provenant")
            );
        } else {
            assert!(fs::symlink_metadata(&old_binary).is_err());
        }
        assert!(f.targets.bin_dir.join("provenant").is_file());
        let receipt = read_receipt(&f.targets, &LocalFs).unwrap().unwrap();
        assert_eq!(receipt.bin_dir.to_path_buf().unwrap(), f.targets.bin_dir);
        assert!(
            receipt
                .entries
                .iter()
                .all(|entry| entry.path != AbsolutePath::new(&old_binary).unwrap())
        );
        assert!(f.plan_for(release_b).actions().is_empty());
    }
}

#[test]
fn receipt_targets_cover_two_harnesses_and_uninstall_uses_recorded_paths() {
    let mut f = Fixture::new();
    let custom = f.home.join("custom");
    let codex_skills = custom.join("codex/skills");
    let codex_instruction = custom.join("codex/AGENTS.md");
    let claude_skills = custom.join("claude/skills");
    let claude_instruction = custom.join("claude/CLAUDE.md");
    f.targets.bin_dir = custom.join("bin");
    f.targets.harnesses[0].skills_dir = codex_skills.clone();
    f.targets.harnesses[0].instruction = Some(codex_instruction.clone());
    f.targets.harnesses.push(HarnessTarget {
        harness: Harness::Claude,
        skills_dir: claude_skills.clone(),
        instruction: Some(claude_instruction.clone()),
    });
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let path = f.source.join("instructions").join(name);
        fs::write(path, b"release instruction").unwrap();
        fs::set_permissions(
            f.source.join("instructions").join(name),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    let release = f.release();
    let plan = f.plan_for(release);
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

    let receipt = read_receipt(&f.targets, &LocalFs).unwrap().unwrap();
    assert_eq!(receipt.bin_dir.to_path_buf().unwrap(), f.targets.bin_dir);
    assert_eq!(receipt.harnesses, vec![Harness::Claude, Harness::Codex]);
    assert_eq!(
        receipt
            .harness_targets
            .iter()
            .find(|target| target.harness == Harness::Claude)
            .unwrap()
            .skills_dir
            .to_path_buf()
            .unwrap(),
        claude_skills
    );

    // Current CLI path resolution can differ from install time. Check and
    // uninstall must use the recorded locations from the receipt.
    let mut changed = f.targets.clone();
    changed.bin_dir = custom.join("changed-bin");
    for target in &mut changed.harnesses {
        target.skills_dir = custom
            .join("changed")
            .join(format!("{:?}/skills", target.harness));
        target.instruction = None;
    }
    assert!(
        check_install_for(&changed, None, "fixture-target", &LocalFs)
            .unwrap()
            .is_empty()
    );

    changed.harnesses = vec![HarnessTarget {
        harness: Harness::Claude,
        skills_dir: custom.join("changed/claude/skills"),
        instruction: None,
    }];
    uninstall(
        &changed,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(fs::symlink_metadata(claude_skills.join("example")).is_err());
    assert!(fs::symlink_metadata(codex_skills.join("example")).is_ok());
    assert!(fs::symlink_metadata(claude_instruction).is_err());
    assert!(fs::symlink_metadata(codex_instruction).is_ok());
    let shrunk = read_receipt(&changed, &LocalFs).unwrap().unwrap();
    assert_eq!(shrunk.harnesses, vec![Harness::Codex]);
    assert_eq!(shrunk.harness_targets.len(), 1);
    assert_eq!(shrunk.harness_targets[0].harness, Harness::Codex);
    assert!(
        check_install_for(&changed, None, "fixture-target", &LocalFs)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn receipt_targets_drive_check_and_uninstall_and_uninstall_shrinks_harnesses() {
    let f = Fixture::new();
    let release = f.release();
    let plan = f.plan_for(release.clone());
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

    let receipt = read_receipt(&f.targets, &LocalFs).unwrap().unwrap();
    assert_eq!(receipt.bin_dir.to_path_buf().unwrap(), f.targets.bin_dir);
    assert_eq!(receipt.harnesses, vec![Harness::Codex]);
    assert_eq!(
        receipt.harness_targets[0].skills_dir.to_path_buf().unwrap(),
        f.home.join(".codex/skills")
    );
    assert_eq!(
        receipt.harness_targets[0]
            .instruction
            .as_ref()
            .unwrap()
            .to_path_buf()
            .unwrap(),
        f.home.join(".codex/AGENTS.md")
    );
    let mut changed_targets = f.targets.clone();
    changed_targets.bin_dir = f.home.join("elsewhere/bin");
    changed_targets.harnesses[0].skills_dir = f.home.join("elsewhere/skills");
    changed_targets.harnesses[0].instruction = Some(f.home.join("elsewhere/AGENTS.md"));
    assert!(
        check_install_for(&changed_targets, Some(&release), "fixture-target", &LocalFs,)
            .unwrap()
            .is_empty()
    );
    let outcome = uninstall(
        &changed_targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(outcome.actions.iter().any(|action| {
        action.disposition == Disposition::Remove
            && action.path == AbsolutePath::new(&f.link()).unwrap()
    }));
    assert!(!f.link().exists());
    let receipt = read_receipt(&changed_targets, &LocalFs).unwrap().unwrap();
    assert!(receipt.harnesses.is_empty());
    assert!(receipt.harness_targets.is_empty());
    assert_eq!(
        receipt
            .entries
            .iter()
            .filter(|entry| entry.selector == "binary")
            .count(),
        1
    );
}

#[test]
fn a_completed_release_with_a_different_manifest_is_a_typed_conflict() {
    let f = Fixture::new();
    let first = f.release();
    let first_digest = CanonicalV1::file_digest(&first.manifest_bytes);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan_for(first.clone()),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();

    let mut second = f.release();
    second.manifest.toolchain = "different-toolchain".into();
    second.manifest_bytes = provenant_records::encode(&second.manifest).unwrap();
    let second_digest = CanonicalV1::file_digest(&second.manifest_bytes);
    let error = Snapshots::capture(&second, &f.targets, &LocalFs).unwrap_err();
    assert!(matches!(
        error,
        InstallError::ReleaseConflict {
            existing_manifest_digest,
            requested_manifest_digest,
            ..
        } if existing_manifest_digest == first_digest
            && requested_manifest_digest == second_digest
    ));
    assert!(error.to_string().contains(&first_digest.to_string()));
    assert!(error.to_string().contains(&second_digest.to_string()));
    assert!(error.to_string().contains("select a new source digest"));
}

#[test]
fn interrupted_release_publish_leaves_no_manifest_and_reapplies_with_real_staging_file() {
    let f = Fixture::new();
    let release = f.release();
    let release_root = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let binary = release_root.join("bin/provenant");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, b"interrupted old bytes").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let faults = FaultFs::new(FaultMode::InterruptedReleasePublish, None);
    let error = install(
        &f.plan_for(release.clone()),
        &fault_context(&faults, &clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(matches!(error, InstallError::Partial { .. }));
    assert!(!release_root.join("release.json").exists());
    // Every payload was published before the failed completion marker.
    for (path, (bytes, _)) in &release.files {
        assert_eq!(fs::read(release_root.join(path)).unwrap(), *bytes);
    }
    let staging = fs::read_dir(&release_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".provenant-staging.")
        })
        .unwrap();
    assert!(staging.is_file());

    install(
        &f.plan_for(release.clone()),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(!staging.exists());
    Release::read(&release_root, &release.manifest.target, &LocalFs).unwrap();
}

#[test]
fn capture_does_not_unlink_an_active_install_staging_file() {
    let f = Fixture::new();
    let release = f.release();
    let root = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let install_dir = directory(
        &f.targets.roots.state.as_path().join("install"),
        true,
        &LocalFs,
    )
    .unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let _lock = install_dir
        .lock(
            OsStr::new("install.lock"),
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &cancel,
            &clock,
        )
        .unwrap();
    let binary_dir = directory(&root.join("bin"), true, &LocalFs).unwrap();
    let temporary = LocalFs.temporary_name().unwrap();
    let mut file = binary_dir.create_new(&temporary).unwrap();
    std::io::Write::write_all(&mut file, b"live staged bytes").unwrap();
    file.sync().unwrap();

    // A second caller can preview while the first writer owns the lock. Its
    // inventory must leave that writer's open staging entry intact.
    Snapshots::capture(&release, &f.targets, &LocalFs).unwrap();
    binary_dir
        .rename_noreplace(&temporary, OsStr::new("provenant"))
        .unwrap();
    assert_eq!(
        fs::read(root.join("bin/provenant")).unwrap(),
        b"live staged bytes"
    );
}

#[test]
fn completed_release_tolerates_only_supervisor_staging_names() {
    let f = Fixture::new();
    let release = f.release();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan_for(release.clone()),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let root = f
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let staging = root.join(LocalFs.temporary_name().unwrap());
    fs::write(&staging, b"leftover staging").unwrap();
    f.plan_for(release.clone());
    Release::read(&root, &release.manifest.target, &LocalFs).unwrap();
    assert!(staging.exists());
    let foreign = root.join(".provenant-staging.not-supervisor");
    fs::write(&foreign, b"foreign").unwrap();
    assert!(matches!(Snapshots::capture(&release, &f.targets, &LocalFs),
        Err(InstallError::Tampered(path)) if path == foreign));
}

struct WrongSmoke;
impl ReleaseSmoke for WrongSmoke {
    fn check(&self, release: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        let mut info = Smoke.check(release, &CancelToken::new())?;
        info.target = "wrong-target".into();
        Ok(info)
    }
}

#[test]
fn install_refuses_a_binary_whose_version_identity_differs_from_manifest() {
    let f = Fixture::new();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &WrongSmoke,
        false,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid release manifest: binary identity differs from release manifest"
    );
    assert!(!f.targets.roots.state.as_path().exists());
    assert!(!f.targets.bin_dir.exists());
}
