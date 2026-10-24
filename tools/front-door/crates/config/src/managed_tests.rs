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
        inspect_install(
            &f.targets,
            Some(&f.release()),
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
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
        publish(
            &path,
            desired,
            false,
            &LocalFs,
            f.targets.roots.state.as_path(),
        )
        .unwrap();
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
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
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
    assert!(
        outcome
            .findings
            .iter()
            .any(|finding| finding.rule == "relinquished_modified_entry"
                && finding.path.to_path_buf().unwrap() == f.link())
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
    StateMount,
    SourceMount,
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
    fn into_anchor(self: Box<Self>) -> TestFsResult<Box<dyn SafeDir>> {
        Ok(Box::new(Self {
            inner: self.inner.into_anchor()?,
            state: self.state,
        }))
    }
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
        if (self.state.mode == FaultMode::StateMount && name == "install")
            || (self.state.mode == FaultMode::SourceMount && name == "skills")
        {
            return Err(FsError::CrossDevice { path: name.into() });
        }
        Ok(self.child(self.inner.open_verified_directory(name)?))
    }
    fn open_read(&self, name: &OsStr) -> TestFsResult<Box<dyn SafeFile>> {
        assert!(
            !(self.state.mode == FaultMode::SourceMount && name == "SKILL.md"),
            "asset read before source confinement"
        );
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
        inspect_install(
            &f.targets,
            Some(&f.release()),
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
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
        let error = inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &smoke,
            &cancel,
        )
        .unwrap()
        .error
        .unwrap();
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
        let error = inspect_install(
            &f.targets,
            Some(&other_source),
            "fixture-target",
            &LocalFs,
            &smoke,
            &cancel,
        )
        .unwrap()
        .error
        .unwrap();
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
    let findings = inspect_install(
        &f.targets,
        Some(&other_source),
        "fixture-target",
        &LocalFs,
        &Smoke,
        &CancelToken::new(),
    )
    .unwrap()
    .findings;
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
    assert_eq!(
        result
            .actions
            .iter()
            .map(|a| a.selector.as_str())
            .collect::<Vec<_>>(),
        ["config", "receipt"]
    );
    assert!(
        read_receipt(&f.targets, &LocalFs)
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .all(|e| e.harness != Some(Harness::Codex))
    );
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
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
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .error
        .unwrap()
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
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .error
        .unwrap()
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
        publish(
            &path,
            desired,
            replace,
            &LocalFs,
            f.targets.roots.state.as_path(),
        )
        .unwrap();
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
        mcp_file: f.home.join(".claude.json"),
        hook_file: Some(f.home.join(".claude/settings.json")),
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
        inspect_install(
            &changed,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );

    changed.harnesses = vec![HarnessTarget {
        harness: Harness::Claude,
        mcp_file: f.home.join(".claude.json"),
        hook_file: Some(f.home.join(".claude/settings.json")),
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
        inspect_install(
            &changed,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
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
        inspect_install(
            &changed_targets,
            Some(&release),
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
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

#[test]
fn harness_cutover_previews_mcp_and_dry_run_creates_nothing() {
    let f = Fixture::new();
    let plan = f.plan();
    assert!(plan.actions().iter().any(|a| a.selector == "config"));
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        true,
    )
    .unwrap();
    assert!(!f.home.join(".codex").exists());
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn mcp_selector_ownership_preserves_user_edits_and_uninstalls_only_unchanged() {
    let f = Fixture::new();
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let foreign =
        "# user comment\nmodel = 'user-model'\n[mcp_servers.other]\ncommand = 'foreign' # keep\n";
    fs::write(&config, foreign).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(&f.plan(), &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.starts_with("# user comment\nmodel = 'user-model'\n"));
    assert!(text.contains("[mcp_servers.other]\ncommand = 'foreign' # keep\n"));
    assert!(text.contains("--state-dir"));
    let edited = text.replace("user-model", "changed-model");
    fs::write(&config, &edited).unwrap();
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
    assert!(f.plan().actions().is_empty());
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    let after = fs::read_to_string(&config).unwrap();
    assert!(after.starts_with("# user comment\nmodel = 'changed-model'\n"));
    assert!(after.contains("[mcp_servers.other]\ncommand = 'foreign' # keep\n"));
    assert!(!after.contains("mcp_servers.fabric"));
    assert!(f.targets.bin_dir.join("provenant").is_symlink());
}

#[test]
fn hooks_on_and_symlinked_configs_refuse_before_any_write() {
    let mut f = Fixture::new();
    f.targets.hooks = true;
    let release = f.release();
    assert_eq!(
        Snapshots::capture(&release, &f.targets, &LocalFs)
            .unwrap_err()
            .to_string(),
        "hook registration refused: no fixture-backed HookSpec is available"
    );
    assert!(!f.targets.roots.state.as_path().exists());
    f.targets.hooks = false;
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let regular = f.home.join("foreign.toml");
    fs::write(&regular, "model='keep'\n").unwrap();
    symlink(&regular, &config).unwrap();
    let err = Snapshots::capture(&release, &f.targets, &LocalFs).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "symlinked or non-regular harness config refused: {}",
            config.display()
        )
    );
    assert_eq!(fs::read_to_string(&regular).unwrap(), "model='keep'\n");
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn mcp_conflict_does_not_adopt_foreign_env_or_create_state() {
    let mut f = Fixture::new();
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let foreign =
        "[mcp_servers.fabric]\ncommand='foreign'\n[mcp_servers.fabric.env]\nTOKEN='do-not-copy'\n";
    fs::write(&config, foreign).unwrap();
    f.targets.adopt_existing = true;
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &f.targets, &LocalFs);
    assert_eq!(
        snapshots.unwrap_err().to_string(),
        format!("unmanaged destination conflicts: {}", config.display())
    );
    assert_eq!(fs::read_to_string(config).unwrap(), foreign);
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn five_client_cutover_is_exact_preserves_herdr_comments_and_reapplies() {
    let mut f = Fixture::new();
    let legacy = f.home.join("old-checkout");
    f.targets = Targets::new(
        f.targets.roots.clone(),
        &f.home,
        &[
            Harness::Claude,
            Harness::Codex,
            Harness::Cursor,
            Harness::Agy,
            Harness::Opencode,
        ],
    );
    f.targets.legacy_checkout = Some(legacy.clone());
    for target in &f.targets.harnesses {
        fs::create_dir_all(&target.skills_dir).unwrap();
        symlink(
            legacy.join("skills/autopilot-near"),
            target.skills_dir.join("autopilot"),
        )
        .unwrap();
        symlink(
            legacy.join("skills/_shared"),
            target.skills_dir.join("_shared"),
        )
        .unwrap();
        if let Some(instruction) = &target.instruction {
            let source = if matches!(target.harness, Harness::Claude | Harness::Codex) {
                f.home.join(".agents/AGENTS.md")
            } else {
                legacy.join("AGENTS.md")
            };
            symlink(source, instruction).unwrap();
        }
    }
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        let p = f.source.join("instructions").join(name);
        fs::write(&p, b"PROVENANT_AGENT_LABEL instructions\n").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::create_dir_all(f.home.join(".agents")).unwrap();
    fs::write(
        f.home.join(".agents/AGENTS.md"),
        b"Set AGENT_FABRIC_LABEL for messages\n",
    )
    .unwrap();
    let hook = f.home.join(".claude/settings.json");
    let old_hook = format!(
        "python3 \"{}/skills/session/scripts/state_check.py\" --hook",
        legacy.display()
    );
    let owned =
        serde_json::to_string(&serde_json::json!({"type":"command","command":old_hook})).unwrap();
    let herdr = r#"{"type":"command", "command":"herdr capture", "timeout":2}"#;
    let before = format!(
        "{{\n // herdr stays\n \"user\": 7,\n \"hooks\": {{\"PreCompact\":[{{\"hooks\":[{owned}, {herdr}]}}]}}\n}}\n"
    );
    fs::write(&hook, &before).unwrap();
    let codex_hooks = f.home.join(".codex/hooks.json");
    let codex_foreign = b"{\n // Herdr\n \"hooks\":{\"Stop\":[{\"command\":\"herdr\"}]}\n}\n";
    fs::write(&codex_hooks, codex_foreign).unwrap();
    let opencode = f.home.join(".config/opencode/opencode.jsonc");
    fs::write(&opencode,format!("{{// foreign comment\n\"instructions\":[\"foreign.md\",{},],\n\"theme\": \"user\",}}\n",serde_json::to_string(&f.home.join(".agents/AGENTS.md").to_str().unwrap()).unwrap())).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    let plan = f.plan();
    assert!(plan.actions().iter().any(|a| !a.edits.is_empty()));
    install(&plan, &ctx, &mut Reporter::default(), &Smoke, false).unwrap();
    for target in &f.targets.harnesses {
        assert!(fs::symlink_metadata(target.skills_dir.join("_shared")).is_err());
        assert_eq!(
            fs::read_link(target.skills_dir.join("autopilot")).unwrap(),
            legacy.join("skills/autopilot-near")
        );
        assert!(target.mcp_file.is_file());
        if let Some(instruction) = &target.instruction {
            assert!(
                fs::read_link(instruction)
                    .unwrap()
                    .ends_with("instructions/AGENTS.md")
            );
        }
    }
    let after = fs::read_to_string(&hook).unwrap();
    assert!(after.contains(herdr));
    assert!(after.contains("// herdr stays"));
    assert_eq!(after, before);
    assert_eq!(fs::read(codex_hooks).unwrap(), codex_foreign);
    assert_eq!(
        fs::read_to_string(f.home.join(".agents/AGENTS.md")).unwrap(),
        "Set PROVENANT_AGENT_LABEL for messages\n"
    );
    assert!(
        fs::read_to_string(&opencode)
            .unwrap()
            .contains("// foreign comment")
    );
    assert!(f.plan().actions().is_empty());
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .iter()
        .any(
            |finding| finding.rule == "legacy_entry" && finding.path.to_path_buf().unwrap() == hook
        )
    );
    uninstall(&f.targets, &ctx, &mut Reporter::default()).unwrap();
    let after = fs::read_to_string(opencode).unwrap();
    assert!(after.contains("foreign.md"));
    assert!(after.contains("// foreign comment"));
    assert!(!after.contains("instructions/AGENTS.md"));
}

#[test]
fn whole_skills_link_is_unlinked_without_reading_its_target() {
    let mut f = Fixture::new();
    let legacy = f.home.join("unread-old-checkout");
    f.targets.legacy_checkout = Some(legacy.clone());
    fs::create_dir_all(f.home.join(".codex")).unwrap();
    symlink(legacy.join("skills"), f.home.join(".codex/skills")).unwrap();
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
    assert!(f.home.join(".codex/skills").is_dir());
    assert!(!f.home.join(".codex/skills").is_symlink());
    assert!(f.link().is_symlink());
    assert!(!legacy.exists());
    assert!(f.plan().actions().is_empty());
}

#[test]
fn legacy_checkout_destinations_and_duplicate_owned_instruction_values_refuse() {
    let mut f = Fixture::new();
    let legacy = f.home.join("old-checkout");
    f.targets.legacy_checkout = Some(legacy.clone());
    f.targets.bin_dir = legacy.join("bin");
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    fs::write(
        f.source.join("instructions/AGENTS.md"),
        "fixture instructions",
    )
    .unwrap();
    let release = f.release();
    let error = Snapshots::capture(&release, &f.targets, &LocalFs).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "unmanaged destination conflicts: {}",
            legacy.join("bin/provenant").display()
        )
    );
    assert!(!legacy.exists());
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Opencode]);
    f.targets.legacy_checkout = Some(legacy);
    let config = f.targets.harnesses[0].mcp_file.clone();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = f
        .home
        .join(".agents/AGENTS.md")
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = serde_json::to_vec(&serde_json::json!({"instructions":[old,old]})).unwrap();
    fs::write(&config, &bytes).unwrap();
    assert_eq!(
        Snapshots::capture(&release, &f.targets, &LocalFs)
            .unwrap_err()
            .to_string(),
        "invalid harness configuration: duplicate owned instruction"
    );
    assert_eq!(fs::read(config).unwrap(), bytes);
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn codex_inline_table_retains_foreign_values_and_comments() {
    let mut f = Fixture::new();
    f.targets.adopt_existing = true;
    let config = f.home.join(".codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let before = "# keep top\nmcp_servers = { other = { command = 'foreign' }, fabric = { command = 'old', env = {} } } # keep tail\nmodel = 'user'\n";
    fs::write(&config, before).unwrap();
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
    let after = fs::read_to_string(config).unwrap();
    assert!(after.starts_with("# keep top\n"));
    assert!(after.contains("other = { command = 'foreign' }"));
    assert!(after.contains("# keep tail\nmodel = 'user'\n"));
    assert!(f.plan().actions().is_empty());
}

#[test]
fn explicit_user_install_paths_win_over_harness_environment_even_when_default() {
    let mut f = Fixture::new();
    let state = f.targets.roots.state.as_path();
    fs::create_dir_all(state).unwrap();
    let config = format!(
        "schema_version=1\n[install.targets.codex]\nhome={}\nconfig={}\n",
        serde_json::to_string(f.home.join(".codex").to_str().unwrap()).unwrap(),
        serde_json::to_string(f.home.join(".codex/config.toml").to_str().unwrap()).unwrap()
    );
    fs::write(state.join("config.toml"), &config).unwrap();
    crate::apply_install_config_with_domains(&mut f.targets, &LocalFs, &crate::domains::NO_DOMAINS)
        .unwrap();
    f.targets
        .apply_environment(None, Some(&f.home.join("alternate")))
        .unwrap();
    assert_eq!(
        f.targets.harnesses[0].skills_dir,
        f.home.join(".codex/skills")
    );
    assert_eq!(
        f.targets.harnesses[0].mcp_file,
        f.home.join(".codex/config.toml")
    );
    let loaded = crate::load(&f.targets.roots, Some(config.as_bytes())).unwrap();
    crate::test_support::assert_leaf_origins(&loaded);
}

#[test]
fn only_manifest_matched_utility_hooks_refuse_and_check_reports_findings() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    let checkout = f.home.join("legacy-checkout");
    f.targets.legacy_checkout = Some(checkout.clone());
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
    let config = f.home.join(".claude/settings.json");
    let foreign = b"{\"hooks\":{\"PostToolUse\":[{\"matcher\":\"Bash\",\"hooks\":[{\"type\":\"command\",\"command\":\"foreign command remains unchanged\"}]}]}}\n";
    fs::write(&config, foreign).unwrap();
    assert!(f.plan().actions().is_empty());
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .is_empty()
    );
    assert_eq!(fs::read(&config).unwrap(), foreign);
    let command = format!(
        "/unrecorded/python '{}'",
        checkout.join("scripts/utility_output_hook.py").display()
    );
    let ambiguous = serde_json::to_vec(&serde_json::json!({"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":command,"timeout":30}]}]}})).unwrap();
    fs::write(&config, &ambiguous).unwrap();
    let release = f.release();
    assert!(matches!(
        Snapshots::capture(&release, &f.targets, &LocalFs),
        Err(InstallError::LegacyUnresolved(_))
    ));
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .iter()
        .any(|finding| finding.rule == "legacy_entry"
            && finding.path.to_path_buf().unwrap() == config)
    );
    assert_eq!(fs::read(&config).unwrap(), ambiguous);
}

