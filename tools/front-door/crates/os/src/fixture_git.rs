//! Git commands for fixtures, independent of a writer lane's Git binding.
use std::{ffi::OsStr, path::Path, process::Command};

/// Remove all inherited Git settings before a fixture adds its own settings.
pub fn scrub_git_env(command: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
}

/// Spawn a specified native Git executable with no inherited Git settings.
#[allow(clippy::disallowed_methods)]
pub fn git_command_at(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    scrub_git_env(&mut command);
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

/// Select native Git directly, bypassing the lane's PATH wrapper.
pub fn git_command() -> Command {
    let program = crate::git::git_executable().unwrap_or_else(|| {
        [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/opt/local/bin/git",
            "/Library/Developer/CommandLineTools/usr/bin/git",
            "/usr/bin/git",
            "/bin/git",
        ]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .unwrap_or("/usr/bin/git")
        .into()
    });
    git_command_at(program)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_uses_resolved_git_when_available() {
        if let Some(git) = crate::git::git_executable() {
            assert_eq!(super::git_command().get_program(), git.as_os_str());
        }
    }
}
