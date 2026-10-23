//! Dev-only fixtures: dispatch scenario arguments before the test suite.
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Command;
pub fn environment(root: &Path) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ];
    for name in [
        "HOME",
        "PROVENANT_STATE_DIR",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "CURSOR_CONFIG_DIR",
        "AGY_HOME",
        "OPENCODE_CONFIG_DIR",
    ] {
        env.push((name.into(), root.join(name).into_os_string()));
    }
    env
}
/// Only self re-exec has this scoped lint allowance.
#[allow(clippy::disallowed_methods)]
pub fn command(scenario: &str, root: &Path) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--fixture")
        .arg(scenario)
        .env_clear()
        .envs(environment(root))
        .current_dir(root);
    Ok(command)
}
pub fn argv(scenario: &str) -> io::Result<Vec<OsString>> {
    Ok(vec![
        std::env::current_exe()?.into_os_string(),
        "--fixture".into(),
        scenario.into(),
    ])
}
pub fn dispatch() -> io::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != "--fixture") {
        return Ok(false);
    }
    let scenario = args
        .get(2)
        .and_then(|arg| arg.to_str())
        .ok_or(io::ErrorKind::InvalidInput)?;
    match scenario {
        "echo" => {
            io::copy(&mut io::stdin().lock(), &mut io::stdout().lock())?;
            io::stderr().write_all(b"done\r\n")?;
        }
        "bytes" => {
            io::stdout().write_all(b"a\0b\xff\nterminal\n")?;
            io::stderr().write_all(b"err\r\n")?;
        }
        "flood" => {
            for _ in 0..32 {
                io::stdout().write_all(&[b'x'; 8192])?;
                io::stderr().write_all(&[b'e'; 8192])?;
            }
        }
        "blocked" => {
            crate::sys::fixture_ignore_term()?;
            io::stdout().write_all(b"ready\n")?;
            let mut byte = [0];
            loop {
                if io::stdin().read(&mut byte)? == 0 {
                    std::thread::park_timeout(std::time::Duration::from_secs(1));
                }
            }
        }
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    }
    Ok(true)
}