#[test]
fn check_install_detects_reintroduced_global_legacy_link() {
    let mut f = Fixture::new();
    f.targets.legacy_checkout = Some(f.home.join("legacy-checkout"));
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
    fs::create_dir_all(f.home.join(".agents")).unwrap();
    let global = f.home.join(".agents/AGENTS.md");
    symlink(
        f.targets
            .legacy_checkout
            .as_ref()
            .unwrap()
            .join("AGENTS.md"),
        &global,
    )
    .unwrap();
    let findings = inspect_install(
        &f.targets,
        None,
        "fixture-target",
        &LocalFs,
        &Smoke,
        &CancelToken::new(),
    )
    .unwrap()
    .findings;
    assert!(
        findings
            .iter()
            .any(|f| f.rule == "legacy_entry" && f.path.to_path_buf().unwrap() == global)
    );
}

#[test]
fn install_uses_strict_configuration_before_planning() {
    let mut f = Fixture::new();
    let state = f.targets.roots.state.as_path();
    fs::create_dir_all(state).unwrap();
    let bytes = b"schema_version=1\nforeign_key='private sentinel'\n";
    fs::write(state.join("config.toml"), bytes).unwrap();
    let error = crate::apply_install_config_with_domains(
        &mut f.targets,
        &LocalFs,
        &crate::domains::NO_DOMAINS,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        InstallError::Config(crate::ConfigError::Schema {
            rule: "unknown_key",
            ..
        })
    ));
    assert!(!error.to_string().contains("private sentinel"));
    assert_eq!(
        fs::read(f.targets.roots.state.as_path().join("config.toml")).unwrap(),
        bytes
    );
}

