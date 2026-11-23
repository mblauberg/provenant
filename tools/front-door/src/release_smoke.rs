//! Shared process-edge release smoke for installation and build packaging.
use provenant_config::{self as config, Release, ReleaseSmoke};
use provenant_core::CancelToken;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_records::{VersionInfo, wire::ENVELOPE_VERSION};
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

/// Validate the identity envelope against the shape the CLI spec defines.
/// Kept separate from the process invocation so a unit test can feed it the
/// real serialised `CommandResult` and catch envelope drift.
fn check_version_identity(version: &SmokeEnvelope<SmokeVersion>) -> io::Result<()> {
    if version.schema_version != ENVELOPE_VERSION
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
    Ok(())
}

impl SystemSmoke {
    /// Read the binary's own identity without relying on the packager's build stamp.
    pub(crate) fn identity(
        &self,
        executable: &Path,
        cancel: &CancelToken,
    ) -> io::Result<VersionInfo> {
        let version = self.invoke::<SmokeVersion>(executable, &["--version", "--json"], cancel)?;
        check_version_identity(&version)?;
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
        if help.schema_version != ENVELOPE_VERSION
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

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{CommandResult, Outcome, SourceDigest};
    use serde::Serialize;

    /// Mirrors the `BuildIdentity` the version command emits: `package_version`
    /// beside the flattened installation `VersionInfo`.
    #[derive(Serialize)]
    struct TestBuildIdentity {
        package_version: &'static str,
        #[serde(flatten)]
        installation: VersionInfo,
    }

    fn serialised_identity() -> Vec<u8> {
        let installation = VersionInfo {
            extra: Default::default(),
            source_digest: SourceDigest::from_bytes([0x11; 32]),
            asset_digest: SourceDigest::from_bytes([0x22; 32]),
            target: "x86_64-apple-darwin".to_owned(),
            toolchain: "stable".to_owned(),
            record_schema_version: 1,
            config_schema_version: config::CONFIG_SCHEMA_VERSION,
        };
        let data = TestBuildIdentity {
            package_version: env!("CARGO_PKG_VERSION"),
            installation,
        };
        serde_json::to_vec(&CommandResult::new("version", Outcome::Ok, data))
            .expect("serialise version envelope")
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope
    fn version_identity_check_accepts_the_real_serialised_envelope() {
        let bytes = serialised_identity();
        let envelope: SmokeEnvelope<SmokeVersion> =
            serde_json::from_slice(&bytes).expect("deserialise version envelope");
        assert_eq!(envelope.schema_version, ENVELOPE_VERSION);
        check_version_identity(&envelope).expect("the real envelope must pass the identity check");
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope
    fn version_identity_check_still_rejects_a_foreign_envelope_version() {
        let bytes = serialised_identity();
        let mut value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("deserialise envelope value");
        value["schema_version"] = serde_json::json!(ENVELOPE_VERSION + 1);
        let envelope: SmokeEnvelope<SmokeVersion> =
            serde_json::from_value(value).expect("deserialise tampered envelope");
        assert!(
            check_version_identity(&envelope).is_err(),
            "a foreign envelope version must still be rejected"
        );
    }
}
