//! Fabric's system-ps bridge; fallback is permitted only for spawn refusals.
//!
//! The caller supplies the bundled shim's installed absolute path. This library
//! remains inert; no command ownership or installed path is changed here.
use crate::bounded::{run_bounded_limited, BoundedError, Options};
use std::ffi::OsString;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PsError {
    #[error("ps could not start or be observed")]
    Execution(#[from] BoundedError),
    #[error("ps exited with status {returncode}")]
    Exit { returncode: i32, stdout: String },
    #[error("ps timed out")]
    Timeout,
    #[error("ps output exceeded 1048576 bytes")]
    OutputLimit,
}

/// Paths are values so isolated installations and OS fixtures need no globals.
#[derive(Clone, Copy, Debug)]
pub struct PsPrograms<'a> {
    pub system: &'a Path,
    pub shim: &'a Path,
}
impl PsPrograms<'_> {
    pub fn output(
        self,
        args: &[OsString],
        env: &[(OsString, OsString)],
    ) -> Result<String, PsError> {
        match execute(self.system, args, env) {
            Err(PsError::Execution(BoundedError::Io {
                operation: "spawn",
                source,
            })) if matches!(
                source.raw_os_error(),
                Some(libc::EPERM | libc::EACCES | libc::ENOENT)
            ) && self.shim.exists() =>
            {
                let mut env = env.to_vec();
                match env.iter_mut().find(|(name, _)| name == "PATH") {
                    Some((_, value)) if value.is_empty() => *value = "/usr/bin:/bin".into(),
                    None => env.push(("PATH".into(), "/usr/bin:/bin".into())),
                    _ => {}
                }
                execute(self.shim, args, &env)
            }
            result => result,
        }
    }
}
pub fn ps_output(
    args: &[OsString],
    env: &[(OsString, OsString)],
    shim: &Path,
) -> Result<String, PsError> {
    PsPrograms {
        system: Path::new("/bin/ps"),
        shim,
    }
    .output(args, env)
}
fn execute(
    program: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> Result<String, PsError> {
    let command: Vec<_> = std::iter::once(program.as_os_str().to_owned())
        .chain(args.iter().cloned())
        .collect();
    let mut options = Options::new(Path::new("."), 5.0);
    options.env = Some(env);
    options.merge_stderr = false;
    options.null_stdin = true;
    let result = run_bounded_limited(&command, options)?;
    if result.timed_out {
        return Err(PsError::Timeout);
    }
    if result.output_truncated {
        return Err(PsError::OutputLimit);
    }
    if result.returncode != 0 {
        return Err(PsError::Exit {
            returncode: result.returncode,
            stdout: result.output,
        });
    }
    Ok(result.output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(root: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    #[test]
    fn successful_ps_returns_its_output() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let shim = root.join("skills/orchestrate/scripts/bin/ps");
        let args = [
            "-o".into(),
            "pid=".into(),
            "-p".into(),
            std::process::id().to_string().into(),
        ];
        let output = ps_output(&args, &crate::test_support::environment(), &shim).unwrap();
        assert_eq!(output.trim(), std::process::id().to_string());
    }
    #[test]
    fn only_unstartable_errors_retry_and_path_default_is_fallback_only() {
        let temp = tempfile::tempdir().unwrap();
        let shim = script(temp.path(), "shim", "printf '%s' \"$PATH\"");
        let missing = temp.path().join("missing");
        let programs = PsPrograms {
            system: &missing,
            shim: &shim,
        };
        for env in [vec![], vec![("PATH".into(), "".into())]] {
            assert_eq!(programs.output(&[], &env).unwrap(), "/usr/bin:/bin");
        }
        let env = [("PATH".into(), "/explicit/bin".into())];
        assert_eq!(programs.output(&[], &env).unwrap(), "/explicit/bin");
        let refused = script(temp.path(), "refused", "exit 0");
        std::fs::set_permissions(&refused, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            PsPrograms {
                system: &refused,
                shim: &shim
            }
            .output(&[], &env)
            .unwrap(),
            "/explicit/bin"
        );
        let not_directory = refused.join("ps");
        assert!(matches!(
            PsPrograms {
                system: &not_directory,
                shim: &shim
            }
            .output(&[], &env),
            Err(PsError::Execution(_))
        ));
        assert!(matches!(
            PsPrograms {
                system: &missing,
                shim: &missing
            }
            .output(&[], &env),
            Err(PsError::Execution(_))
        ));
    }
    #[test]
    fn failures_after_start_and_failed_shims_are_never_retried() {
        let temp = tempfile::tempdir().unwrap();
        let failed = script(
            temp.path(),
            "failed",
            "printf 'failure'; printf 'ignored' >&2; exit 7",
        );
        let shim = script(temp.path(), "shim", "printf 'incorrect fallback'");
        assert!(
            matches!(PsPrograms { system: &failed, shim: &shim }.output(&[], &[]), Err(PsError::Exit { returncode: 7, stdout }) if stdout == "failure")
        );
        let missing = temp.path().join("missing");
        assert!(matches!(
            PsPrograms {
                system: &missing,
                shim: &failed
            }
            .output(&[], &[]),
            Err(PsError::Exit { returncode: 7, .. })
        ));
        let signal = script(temp.path(), "signal", "kill -TERM $$");
        assert!(matches!(
            PsPrograms {
                system: &signal,
                shim: &shim
            }
            .output(&[], &[]),
            Err(PsError::Exit {
                returncode: -15,
                ..
            })
        ));
    }
    #[test]
    fn stdout_has_node_utf8_replacement_and_no_newline_translation() {
        let temp = tempfile::tempdir().unwrap();
        let system = script(
            temp.path(),
            "ps",
            "printf 'a\\r\\nb\\r\\377'; printf ignored >&2; read input || printf EOF",
        );
        let actual = PsPrograms {
            system: &system,
            shim: Path::new("/absent"),
        }
        .output(&[], &[])
        .unwrap();
        assert_eq!(actual, "a\r\nb\r�EOF");
    }
    #[test]
    fn exec_file_sync_default_stdout_limit_is_a_failure() {
        let temp = tempfile::tempdir().unwrap();
        let system = script(
            temp.path(),
            "ps",
            "/usr/bin/head -c 1048577 /dev/zero; while :; do :; done",
        );
        let shim = script(temp.path(), "shim", "printf 'incorrect fallback'");
        assert!(matches!(
            PsPrograms {
                system: &system,
                shim: &shim
            }
            .output(&[], &[]),
            Err(PsError::OutputLimit)
        ));
    }
    #[test]
    fn differential_system_ps_matches_node_success_and_failure() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let shim = root.join("skills/orchestrate/scripts/bin/ps");
        let pid = std::process::id();
        let js = format!(
            r#"
import {{ psOutput }} from './runtime/fabric/src/ps.mjs';
const cases = [['-o', 'pid=,ppid=,pgid=', '-p', '{pid}'], ['--provenant-invalid-option']];
console.log(JSON.stringify(cases.map(args => {{
  try {{ return {{ok: psOutput(args)}}; }}
  catch (e) {{ return {{status: e.status, stdout: e.stdout || ''}}; }}
}})));
"#
        );
        let Some(expected) = crate::test_support::node(&root, &js) else {
            return;
        };
        let env = crate::test_support::environment();
        let cases = [
            vec![
                "-o".into(),
                "pid=,ppid=,pgid=".into(),
                "-p".into(),
                pid.to_string().into(),
            ],
            vec!["--provenant-invalid-option".into()],
        ];
        let actual: Vec<_> = cases
            .into_iter()
            .map(|args| match ps_output(&args, &env, &shim) {
                Ok(output) => serde_json::json!({"ok": output}),
                Err(PsError::Exit { returncode, stdout }) => {
                    serde_json::json!({"status": returncode, "stdout": stdout})
                }
                Err(error) => panic!("unexpected ps oracle error: {error}"),
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