#[test]
fn protected_destination_is_refused_before_any_filesystem_read() {
    struct NoReads;
    impl SafeFs for NoReads {
        fn root(&self, _: bool) -> TestFsResult<Box<dyn SafeDir>> {
            panic!("unexpected root read")
        }
        fn open_anchor(&self, _: &Path) -> TestFsResult<Box<dyn SafeDir>> {
            panic!("unexpected anchor read")
        }
        fn open_source(&self, _: &Path) -> TestFsResult<Box<dyn SafeFile>> {
            panic!("unexpected source read")
        }
        fn temporary_name(&self) -> TestFsResult<OsString> {
            panic!("unexpected write")
        }
    }
    let mut f = Fixture::new();
    let release = f.release();
    let old = f.home.join("old-checkout");
    fs::create_dir(&old).unwrap();
    let sentinel = old.join("config.toml");
    fs::write(&sentinel, "private sentinel must not be read").unwrap();
    // Composition resolves aliases to a canonical protected root once; the
    // pure plan must reject every destination before opening any file.
    let alias = f.home.join("old-alias");
    symlink(&old, &alias).unwrap();
    f.targets
        .protected_roots
        .push(alias.canonicalize().unwrap());
    f.targets.harnesses[0].mcp_file = alias.join("config.toml");
    assert_eq!(
        f.targets
            .resolve_destinations(&LocalFs)
            .unwrap_err()
            .to_string(),
        format!("unmanaged destination conflicts: {}", sentinel.display())
    );
    let error = Snapshots::capture(&release, &f.targets, &NoReads).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("unmanaged destination conflicts: {}", sentinel.display())
    );
    assert_eq!(
        fs::read_to_string(&sentinel).unwrap(),
        "private sentinel must not be read"
    );
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn mcp_registry_matches_recorded_client_config_contracts() {
    let mut f = Fixture::new();
    for (harness, fixture) in [
        (
            Harness::Claude,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/contracts/claude-code-2.1.310/config.json"
            )),
        ),
        (
            Harness::Codex,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/contracts/codex-cli-0.163.0/config.toml"
            )),
        ),
        (
            Harness::Agy,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/contracts/agy-1.4.6/mcp_config.json"
            )),
        ),
    ] {
        f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[harness]);
        let normalized = fixture
            .replace(
                "/FIXTURE_ROOT/bin/mcp-probe",
                f.targets.bin_dir.join("provenant").to_str().unwrap(),
            )
            .replace(
                "/FIXTURE_ROOT/state",
                f.targets.roots.state.as_path().to_str().unwrap(),
            );
        let document: serde_json::Value = if harness == Harness::Codex {
            toml_edit::de::from_str(&normalized).unwrap()
        } else {
            serde_json::from_str(&normalized).unwrap()
        };
        let mut expected = document[if harness == Harness::Codex {
            "mcp_servers"
        } else {
            "mcpServers"
        }]["fabric"]
            .clone();
        // Codex's recorded optional env is absent; an empty env has the same
        // accepted wire meaning and is emitted explicitly by the registry.
        if harness == Harness::Codex {
            expected["env"] = serde_json::json!({});
        }
        let plan = f.plan();
        let edit = plan
            .actions()
            .iter()
            .flat_map(|a| &a.edits)
            .find(|edit| edit.selector.ends_with("fabric"))
            .unwrap();
        assert_eq!(edit.after.as_ref(), Some(&expected));
        if harness == Harness::Claude {
            let recorded: serde_json::Value = serde_json::from_str(
                &include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/fixtures/contracts/claude-code-2.1.310/stdio-argv.json"
                ))
                .replace(
                    "/FIXTURE_ROOT/state",
                    f.targets.roots.state.as_path().to_str().unwrap(),
                ),
            )
            .unwrap();
            assert_eq!(expected["args"], recorded);
        }
        assert!(!f.targets.roots.state.as_path().exists());
    }
}

