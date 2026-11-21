//! Shared construction of Git configuration commands. Reads and mutations are
//! both built here: callers supply a named context and a typed mutation, never
//! a raw config argv tail.
use std::{ffi::OsString, fmt, path::PathBuf};

/// The identity of one Git configuration command, read or write. Every config
/// command takes one, and builds every failure through it, so a refusal names
/// the operation and a static key class — never a value from a config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigContext {
    /// Stable label for the command.
    pub operation: &'static str,
    /// The config file or directory being inspected or changed.
    pub file: PathBuf,
    /// Static key or directive label, never a value from the config file.
    pub key: Option<&'static str>,
}

impl ConfigContext {
    pub fn new(
        operation: &'static str,
        file: impl Into<PathBuf>,
        key: Option<&'static str>,
    ) -> Self {
        Self {
            operation,
            file: file.into(),
            key,
        }
    }

    /// The same operation narrowed to one static key label.
    pub fn with_key(&self, key: &'static str) -> Self {
        Self {
            key: Some(key),
            ..self.clone()
        }
    }
}

impl fmt::Display for ConfigContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on {}",
            self.operation,
            provenant_core::bounded_line(&self.file.display().to_string(), 192)
        )?;
        if let Some(key) = self.key {
            write!(f, " for key {key}")?;
        }
        Ok(())
    }
}

/// The one error type for a failed Git configuration command. Its `Display`
/// names only the context; the underlying failure is reached through
/// `std::error::Error::source`, so printing an error chain shows the cause once.
#[derive(Debug)]
pub struct ConfigError<E> {
    pub context: ConfigContext,
    pub source: E,
}

impl<E: fmt::Display> fmt::Display for ConfigError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.context.fmt(f)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ConfigError<E> {
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
    GetRegexpLocal(&'a str),
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
    context: ConfigContext,
    request: Read<'_>,
    run: impl FnOnce(&ReadCommand) -> Result<T, E>,
) -> Result<T, ConfigError<E>> {
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
        Read::GetRegexpLocal(pattern) => {
            args.extend(["--local", "--get-regexp", "--", pattern].map(OsString::from))
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
    run(&ReadCommand(args)).map_err(|source| ConfigError { context, source })
}

/// The settings Provenant writes in a bare store or slot repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSetting {
    HooksPath,
    GcAuto,
    ReceiveDenyDeletes,
}

impl StoreSetting {
    pub fn key(self) -> &'static str {
        match self {
            Self::HooksPath => "core.hooksPath",
            Self::GcAuto => "gc.auto",
            Self::ReceiveDenyDeletes => "receive.denyDeletes",
        }
    }
}

/// A branch tracking field Provenant writes during settlement and landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchField {
    Remote,
    Merge,
    PushRemote,
}

impl BranchField {
    fn key(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Merge => "merge",
            Self::PushRemote => "pushRemote",
        }
    }
}

/// The two identity keys Provenant writes when recording a provider fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityField {
    Name,
    Email,
}

impl IdentityField {
    fn key(self) -> &'static str {
        match self {
            Self::Name => "user.name",
            Self::Email => "user.email",
        }
    }
}

/// Every Git config mutation Provenant performs. A caller cannot name an
/// option or an arbitrary key: the core owns the argv and the scope flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigMutation {
    /// `git config <key> <value>` in a bare store or slot repository.
    SetStore {
        setting: StoreSetting,
        value: String,
    },
    /// `git config --local [--replace-all] branch.<branch>.<field> <value>`.
    SetBranchTracking {
        branch: String,
        field: BranchField,
        value: String,
        replace: bool,
    },
    /// `git config --remove-section branch.<branch>`.
    RemoveBranchSection { branch: String },
    /// `git config <user.name|user.email> <value>` in a fixture repository.
    SetIdentity { field: IdentityField, value: String },
}

/// A config-write command constructed only by this module. Its fields and
/// constructor are private, so no code outside the core can issue a config
/// write with an argv it assembled itself.
pub struct WriteCommand(Vec<OsString>);

impl AsRef<[OsString]> for WriteCommand {
    fn as_ref(&self) -> &[OsString] {
        &self.0
    }
}

