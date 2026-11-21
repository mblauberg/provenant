//! The installed binary: parse the command line and dispatch it.
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

const INTERNAL_GIT_HOOKS_PATH: &str = "--__provenant_internal_git_hooks_path";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(OsStr::new(INTERNAL_GIT_HOOKS_PATH)) {
        return git_hooks_path_helper(args.collect());
    }
    let parsed = provenant_cli::parse(first.into_iter().chain(args));
    ExitCode::from(provenant::dispatch(parsed, &provenant::System))
}

/// Protocol used only by the generated Git wrapper. Exit 1 means the setting
/// is absent; every operational failure is distinct so the wrapper can avoid
/// retrying a command when it cannot establish whether hooks are configured.
fn git_hooks_path_helper(arguments: Vec<OsString>) -> ExitCode {
    use provenant_os::exec::Executor;

    let [include] = arguments.as_slice() else {
        return git_hooks_path_error();
    };
    let Some(git) = provenant_os::git::git_executable() else {
        return git_hooks_path_error();
    };
    let Ok(cwd) = std::env::current_dir() else {
        return git_hooks_path_error();
    };
    let context = provenant_os::git_config::ConfigRead {
        operation: "read Git hooks path",
        file: PathBuf::from("<effective Git configuration>"),
        key: Some("core.hooksPath"),
    };
    let read = provenant_os::git_config::read(
        context,
        provenant_os::git_config::Read::HooksPath,
        |tail| {
            let mut argv = vec![git.as_os_str().to_owned()];
            if !include.is_empty() {
                argv.extend([OsString::from("-c"), include.clone()]);
            }
            argv.extend_from_slice(tail);
            let executor = provenant_os::exec::SystemExecutor::new(1)?;
            let mut options =
                provenant_os::exec::ExecOptions::new(&argv, &cwd, Duration::from_secs(5));
            options.stdout = provenant_os::exec::StreamPolicy::Capture { limit: 4096 };
            options.stderr = provenant_os::exec::StreamPolicy::Discard;
            options.stop_on_output_limit = true;
            executor.execute(options)
        },
    );
    let Ok(output) = read else {
        return git_hooks_path_error();
    };
    let complete = !output.timed_out
        && !output.cancelled
        && !output.drain_truncated
        && !output.overflow_stopped
        && !output.stdout.truncated
        && !output.stdout.read_failed
        && output.stdout.total_bytes == output.stdout.bytes.len() as u64;
    if !complete {
        return git_hooks_path_error();
    }
    match output.termination {
        provenant_os::exec::Termination::Exit(0) => {
            if std::io::stdout()
                .lock()
                .write_all(&output.stdout.bytes)
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                git_hooks_path_error()
            }
        }
        provenant_os::exec::Termination::Exit(1) if output.stdout.bytes.is_empty() => {
            ExitCode::from(1)
        }
        _ => git_hooks_path_error(),
    }
}

fn git_hooks_path_error() -> ExitCode {
    let _ = std::io::stderr()
        .lock()
        .write_all(b"provenant: Git configuration could not be read\n");
    ExitCode::from(2)
}
