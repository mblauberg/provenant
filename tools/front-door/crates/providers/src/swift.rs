//! Swift toolchain grants and the lane `swift` shim.
//!
//! A lane runs inside one Seatbelt profile, and SwiftPM cannot nest its own
//! sandbox (`sandbox_apply: Operation not permitted`) or run its default
//! build system there: XCBuild creates temporary directories in `/var/tmp`.
//! The shim keeps SwiftPM inside what the profile already allows:
//!
//! - `--disable-sandbox`: the lane profile already confines the manifest
//!   process and every build tool, so the nested profile adds nothing.
//! - `--build-system native`: writes only the package scratch path (`.build`
//!   by default), `TMPDIR` and the lane cache, all writable to a writer.
//! - SwiftPM's cache, configuration and security directories, and the Clang
//!   module cache, move from the user's `~/Library` and `~/.cache` into the
//!   lane cache, so no shared host store is written.
//!
//! The grants add read and execute on the selected developer directory (the
//! Xcode toolchain or SDK), and read on Xcode's shared frameworks that
//! SwiftPM links. They add no write access. The shim also selects that
//! developer directory: the lane-wide `DEVELOPER_DIR` names the Command Line
//! Tools for Cargo's linker, whose SDK and compiler can disagree.
use crate::launch::{Access, PrepareInput, SetupStep, exact, subtree};
use provenant_confinement::permissions::PathGrant;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const APPLICATIONS: &str = "/Applications";
const XCODE_LICENCE: &str = "/Library/Preferences/com.apple.dt.Xcode.plist";
const COMMAND_LINE_TOOLS: &str = "/Library/Developer/CommandLineTools";

/// The developer directory `xcode-select` chose, and its `swift` driver.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SwiftToolchain {
    developer: PathBuf,
    swift: PathBuf,
}

impl SwiftToolchain {
    /// Resolve a developer directory to its toolchain, when it is a known
    /// location and has a driver.
    pub(crate) fn at(developer: &Path) -> Option<Self> {
        Self::at_in(developer, Path::new(APPLICATIONS))
    }

    /// As `at`, with the directory that holds Xcode app bundles. The whole
    /// developer subtree becomes executable, so only the Command Line Tools
    /// and `<applications>/<name>.app/Contents/Developer` qualify, by their
    /// canonical path; any other `DEVELOPER_DIR` adds no grant and no shim.
    fn at_in(developer: &Path, applications: &Path) -> Option<Self> {
        let developer = std::fs::canonicalize(developer).ok()?;
        let contents = developer.parent()?;
        let bundle = contents.parent()?;
        let known = developer == Path::new(COMMAND_LINE_TOOLS)
            || (developer.file_name()? == "Developer"
                && contents.file_name()? == "Contents"
                && bundle.extension()? == "app"
                && bundle.parent()? == std::fs::canonicalize(applications).ok()?);
        if !known {
            return None;
        }
        let swift = [
            "Toolchains/XcodeDefault.xctoolchain/usr/bin/swift",
            "usr/bin/swift",
        ]
        .iter()
        .map(|relative| developer.join(relative))
        .find(|candidate| candidate.is_file())?;
        Some(Self { developer, swift })
    }

