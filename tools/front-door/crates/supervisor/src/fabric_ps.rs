//! Fabric's system-ps bridge; fallback is permitted only for spawn refusals.
//!
//! The caller supplies the bundled shim's installed absolute path. This library
//! remains inert; no command ownership or installed path is changed here.
use crate::bounded::{BoundedError, Options, run_bounded_limited};
use std::ffi::OsString;
use std::path::Path;
use thiserror::Error;

/// Child error evidence, keeping normal exit and signal termination separate.
/// The bounded spool can truncate stdout on overflow; exact Node maxBuffer
/// failure bytes and a pipe-based hard cap remain unported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PsFailure {
    pub code: Option<&'static str>,
    pub status: Option<i32>,
    pub signal: Option<&'static str>,
    pub stdout: String,
    pub stdout_bytes: u64,
    pub stdout_truncated: bool,
}
#[derive(Debug, Error)]
pub enum PsError {
    #[error("ps could not start or be observed")]
    Execution(#[from] BoundedError),
    #[error("ps child failed: {0:?}")]
    Child(PsFailure),
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
                Some(
                    crate::sys::errno::EPERM
                        | crate::sys::errno::EACCES
                        | crate::sys::errno::ENOENT
                )
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
    let code = if result.timed_out {
        Some("ETIMEDOUT")
    } else if result.output_truncated {
        Some("ENOBUFS")
    } else {
        None
    };
    if code.is_some() || result.returncode != 0 {
        return Err(PsError::Child(PsFailure {
            code,
            status: (result.returncode >= 0).then_some(result.returncode),
            signal: result
                .terminating_signal()
                .and_then(crate::sys::process::signal_name),
            stdout: result.output,
            stdout_bytes: result.output_bytes,
            stdout_truncated: result.output_truncated,
        }));
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
            matches!(PsPrograms { system: &failed, shim: &shim }.output(&[], &[]), Err(PsError::Child(PsFailure { status: Some(7), stdout, .. })) if stdout == "failure")
        );
        let missing = temp.path().join("missing");
        assert!(matches!(
            PsPrograms {
                system: &missing,
                shim: &failed
            }
            .output(&[], &[]),
            Err(PsError::Child(PsFailure {
                status: Some(7),
                ..
            }))
        ));
        let signal = script(temp.path(), "signal", "kill -TERM $$");
        assert!(matches!(
            PsPrograms {
                system: &signal,
                shim: &shim
            }
            .output(&[], &[]),
            Err(PsError::Child(PsFailure {
                status: None,
                signal: Some("SIGTERM"),
                ..
            }))
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
            Err(PsError::Child(PsFailure {
                code: Some("ENOBUFS"),
                ..
            }))
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
                Err(PsError::Child(failure)) => {
                    serde_json::json!({"status": failure.status, "stdout": failure.stdout})
                }
                Err(error) => panic!("unexpected ps oracle error: {error}"),
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
    #[test]
    fn differential_signal_and_timeout_retain_node_child_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("blocked-input");
        let made = crate::bounded::run_bounded(
            &["/usr/bin/mkfifo".into(), fifo.as_os_str().to_owned()],
            crate::bounded::Options::new(temp.path(), 5.0),
        )
        .unwrap();
        assert_eq!(made.returncode, 0);
        let signal = script(temp.path(), "signal", "printf before; kill -TERM $$");
        let command = crate::process_info::shell_join_os([
            std::ffi::OsStr::new("/bin/cat"),
            fifo.as_os_str(),
        ]);
        let timeout = script(
            temp.path(),
            "timeout",
            &format!("printf before; exec {}", command.to_str().unwrap()),
        );
        let paths = serde_json::to_string(&[&signal, &timeout]).unwrap();
        let js = format!(
            r#"
import {{ execFileSync }} from 'node:child_process';
const results = {paths}.map(path => {{
  try {{ return {{ok: execFileSync(path, [], {{encoding:'utf8',timeout:5000,stdio:['ignore','pipe','ignore'],env:process.env}})}}; }}
  catch (e) {{ return {{code:e.code || null,status:e.status,signal:e.signal,stdout:e.stdout}}; }}
}});
console.log(JSON.stringify(results));
"#
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::node(&root, &js) else {
            return;
        };
        let env = crate::test_support::environment();
        let actual: Vec<_> = [signal, timeout].iter().map(|system| {
            match (PsPrograms { system, shim: Path::new("/absent") }).output(&[], &env) {
                Err(PsError::Child(failure)) => serde_json::json!({"code":failure.code,"status":failure.status,"signal":failure.signal,"stdout":failure.stdout}),
                other => panic!("expected child failure, got {other:?}"),
            }
        }).collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}

#[cfg(test)]
mod platform_signal_tests {
    #[test]
    fn platform_signal_names_match_nodes_public_signal_constants() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::node(
            &root,
            r#"
import { constants } from 'node:os';
const names = process.platform === 'darwin' ? ['SIGINFO'] : ['SIGSTKFLT','SIGPWR'];
console.log(JSON.stringify(names.map(name => [constants.signals[name],name])));
"#,
        ) else {
            return;
        };
        let actual: Vec<_> = expected
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let signal = u32::try_from(row[0].as_u64().unwrap()).unwrap();
                serde_json::json!([signal, crate::sys::process::signal_name(signal)])
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
