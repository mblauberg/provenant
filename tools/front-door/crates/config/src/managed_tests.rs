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