#[test]
fn compiled_replace_disposition_refuses_missing_assets_and_links_library_skills() {
    let mut f = Fixture::new();
    let legacy = f.home.join("legacy-checkout");
    f.targets.legacy_checkout = Some(legacy.clone());
    let skills = f.targets.harnesses[0].skills_dir.clone();
    fs::create_dir_all(&skills).unwrap();
    symlink(legacy.join("skills/autopilot"), skills.join("autopilot")).unwrap();
    assert_eq!(
        Snapshots::capture(&f.release(), &f.targets, &LocalFs)
            .unwrap_err()
            .to_string(),
        "invalid release manifest: missing legacy replacement asset"
    );
    assert_eq!(
        fs::read_link(skills.join("autopilot")).unwrap(),
        legacy.join("skills/autopilot")
    );
    assert!(!f.targets.roots.state.as_path().exists());
    let mut f = Fixture::new();
    let legacy = f.home.join("legacy-checkout");
    f.targets.legacy_checkout = Some(legacy.clone());
    let skills = f.targets.harnesses[0].skills_dir.clone();
    fs::create_dir_all(&skills).unwrap();
    symlink(legacy.join("skills/autopilot"), skills.join("autopilot")).unwrap();
    for relative in ["skills/autopilot", "library/playwright"] {
        let dir = f.source.join(relative);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: retained\n---\n").unwrap();
    }
    symlink(legacy.join("library/playwright"), skills.join("playwright")).unwrap();
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
    assert!(
        fs::read_link(skills.join("autopilot"))
            .unwrap()
            .ends_with("skills/autopilot")
    );
    assert!(
        fs::read_link(skills.join("playwright"))
            .unwrap()
            .ends_with("library/playwright")
    );
    assert!(f.plan().actions().is_empty());
}