    /// The host's selected toolchain, if any: `DEVELOPER_DIR`, which takes
    /// precedence even when it names no toolchain, else `xcode-select -p`
    /// (run once, bounded). There is no fallback to a default Xcode, so a
    /// host that selected the Command Line Tools keeps them.
    #[cfg(target_os = "macos")]
    pub(crate) fn selected(host_var: &dyn Fn(&str) -> Option<OsString>) -> Option<Self> {
        if let Some(explicit) = host_var("DEVELOPER_DIR").filter(|value| !value.is_empty()) {
            return Self::at(Path::new(&explicit));
        }
        // This crate's unit tests pin the profile to their fixed lookup, so
        // the developer directory a runner has selected never changes an
        // expectation.
        if cfg!(test) {
            return None;
        }
        Self::at(&xcode_select_path()?)
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn selected(_: &dyn Fn(&str) -> Option<OsString>) -> Option<Self> {
        None
    }

    fn is_command_line_tools(&self) -> bool {
        self.developer == Path::new(COMMAND_LINE_TOOLS)
    }

    /// Read grants for the developer directory and, for an Xcode app, the
    /// parts of `Contents` a build touches: `SharedFrameworks` holds
    /// `SwiftBuild` and `BuildServerProtocol`, `Frameworks` holds the `xcrun`
    /// loader library, and the bundle records give its version. Nothing here
    /// is writable. The Command Line Tools already have their own grants.
    pub(crate) fn reads(&self) -> Vec<PathGrant> {
        if self.is_command_line_tools() {
            return Vec::new();
        }
        let mut grants = vec![subtree(&self.developer)];
        if let Some(contents) = self.developer.parent() {
            grants.extend([
                subtree(contents.join("Frameworks")),
                subtree(contents.join("SharedFrameworks")),
                exact(contents.join("Info.plist")),
                exact(contents.join("version.plist")),
            ]);
        }
        // `xcrun` refuses to run until it reads the accepted-licence record.
        // The file is root-owned and public.
        let licence = Path::new(XCODE_LICENCE);
        if licence.is_file() {
            grants.push(exact(licence));
        }
        grants
    }

    /// The compiler, linker, SwiftPM and test runner live under here.
    pub(crate) fn executables(&self) -> Vec<PathGrant> {
        if self.is_command_line_tools() {
            Vec::new()
        } else {
            vec![subtree(&self.developer)]
        }
    }

    /// The shim script: it selects this toolchain, redirects module and
    /// SwiftPM caches into `cache`, and runs SwiftPM inside the lane profile.
    pub(crate) fn shim(&self, cache: &Path) -> Result<Vec<u8>, crate::ProviderError> {
        let quote = |path: &Path| -> Result<String, crate::ProviderError> {
            let text = path.to_str().ok_or(crate::ProviderError::InvalidPath)?;
            if !path.is_absolute() || text.chars().any(char::is_control) {
                return Err(crate::ProviderError::InvalidPath);
            }
            Ok(crate::lane::shell_quote(text))
        };
        let developer = quote(&self.developer)?;
        let swift = quote(&self.swift)?;
        let modules = quote(&cache.join("clang-modules"))?;
        let packages = quote(&cache.join("cache"))?;
        let configuration = quote(&cache.join("configuration"))?;
        let security = quote(&cache.join("security"))?;
        let cache_text = quote(cache)?;
        Ok(format!(
            r#"#!/bin/sh
# Provenant lane shim: SwiftPM inside the lane's Seatbelt profile.
DEVELOPER_DIR={developer}
CLANG_MODULE_CACHE_PATH={modules}
export DEVELOPER_DIR CLANG_MODULE_CACHE_PATH
unset SDKROOT
real={swift}
sub=${{1-}}
case "$sub" in
  build|test|run|package)
    shift
    build_system=--build-system=native
    [ "$sub" = package ] && build_system=
    for arg in "$@"; do
      case "$arg" in
        --) break ;;
        --build-system|--build-system=*) build_system= ;;
      esac
    done
    set -- "$sub" --disable-sandbox --cache-path {packages} --config-path {configuration} --security-path {security} ${{build_system:+"$build_system"}} "$@"
    ;;
esac
"$real" "$@"
status=$?
if [ "$status" -ne 0 ]; then
  case "$sub" in
    build|test|run|package)
      printf 'provenant: swift %s exited %s inside the lane sandbox. Writes are allowed only in the worktree, TMPDIR (%s) and the lane cache (%s); an Operation not permitted or authorization denied error names a path outside them. SwiftPM runs with --disable-sandbox and the native build system; pass --build-system swiftbuild to override.\n' "$sub" "$status" "$TMPDIR" {cache_text} >&2
      ;;
  esac
fi
exit "$status"
"#
        )
        .into_bytes())
    }
}

/// `xcode-select -p`, killed after a few seconds. Its environment is empty,
/// so only the host's own selection answers.
#[cfg(target_os = "macos")]
fn xcode_select_path() -> Option<PathBuf> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let argv: Vec<OsString> = vec!["/usr/bin/xcode-select".into(), "-p".into()];
    let mut options = ExecOptions::new(&argv, Path::new("/"), std::time::Duration::from_secs(5));
    options.env = Some(&[]);
    options.stdout = StreamPolicy::Capture { limit: 4096 };
    options.stderr = StreamPolicy::Discard;
    let output = SystemExecutor::new(1).ok()?.execute(options).ok()?;
    if output.termination != Termination::Exit(0) || output.stdout.truncated {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout.bytes).ok()?.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// Grants, shim and `PATH` for a writer that may run `swift build` and