impl std::ops::Deref for WriteCommand {
    type Target = [OsString];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a WriteCommand {
    type Item = &'a OsString;
    type IntoIter = std::slice::Iter<'a, OsString>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Build one typed config mutation and run it through the caller's process
/// policy. The argv is private to this module; the only public input is the
/// mutation enum.
pub fn write<T, E>(
    context: ConfigContext,
    mutation: ConfigMutation,
    run: impl FnOnce(&WriteCommand) -> Result<T, E>,
) -> Result<T, ConfigError<E>> {
    let mut args = vec![OsString::from("config")];
    match mutation {
        ConfigMutation::SetStore { setting, value } => {
            args.extend(["--", setting.key(), &value].map(OsString::from))
        }
        ConfigMutation::SetBranchTracking {
            branch,
            field,
            value,
            replace,
        } => {
            args.push("--local".into());
            if replace {
                args.push("--replace-all".into());
            }
            args.extend([
                OsString::from("--"),
                OsString::from(format!("branch.{branch}.{}", field.key())),
                OsString::from(value),
            ]);
        }
        ConfigMutation::RemoveBranchSection { branch } => {
            args.extend([
                OsString::from("--remove-section"),
                OsString::from("--"),
                OsString::from(format!("branch.{branch}")),
            ]);
        }
        ConfigMutation::SetIdentity { field, value } => {
            args.extend(["--", field.key(), &value].map(OsString::from))
        }
    }
    run(&WriteCommand(args)).map_err(|source| ConfigError { context, source })
}

/// Read host identity values, distinguishing a missing key from a failed read.
#[allow(clippy::disallowed_methods)]
pub fn identity(
    context: ConfigContext,
    git: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<Vec<(OsString, OsString)>, ConfigError<std::io::Error>> {
    let mut env = Vec::new();
    for (key, author, committer) in [
        ("user.name", "GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"),
        ("user.email", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"),
    ] {
        let context = context.with_key(key);
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
        let context = ConfigContext::new("read identity", "/repo", Some("user.name"));
        let error = read(context.clone(), Read::Get("user.name"), |_| {
            Err::<(), _>(std::io::Error::other("execution failed"))
        })
        .unwrap_err();
        assert_eq!(error.context, context);
        assert_eq!(error.source.kind(), std::io::ErrorKind::Other);
    }

    #[test]
    fn error_display_omits_the_source_the_chain_reports() {
        let error = ConfigError {
            context: ConfigContext::new(
                "set branch tracking",
                "/repo/.git/config",
                Some("branch tracking"),
            ),
            source: std::io::Error::other("cause marker"),
        };
        assert_eq!(
            error.to_string(),
            "set branch tracking on /repo/.git/config for key branch tracking"
        );
        let mut rendered = error.to_string();
        let mut next = std::error::Error::source(&error);
        while let Some(source) = next {
            rendered.push_str(&source.to_string());
            next = source.source();
        }
        assert_eq!(rendered.matches("cause marker").count(), 1);
    }

    #[test]
    fn every_typed_mutation_builds_only_its_declared_argv() {
        let cases: Vec<(ConfigMutation, Vec<&str>)> = vec![
            (
                ConfigMutation::SetStore {
                    setting: StoreSetting::HooksPath,
                    value: "/dev/null".into(),
                },
                vec!["config", "--", "core.hooksPath", "/dev/null"],
            ),
            (
                ConfigMutation::SetStore {
                    setting: StoreSetting::GcAuto,
                    value: "0".into(),
                },
                vec!["config", "--", "gc.auto", "0"],
            ),
            (
                ConfigMutation::SetStore {
                    setting: StoreSetting::ReceiveDenyDeletes,
                    value: "false".into(),
                },
                vec!["config", "--", "receive.denyDeletes", "false"],
            ),
            (
                ConfigMutation::SetBranchTracking {
                    branch: "lane".into(),
                    field: BranchField::Remote,
                    value: "origin".into(),
                    replace: true,
                },
                vec![
                    "config",
                    "--local",
                    "--replace-all",
                    "--",
                    "branch.lane.remote",
                    "origin",
                ],
            ),
            (
                ConfigMutation::SetBranchTracking {
                    branch: "lane".into(),
                    field: BranchField::Merge,
                    value: "refs/heads/main".into(),
                    replace: false,
                },
                vec![
                    "config",
                    "--local",
                    "--",
                    "branch.lane.merge",
                    "refs/heads/main",
                ],
            ),
            (
                ConfigMutation::SetBranchTracking {
                    branch: "lane".into(),
                    field: BranchField::PushRemote,
                    value: "origin".into(),
                    replace: false,
                },
                vec![
                    "config",
                    "--local",
                    "--",
                    "branch.lane.pushRemote",
                    "origin",
                ],
            ),
            (
                ConfigMutation::RemoveBranchSection {
                    branch: "lane".into(),
                },
                vec!["config", "--remove-section", "--", "branch.lane"],
            ),
            (
                ConfigMutation::SetIdentity {
                    field: IdentityField::Name,
                    value: "Probe".into(),
                },
                vec!["config", "--", "user.name", "Probe"],
            ),
            (
                ConfigMutation::SetIdentity {
                    field: IdentityField::Email,
                    value: "probe@example.invalid".into(),
                },
                vec!["config", "--", "user.email", "probe@example.invalid"],
            ),
        ];
        for (mutation, expected) in cases {
            let argv = write(
                ConfigContext::new("typed mutation", "/repo/.git/config", None),
                mutation.clone(),
                |command| {
                    Ok::<_, ()>(
                        command
                            .iter()
                            .map(|arg| arg.to_string_lossy().into_owned())
                            .collect::<Vec<_>>(),
                    )
                },
            )
            .unwrap();
            assert_eq!(argv, expected, "{mutation:?}");
        }
    }

    #[test]
    fn option_shaped_key_cannot_become_a_git_option() {
        let context = ConfigContext::new("test config read", "<environment>", None);
        let output = read(context, Read::Get("--list"), |args| {
            crate::fixture::git_command_at("/usr/bin/git")
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