#[test]
fn absolute_bootstrap_refuses_intermediate_state_and_release_symlinks() {
    let f = Fixture::new();
    let foreign = f.home.join("foreign");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), b"untouched").unwrap();
    for root in [
        f.targets.roots.state.as_path().to_path_buf(),
        f.source.clone(),
    ] {
        fs::create_dir_all(&root).unwrap();
        let link = root.join("escaped");
        symlink(&foreign, &link).unwrap();
        assert!(directory(&link.join("nested"), true, &LocalFs).is_err());
        assert!(!foreign.join("nested").exists());
        assert_eq!(fs::read(foreign.join("sentinel")).unwrap(), b"untouched");
    }
}

#[test]
fn state_mount_descendant_refuses_before_lock_or_receipt_writes() {
    let f = Fixture::new();
    let plan = f.plan();
    fs::create_dir_all(f.targets.roots.state.as_path().join("install")).unwrap();
    let fault = FaultFs::new(FaultMode::StateMount, None);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = install(
        &plan,
        &InstallContext {
            fs: &fault,
            ..context(&clock, &cancel)
        },
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(
        matches!(error, InstallError::Fs(FsError::CrossDevice { path }) if path == Path::new("install"))
    );
    assert!(!f.receipt().exists());
    assert!(
        !f.targets
            .roots
            .state
            .as_path()
            .join("install/install.lock")
            .exists()
    );
}

#[test]
fn source_mount_descendant_refuses_before_reading_manifest_assets() {
    let f = Fixture::new();
    f.release();
    let fault = FaultFs::new(FaultMode::SourceMount, None);
    let error = Release::read(&f.source, "fixture-target", &fault).unwrap_err();
    assert!(
        matches!(error, InstallError::Fs(FsError::CrossDevice { path }) if path == Path::new("skills"))
    );
}

#[test]
fn protected_legacy_removal_refuses_before_writes() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    let checkout = f.home.join("legacy-checkout");
    f.targets.legacy_checkout = Some(checkout.clone());
    let workflows = f.home.join(".claude/workflows");
    fs::create_dir_all(&workflows).unwrap();
    let leaf = workflows.join("cross-verify.js");
    symlink(checkout.join("workflows/cross-verify.js"), &leaf).unwrap();
    f.targets.protected_roots.push(workflows);
    let release = f.release();
    let snapshots = Snapshots::capture(&release, &f.targets, &LocalFs).unwrap();
    assert!(
        matches!(plan_install(release, f.targets.clone(), snapshots), Err(InstallError::Conflict(p)) if p == leaf)
    );
    assert_eq!(
        fs::read_link(&leaf).unwrap(),
        checkout.join("workflows/cross-verify.js")
    );
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn uninstall_without_receipt_reports_nothing_installed() {
    let f = Fixture::new();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let outcome = uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(outcome.actions.is_empty());
    assert_eq!(outcome.findings[0].rule, "not_installed");
    assert!(!f.targets.roots.state.as_path().exists());
}

#[test]
fn foreign_git_hooks_are_preserved_and_reported() {
    for foreign_link in [false, true] {
        let mut f = Fixture::new();
        let checkout = f.home.join("legacy-checkout");
        let hooks = checkout.join(".git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        f.targets.legacy_checkout = Some(checkout);
        f.targets.git_hooks_dir = Some(hooks.clone());
        let path = hooks.join("post-merge");
        if foreign_link {
            symlink("/foreign/hook", &path).unwrap();
        } else {
            fs::write(&path, b"personal hook\n").unwrap();
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let plan = f.plan();
        assert!(
            plan.actions()
                .iter()
                .all(|a| a.path.to_path_buf().unwrap() != path)
        );
        let outcome = install(
            &plan,
            &context(&clock, &cancel),
            &mut Reporter::default(),
            &Smoke,
            false,
        )
        .unwrap();
        assert!(
            outcome
                .findings
                .iter()
                .any(|f| f.path.to_path_buf().unwrap() == path)
        );
        if foreign_link {
            assert_eq!(fs::read_link(&path).unwrap(), Path::new("/foreign/hook"));
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"personal hook\n");
        }
        assert!(
            inspect_install(
                &f.targets,
                None,
                "fixture-target",
                &LocalFs,
                &Smoke,
                &CancelToken::new()
            )
            .unwrap()
            .findings
            .iter()
            .any(|f| f.path.to_path_buf().unwrap() == path)
        );
    }
}

#[test]
fn claude_large_harness_config_is_bounded_and_byte_preserved() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    let path = f.home.join(".claude.json");
    let payload = "x".repeat(2 * 1024 * 1024);
    let before = format!("{{\"projects\":\"{payload}\"}}\n");
    fs::write(&path, &before).unwrap();
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
    assert!(fs::read_to_string(&path).unwrap().contains(&payload));
    assert!(f.plan().actions().is_empty());
    uninstall(
        &f.targets,
        &context(&clock, &cancel),
        &mut Reporter::default(),
    )
    .unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains(&payload));
    fs::write(&path, " ".repeat(16 * 1024 * 1024 + 1)).unwrap();
    assert!(Snapshots::capture(&f.release(), &f.targets, &LocalFs).is_err());
}