/// `swift test`. Read-only lanes (#2542) take the same toolchain with
/// scratch-only roots; this adds nothing for them yet.
pub(crate) fn swift_tools(
    input: &PrepareInput<'_>,
    access: Access,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut [(OsString, OsString)],
    setup: &mut Vec<SetupStep>,
) {
    if access != Access::WorktreeWrite {
        return;
    }
    let Some(toolchain) = SwiftToolchain::selected(input.host_var) else {
        return;
    };
    install(
        &toolchain,
        &input.confined.permissions().scratch.path,
        &crate::launch::lane_cache(input).join("swift"),
        runtime_reads,
        tool_exec_roots,
        env,
        setup,
    );
}

fn install(
    toolchain: &SwiftToolchain,
    scratch: &Path,
    cache: &Path,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut [(OsString, OsString)],
    setup: &mut Vec<SetupStep>,
) {
    let Ok(shim) = toolchain.shim(cache) else {
        return;
    };
    runtime_reads.extend(toolchain.reads());
    tool_exec_roots.extend(toolchain.executables());
    let bin = scratch.join("bin");
    let create = SetupStep::CreateDir(bin.clone());
    if !setup.contains(&create) {
        setup.push(create);
    }
    setup.push(SetupStep::WriteExecutable {
        path: bin.join("swift"),
        bytes: shim,
    });
    tool_exec_roots.push(subtree(&bin));
    if let Some((_, path)) = env.iter_mut().find(|(key, _)| key == "PATH")
        && std::env::split_paths(path).next().as_deref() != Some(bin.as_path())
        && let Ok(joined) =
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&*path)))
    {
        *path = joined;
    }
}

