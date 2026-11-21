//! Shared construction of Git configuration reads.
use std::{ffi::OsString, fmt, path::PathBuf};

/// Context retained when a Git configuration read fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigRead {
    pub operation: &'static str,
    pub file: PathBuf,
    pub key: Option<&'static str>,
}

/// An execution failure annotated with the identity of the read.
#[derive(Debug)]
pub struct ConfigReadError<E> {
    pub context: ConfigRead,
    pub source: E,
}

impl<E: fmt::Display> fmt::Display for ConfigReadError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on {}",
            self.context.operation,
            provenant_core::bounded_line(&self.context.file.display().to_string(), 192)
        )?;
        if let Some(key) = self.context.key {
            write!(f, " for key {key}")?;
        }
        write!(
            f,
            ": {}",
            provenant_core::bounded_line(&self.source.to_string(), 192)
        )
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ConfigReadError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The supported config read shapes. Argument construction stays private.
pub enum Read<'a> {
    Listing,
    Directives,
    File(&'a std::path::Path),
    Get(&'a str),
    GetPath(&'a str),
    GetAfterOptions(&'a str),
    GetLocal(&'a str),
    GetRegexp(&'a str),
    Snapshot,
    LocalBareOrigins,
    UrlRewrites,
    HooksPath,
}

/// A config-read command constructed only by this module. Its fields and
/// constructor are private so a verified read-only runner cannot be given
/// arbitrary Git commands.
pub struct ReadCommand(Vec<OsString>);

impl AsRef<[OsString]> for ReadCommand {
    fn as_ref(&self) -> &[OsString] {
        &self.0
    }
}

impl std::ops::Deref for ReadCommand {
    type Target = [OsString];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a ReadCommand {
    type Item = &'a OsString;
    type IntoIter = std::slice::Iter<'a, OsString>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Execute a named config read using the caller's established process policy.
pub fn read<T, E>(
    context: ConfigRead,
    request: Read<'_>,
    run: impl FnOnce(&ReadCommand) -> Result<T, E>,
) -> Result<T, ConfigReadError<E>> {
    let mut args = vec![OsString::from("config")];
    match request {
        Read::Listing => {
            args.extend(["--includes", "--show-origin", "-z", "--list"].map(OsString::from))
        }
        Read::Directives => {
            args.extend(["--no-includes", "--show-origin", "-z", "--list"].map(OsString::from))
        }
        Read::File(path) => {
            args.push("--file".into());
            args.push(path.as_os_str().to_owned());
            args.extend(["--no-includes", "--show-origin", "-z", "--list"].map(OsString::from));
        }
        Read::Get(key) => args.extend(["--get", "--", key].map(OsString::from)),
        Read::GetPath(key) => args.extend(["--path", "--get", "--", key].map(OsString::from)),
        Read::GetAfterOptions(key) => args.extend(["--get", "--", key].map(OsString::from)),
        Read::GetLocal(key) => args.extend(["--local", "--get", "--", key].map(OsString::from)),
        Read::GetRegexp(pattern) => {
            args.extend(["--get-regexp", "--", pattern].map(OsString::from))
        }
        Read::HooksPath => args.extend(["--path", "--get", "core.hooksPath"].map(OsString::from)),
        Read::UrlRewrites => args.extend(
            [
                "--includes",
                "--null",
                "--get-regexp",
                r"^url\..*\.(insteadof|pushinsteadof)$",
            ]
            .map(OsString::from),
        ),
        Read::Snapshot => {
            args.extend(["--file", "-", "--no-includes", "--null", "--list"].map(OsString::from))
        }
        Read::LocalBareOrigins => args.extend(
            [
                "--local",
                "--includes",
                "--show-origin",
                "-z",
                "--bool",
                "--get-all",
                "core.bare",
            ]
            .map(OsString::from),
        ),
    }
    run(&ReadCommand(args)).map_err(|source| ConfigReadError { context, source })
}

/// Execute a configuration mutation through the caller's process policy.
pub fn write<T, E>(
    tail: &[OsString],
    run: impl FnOnce(&[OsString]) -> Result<T, E>,
) -> Result<T, E> {
    let mut args = vec![OsString::from("config")];
    args.extend_from_slice(tail);
    run(&args)
}

/// Read host identity values, distinguishing a missing key from a failed read.
#[allow(clippy::disallowed_methods)]
pub fn identity(
    context: ConfigRead,
    git: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<Vec<(OsString, OsString)>, ConfigReadError<std::io::Error>> {
    let mut env = Vec::new();
    for (key, author, committer) in [
        ("user.name", "GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"),
        ("user.email", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"),
    ] {
        let context = ConfigRead {
            key: Some(key),
            ..context.clone()
        };
        let value = read(context, Read::Get(key), |args| {
            let output = std::process::Command::new(git)
                .current_dir(cwd)
                .args(args)
                .output()?;
            if output.status.code() == Some(1) && output.stdout.is_empty() {
                return Ok(None);
            }
            if !output.status.success() {
                return Err(std::io::Error::other("Git config failed"));
            }
            let value = String::from_utf8(output.stdout)
                .map_err(|_| std::io::Error::other("Git config returned invalid UTF-8"))?;
            Ok(Some(value.strip_suffix('\n').unwrap_or(&value).to_owned()))
        })?;
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            env.push((author.into(), value.clone().into()));
            env.push((committer.into(), value.into()));
        }
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_preserves_context_and_source() {
        let context = ConfigRead {
            operation: "read identity",
            file: PathBuf::from("/repo"),
            key: Some("user.name"),
        };
        let error = read(context.clone(), Read::Get("user.name"), |_| {
            Err::<(), _>(std::io::Error::other("execution failed"))
        })
        .unwrap_err();
        assert_eq!(error.context, context);
        assert_eq!(error.source.kind(), std::io::ErrorKind::Other);
    }
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn option_shaped_key_cannot_become_a_git_option() {
        let context = ConfigRead {
            operation: "test config read",
            file: PathBuf::from("<environment>"),
            key: None,
        };
        let output = read(context, Read::Get("--list"), |args| {
            std::process::Command::new("/usr/bin/git")
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "test.visible")
                .env("GIT_CONFIG_VALUE_0", "sentinel")
                .args(args)
                .output()
        })
        .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("sentinel"));
    }
}