#[test]
fn install_replaces_only_the_exact_legacy_command_file_form() {
    for recognised in [true, false] {
        let mut f = Fixture::new();
        f.targets.legacy_checkout = Some(f.home.join("old-checkout"));
        fs::create_dir_all(&f.targets.bin_dir).unwrap();
        let binary = f.targets.bin_dir.join("provenant");
        // ELF/Mach-O bytes contain the old front door's embedded identity.
        let mut before = b"\x7fELFfixture\n# Managed by scripts/install-harness; local edits to an installed copy are not preserved.\nprovenant-front-door source-sha256=".to_vec();
        before.extend_from_slice(
            b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\ncompiled payload",
        );
        if !recognised {
            before[0] = b'x';
        }
        fs::write(&binary, &before).unwrap();
        let release = f.release();
        let snapshots = Snapshots::capture(&release, &f.targets, &LocalFs).unwrap();
        let plan = plan_install(release, f.targets.clone(), snapshots);
        if recognised {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let plan = plan.unwrap();
            install(
                &plan,
                &context(&clock, &cancel),
                &mut Reporter::default(),
                &Smoke,
                true,
            )
            .unwrap();
            assert_eq!(fs::read(&binary).unwrap(), before);
            install(
                &plan,
                &context(&clock, &cancel),
                &mut Reporter::default(),
                &Smoke,
                false,
            )
            .unwrap();
            assert!(binary.is_symlink());
        } else {
            assert!(matches!(plan, Err(InstallError::Conflict(p)) if p == binary));
            assert_eq!(fs::read(&binary).unwrap(), before);
            assert!(!f.targets.roots.state.as_path().exists());
        }
    }
}

