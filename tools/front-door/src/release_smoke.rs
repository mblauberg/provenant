//! Shared process-edge release smoke for installation and build packaging.
use provenant_config::{self as config, Release, ReleaseSmoke};
use provenant_core::CancelToken;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_records::VersionInfo;
use serde::{Deserialize, de::DeserializeOwned};
use std::{ffi::OsString, io, path::Path, time::Duration};

#[derive(Deserialize)]
struct SmokeVersion {
    package_version: String,
    source_digest: String,
    asset_digest: String,
    target: String,
    toolchain: String,
    record_schema_version: u64,
    config_schema_version: u64,
}
#[derive(Deserialize)]
struct SmokeEnvelope<T> {
    schema_version: u64,
    command: String,
    status: String,
    data: T,
    error: Option<serde_json::Value>,
}

pub(crate) struct SystemSmoke {
    executor: SystemExecutor,
    environment: Vec<(OsString, OsString)>,
}
impl SystemSmoke {
    pub(crate) fn new(home: Option<&Path>) -> io::Result<Self> {
        let executor = SystemExecutor::new(1)
            .map_err(|_| io::Error::other("cannot start bounded release smoke executor"))?;
        Ok(Self {
            executor,
            environment: home.map_or_else(Vec::new, |home| {
                [
                    "HOME",
                    "XDG_CONFIG_HOME",
                    "CLAUDE_CONFIG_DIR",
                    "CODEX_HOME",
                    "PROVENANT_STATE_DIR",
                ]
                .into_iter()
                .map(|key| (key.into(), home.as_os_str().to_owned()))
                .collect()
            }),
        })
    }
    fn invoke<T: DeserializeOwned>(
        &self,
        executable: &Path,
        args: &[&str],
        cancel: &CancelToken,
    ) -> io::Result<SmokeEnvelope<T>> {
        let mut argv = vec![executable.as_os_str().to_owned()];
        argv.extend(args.iter().map(OsString::from));
        let mut options = ExecOptions::new(
            &argv,
            executable
                .parent()
                .ok_or_else(|| io::Error::other("release binary has no parent"))?,
            Duration::from_secs(30),
        );
        options.env = Some(&self.environment);
        options.stdout = StreamPolicy::Capture { limit: 1_048_576 };
        options.stderr = StreamPolicy::Discard;
        options.cancel = Some(cancel);
        options.stop_on_output_limit = true;
        let result = self
            .executor
            .execute(options)
            .map_err(|_| io::Error::other("bounded release smoke command failed"))?;
        if result.termination != Termination::Exit(0)
            || result.timed_out
            || result.drain_truncated
            || result.stdout.truncated
            || result.stdout.read_failed
            || result.cancelled
        {
            return Err(io::Error::other(
                "release smoke command returned an invalid result",
            ));
        }
        provenant_records::decode(
            result.stdout.bytes.as_slice(),
            provenant_records::Limits::CONFIG,
        )
        .map_err(|_| io::Error::other("release smoke output is not a valid result envelope"))
    }
}
impl SystemSmoke {
    /// Read the binary's own identity without relying on the packager's build stamp.
    pub(crate) fn identity(
        &self,
        executable: &Path,
        cancel: &CancelToken,
    ) -> io::Result<VersionInfo> {
        let version = self.invoke::<SmokeVersion>(executable, &["--version", "--json"], cancel)?;
        if version.schema_version != 1
            || version.command != "version"
            || version.status != "ok"
            || version.error.is_some()
            || version.data.package_version != env!("CARGO_PKG_VERSION")
            || version.data.record_schema_version != 1
            || version.data.config_schema_version != config::CONFIG_SCHEMA_VERSION
        {
            return Err(io::Error::other(
                "release version identity envelope differs",
            ));
        }
        let data = version.data;
        Ok(VersionInfo {
            extra: Default::default(),
            source_digest: provenant_records::SourceDigest::parse(&data.source_digest)
                .map_err(|_| io::Error::other("release source digest is invalid"))?,
            asset_digest: provenant_records::SourceDigest::parse(&data.asset_digest)
                .map_err(|_| io::Error::other("release asset digest is invalid"))?,
            target: data.target,
            toolchain: data.toolchain,
            record_schema_version: data.record_schema_version,
            config_schema_version: data.config_schema_version,
        })
    }
}
impl ReleaseSmoke for SystemSmoke {
    fn check(&self, release: &Release, cancel: &CancelToken) -> io::Result<VersionInfo> {
        let executable = release.source().join("bin/provenant");
        let identity = self.identity(&executable, cancel)?;
        let help = self.invoke::<serde_json::Value>(&executable, &["--help", "--json"], cancel)?;
        if help.schema_version != 1
            || help.command != "help"
            || help.status != "ok"
            || help.error.is_some()
            || !help.data.is_object()
        {
            return Err(io::Error::other("release help identity envelope differs"));
        }
        Ok(identity)
    }
}