/// Whether a lane would receive the Swift shim on this host. Integration
/// tests gate on it, as on `swift` itself.
pub fn toolchain_available(host_var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    SwiftToolchain::selected(host_var).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(root: &Path, relative: &str) -> SwiftToolchain {
        let developer = root.join("Xcode.app/Contents/Developer");
        let swift = developer.join(relative);
        std::fs::create_dir_all(swift.parent().unwrap()).unwrap();
        std::fs::write(&swift, b"").unwrap();
        std::fs::create_dir_all(developer.parent().unwrap().join("SharedFrameworks")).unwrap();
        SwiftToolchain::at_in(&developer, root).unwrap()
    }

    #[test]
    fn xcode_grants_are_read_and_execute_on_the_toolchain_only() {
        let root = tempfile::tempdir().unwrap();
        let toolchain = fake(
            &std::fs::canonicalize(root.path()).unwrap(),
            "Toolchains/XcodeDefault.xctoolchain/usr/bin/swift",
        );
        let reads = toolchain.reads();
        assert!(
            reads
                .iter()
                .any(|grant| grant.path.ends_with("Contents/SharedFrameworks")),
            "{reads:?}"
        );
        assert!(
            !reads
                .iter()
                .any(|grant| grant.path.ends_with("Xcode.app/Contents")
                    && grant.scope == provenant_confinement::permissions::PathScope::Subtree)
        );
        assert!(reads.iter().all(|grant| !grant.path.starts_with("/Users")));
        assert_eq!(toolchain.executables().len(), 1);
    }

    #[test]
    fn a_directory_without_a_driver_is_no_toolchain() {
        let root = tempfile::tempdir().unwrap();
        assert!(SwiftToolchain::at(root.path()).is_none());
    }

    #[test]
    fn shim_selects_the_toolchain_and_keeps_swiftpm_inside_the_lane() {
        let root = tempfile::tempdir().unwrap();
        let toolchain = fake(
            &std::fs::canonicalize(root.path()).unwrap(),
            "Toolchains/XcodeDefault.xctoolchain/usr/bin/swift",
        );
        let text =
            String::from_utf8(toolchain.shim(Path::new("/lane/cache/swift")).unwrap()).unwrap();
        for needle in [
            "unset SDKROOT",
            "DEVELOPER_DIR='",
            "CLANG_MODULE_CACHE_PATH='/lane/cache/swift/clang-modules'",
            "--disable-sandbox",
            "--cache-path '/lane/cache/swift/cache'",
            "--build-system=native",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }

    #[test]
    fn install_puts_the_shim_first_on_path_and_grants_no_write() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let toolchain = fake(&root, "Toolchains/XcodeDefault.xctoolchain/usr/bin/swift");
        let (mut reads, mut execs, mut setup) = (Vec::new(), Vec::new(), Vec::new());
        let mut env = vec![("PATH".into(), "/usr/bin:/bin".into())];
        install(
            &toolchain,
            Path::new("/lane/scratch"),
            Path::new("/lane/cache/swift"),
            &mut reads,
            &mut execs,
            &mut env,
            &mut setup,
        );
        assert_eq!(env[0].1, "/lane/scratch/bin:/usr/bin:/bin");
        assert!(setup.iter().any(|step| matches!(
            step,
            SetupStep::WriteExecutable { path, .. } if path == Path::new("/lane/scratch/bin/swift")
        )));
        assert!(reads.iter().chain(&execs).all(|grant| {
            grant.path.starts_with(&root)
                || grant.path.starts_with("/lane/scratch/bin")
                || grant.path.starts_with("/Library/Preferences")
        }));
    }

    #[test]
    fn shim_refuses_a_cache_path_with_control_characters() {
        let root = tempfile::tempdir().unwrap();
        let toolchain = fake(
            &std::fs::canonicalize(root.path()).unwrap(),
            "usr/bin/swift",
        );
        assert!(toolchain.shim(Path::new("/lane/ca\nche")).is_err());
        assert!(toolchain.shim(Path::new("relative")).is_err());
    }

    #[test]
    fn only_the_command_line_tools_and_an_app_bundle_are_toolchains() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let toolchain = fake(&root, "usr/bin/swift");
        let developer = toolchain.developer.clone();
        assert!(SwiftToolchain::at_in(&developer, &root).is_some());
        // The same app, when Xcode apps are expected somewhere else.
        assert!(SwiftToolchain::at_in(&developer, &root.join("Xcode.app")).is_none());
        // A directory with a driver that is not an app bundle's developer directory.
        let loose = root.join("loose");
        std::fs::create_dir_all(loose.join("usr/bin")).unwrap();
        std::fs::write(loose.join("usr/bin/swift"), b"").unwrap();
        assert!(SwiftToolchain::at_in(&loose, &root).is_none());
        assert!(SwiftToolchain::at_in(&loose, &loose).is_none());
    }

    /// Run the shim against a stand-in driver that echoes its arguments.
    #[cfg(unix)]
    fn run_shim(cache: &Path, args: &[&str], status: &str) -> (String, String) {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let toolchain = fake(&root, "usr/bin/swift");
        std::fs::write(
            &toolchain.swift,
            "#!/bin/sh\necho \"$@\"\nexit $FAKE_STATUS\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&toolchain.swift, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let script = root.join("shim.sh");
        std::fs::write(&script, toolchain.shim(cache).unwrap()).unwrap();
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor};
        let argv: Vec<OsString> = ["/bin/sh".into(), script.into_os_string()]
            .into_iter()
            .chain(args.iter().map(OsString::from))
            .collect();
        let env: Vec<(OsString, OsString)> = vec![
            ("FAKE_STATUS".into(), status.into()),
            ("TMPDIR".into(), "/lane/tmp".into()),
        ];
        let mut options = ExecOptions::new(&argv, &root, std::time::Duration::from_secs(10));
        options.env = Some(&env);
        let output = SystemExecutor::new(1).unwrap().execute(options).unwrap();
        (
            String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
            String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn build_system_scan_stops_at_the_argument_separator() {
        let cache = Path::new("/lane/cache/swift");
        let (out, _) = run_shim(cache, &["run", "tool", "--", "--build-system"], "0");
        assert!(out.contains("--build-system=native"), "{out}");
        let (out, _) = run_shim(cache, &["build", "--build-system", "swiftbuild"], "0");
        assert!(!out.contains("--build-system=native"), "{out}");
        let (out, _) = run_shim(cache, &["package", "describe"], "0");
        assert!(!out.contains("--build-system=native"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn failure_hint_prints_the_cache_path_without_running_it() {
        let marker = tempfile::tempdir().unwrap();
        let marker = std::fs::canonicalize(marker.path()).unwrap().join("ran");
        let cache = format!("/lane/$(touch {})/`id`\"x", marker.display());
        let (_, err) = run_shim(Path::new(&cache), &["build"], "3");
        assert!(err.contains("exited 3"), "{err}");
        assert!(err.contains(&cache), "{err}");
        assert!(!marker.exists());
    }
}