#[test]
fn manifest_git_link_exception_never_authorises_state_or_source_removal() {
    for in_state in [true, false] {
        let mut f = Fixture::new();
        let checkout = if in_state {
            f.targets.roots.state.as_path().join("old-checkout")
        } else {
            f.source.join("old-checkout")
        };
        let hooks = checkout.join(".git/hooks");
        // Package first: the fixture release must never inventory this checkout.
        let release = f.release();
        fs::create_dir_all(&hooks).unwrap();
        let path = hooks.join("post-merge");
        let target = checkout.join("scripts/hooks/post-merge");
        symlink(&target, &path).unwrap();
        f.targets.legacy_checkout = Some(checkout);
        f.targets.git_hooks_dir = Some(hooks);
        let snapshots = Snapshots::capture(&release, &f.targets, &LocalFs).unwrap();
        assert!(
            matches!(plan_install(release, f.targets.clone(), snapshots), Err(InstallError::Conflict(p)) if p == path)
        );
        assert_eq!(fs::read_link(path).unwrap(), target);
        assert!(!f.receipt().exists());
    }
}

#[test]
fn install_removes_each_retired_workflow_and_keeps_replace_git_hook() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Claude]);
    let checkout = f.home.join("legacy-checkout");
    let hooks = checkout.join(".git/hooks");
    let workflows = f.home.join(".claude/workflows");
    fs::create_dir_all(&hooks).unwrap();
    fs::create_dir_all(&workflows).unwrap();
    f.targets.legacy_checkout = Some(checkout.clone());
    f.targets.git_hooks_dir = Some(hooks.clone());
    for name in ["codebase-polish.js", "cross-verify.js", "implement-run.js"] {
        symlink(checkout.join("workflows").join(name), workflows.join(name)).unwrap();
    }
    for name in ["pre-push", "post-merge"] {
        symlink(checkout.join("scripts/hooks").join(name), hooks.join(name)).unwrap();
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let plan = f.plan();
    assert!(
        plan.findings()
            .iter()
            .any(|finding| finding.path.to_path_buf().unwrap() == hooks.join("pre-push"))
    );
    let outcome = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    assert!(
        outcome
            .findings
            .iter()
            .any(|finding| finding.path.to_path_buf().unwrap() == hooks.join("pre-push"))
    );
    for name in ["codebase-polish.js", "cross-verify.js", "implement-run.js"] {
        assert!(fs::symlink_metadata(workflows.join(name)).is_err());
    }
    assert!(fs::symlink_metadata(hooks.join("post-merge")).is_err());
    assert_eq!(
        fs::read_link(hooks.join("pre-push")).unwrap(),
        checkout.join("scripts/hooks/pre-push")
    );
    assert!(
        inspect_install(
            &f.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &CancelToken::new()
        )
        .unwrap()
        .findings
        .iter()
        .any(|finding| finding.rule == "legacy_entry"
            && finding.path.to_path_buf().unwrap() == hooks.join("pre-push"))
    );
}

#[test]
fn opencode_reapply_without_legacy_flag_drops_old_release_and_keeps_receipt_ownership() {
    let mut f = Fixture::new();
    f.targets = Targets::new(f.targets.roots.clone(), &f.home, &[Harness::Opencode]);
    f.targets.legacy_checkout = Some(f.home.join("legacy-checkout"));
    fs::create_dir_all(f.source.join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        fs::write(f.source.join("instructions").join(name), b"instructions").unwrap();
    }
    let config = f.targets.harnesses[0].mcp_file.clone();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        serde_json::to_vec(
            &serde_json::json!({"instructions":[f.home.join(".agents/AGENTS.md"), "personal.md"]}),
        )
        .unwrap(),
    )
    .unwrap();
    let a = f.release();
    let old = release_root(f.targets.roots.state.as_path(), a.manifest.source_digest)
        .join("instructions/AGENTS.md");
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let ctx = context(&clock, &cancel);
    install(
        &f.plan_for(a),
        &ctx,
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let b = f.release_variant("opencode-b", &["example"]);
    fs::create_dir_all(b.source().join("instructions")).unwrap();
    for name in ["AGENTS.md", "HARNESS.md"] {
        fs::write(
            b.source().join("instructions").join(name),
            b"new instructions",
        )
        .unwrap();
    }
    fs::remove_file(b.source().join("release.json")).unwrap();
    let b = package_fixture_release(b.source(), b.manifest.source_digest);
    let new = release_root(f.targets.roots.state.as_path(), b.manifest.source_digest)
        .join("instructions/AGENTS.md");
    // A partial prior publication may leave both the old receipt value and the
    // desired new value. Reapply must settle to one newly receipted entry.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    document["instructions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(new));
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    f.targets.legacy_checkout = None;
    install(
        &f.plan_for(b.clone()),
        &ctx,
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert!(
        !document["instructions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(old))
    );
    assert_eq!(
        document["instructions"].as_array().unwrap(),
        &vec![serde_json::json!("personal.md"), serde_json::json!(new)]
    );
    let receipt = read_receipt(&f.targets, &LocalFs).unwrap().unwrap();
    assert!(
        receipt
            .entries
            .iter()
            .any(|entry| entry.selector == format!("array:{}", new.display()))
    );
    assert!(f.plan_for(b.clone()).actions().is_empty());
    let mut duplicate = document.clone();
    duplicate["instructions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(new));
    let bytes = serde_json::to_vec(&duplicate).unwrap();
    fs::write(&config, &bytes).unwrap();
    assert!(matches!(
        Snapshots::capture(&b, &f.targets, &LocalFs),
        Err(InstallError::InvalidConfig("duplicate owned instruction"))
    ));
    assert_eq!(fs::read(&config).unwrap(), bytes);
}

#[test]
fn inspect_install_binds_changed_tampered_bytes_with_identical_metadata_and_findings() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
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
    let inspect = || {
        inspect_install(
            &fixture.targets,
            None,
            "fixture-target",
            &LocalFs,
            &Smoke,
            &cancel,
        )
        .unwrap()
    };
    let clean = inspect();
    assert!(clean.findings.is_empty());
    let repeated = inspect();
    assert_eq!(clean.findings, repeated.findings);
    assert_eq!(clean.input_digest, repeated.input_digest);
    assert!(repeated.error.is_none());
    let asset = fixture
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string())
        .join("skills/example/SKILL.md");
    let before = fs::metadata(&asset).unwrap();
    let modified = before.modified().unwrap();
    let file = fs::OpenOptions::new().write(true).open(&asset).unwrap();
    let set_bytes = |byte| {
        fs::write(&asset, vec![byte; before.len() as usize]).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
    };
    set_bytes(b'a');
    let first = inspect();
    let metadata = snapshot(&asset, &LocalFs).unwrap().unwrap().metadata;
    set_bytes(b'b');
    assert_eq!(
        metadata,
        snapshot(&asset, &LocalFs).unwrap().unwrap().metadata
    );
    let second = inspect();
    assert_eq!(first.findings, second.findings);
    assert!(
        first
            .findings
            .iter()
            .any(|finding| finding.rule == "release_drift")
    );
    assert_ne!(first.input_digest, second.input_digest);
}
