//! Anonymous, verified profile custody across both guardian handoffs. At the
//! final spawn edge a verified snapshot feeds an inherited read pipe: Darwin's
//! sandbox-exec rewrites regular-file /dev/fd names and cannot consume them.
use provenant_os::fs::Directory;
use provenant_os::sys::profile::{ProfilePipe, ProfileSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileIdentity {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug)]
pub struct ProfileFile {
    file: File,
    identity: ProfileIdentity,
}

pub(crate) fn failure(error: impl std::fmt::Display) -> io::Error {
    provenant_os::sys::profile::failure(error)
}

pub(crate) fn is_transport_error(error: &io::Error) -> bool {
    provenant_os::sys::profile::is_transport_error(error)
}

impl ProfileFile {
    /// Stage in a host-owned directory denied to lanes. The real pathname is
    /// used only to obtain a distinct O_RDONLY description, then removed. No
    /// writable description or pathname survives this constructor.
    pub fn create(bytes: &[u8], directory: &Directory) -> io::Result<Self> {
        let name = provenant_os::fs::temporary_name().map_err(failure)?;
        let mut writer = directory.create_new(&name).map_err(failure)?;
        let result = (|| {
            writer.write_all(bytes).map_err(failure)?;
            let reader = directory.open_read(&name).map_err(failure)?;
            if !reader
                .metadata()
                .map_err(failure)?
                .same_identity(&writer.metadata().map_err(failure)?)
            {
                return Err(failure("staged file identity changed"));
            }
            reader.handoff_file().try_clone().map_err(failure)
        })();
        let unlinked = directory.unlink(&name).map_err(failure);
        drop(writer);
        unlinked?;
        let file = result?;
        let metadata = file.metadata().map_err(failure)?;
        let identity = ProfileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: bytes.len() as u64,
            sha256: Sha256::digest(bytes).into(),
        };
        Self::inherit(file, identity)
    }

    pub(crate) fn inherit(file: File, identity: ProfileIdentity) -> io::Result<Self> {
        let profile = Self { file, identity };
        profile.verify()?;
        Ok(profile)
    }

    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn identity(&self) -> &ProfileIdentity {
        &self.identity
    }
    pub fn path(&self) -> OsString {
        format!("/dev/fd/{}", self.file.as_raw_fd()).into()
    }

    pub fn verify(&self) -> io::Result<()> {
        let metadata = self.file.metadata().map_err(failure)?;
        // SAFETY: F_GETFL observes the access flags of a borrowed live file.
        let flags = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(failure(io::Error::last_os_error()));
        }
        if !metadata.is_file()
            || metadata.nlink() != 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
            || metadata.dev() != self.identity.device
            || metadata.ino() != self.identity.inode
            || metadata.len() != self.identity.bytes
        {
            return Err(failure(
                "file is linked, writable or changed identity/length",
            ));
        }
        let mut hash = Sha256::new();
        let mut offset = 0;
        let mut buffer = [0_u8; 65536];
        loop {
            // Positional reads do not disturb other custody verifiers.
            let count = self.file.read_at(&mut buffer, offset).map_err(failure)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        let digest: [u8; 32] = hash.finalize().into();
        if offset != self.identity.bytes || digest != self.identity.sha256 {
            return Err(failure("file content changed after verification"));
        }
        // Keep the staged-file API repeatable for direct fixture readers too.
        (&self.file).seek(SeekFrom::Start(0)).map_err(failure)?;
        Ok(())
    }

    pub fn verify_command(&self, argv: &[OsString]) -> io::Result<()> {
        self.verify()?;
        if argv.len() < 4
            || argv[0] != "/usr/bin/sandbox-exec"
            || argv[1] != "-f"
            || argv[2] != self.path()
            || argv[3].is_empty()
            || argv[3].as_encoded_bytes().starts_with(b"-")
        {
            return Err(failure("command does not consume the held descriptor"));
        }
        Ok(())
    }
}

impl ProfileSource for ProfileFile {
    fn prepare(&self, argv: &[OsString]) -> io::Result<ProfilePipe> {
        self.verify_command(argv)?;
        let mut bytes = Vec::new();
        let mut offset = 0;
        let mut buffer = [0_u8; 65536];
        loop {
            let count = self.file.read_at(&mut buffer, offset).map_err(failure)?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            offset += count as u64;
        }
        // Hash the actual snapshot sent to the pipe, not just an earlier read.
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if offset != self.identity.bytes || digest != self.identity.sha256 {
            return Err(failure("profile snapshot changed before pipe transport"));
        }
        let pipe = ProfilePipe::new(bytes, argv)?;
        self.verify_pipe_command(&pipe, pipe.argv())?;
        Ok(pipe)
    }
}

impl ProfileFile {
    /// Final-form verification uses the prepared pipe's exact argv binding;
    /// accepting an arbitrary /dev/fd path here would lose custody.
    pub fn verify_pipe_command(&self, pipe: &ProfilePipe, argv: &[OsString]) -> io::Result<()> {
        self.verify()?;
        let digest: [u8; 32] = Sha256::digest(pipe.bytes()).into();
        if pipe.bytes().len() as u64 != self.identity.bytes || digest != self.identity.sha256 {
            return Err(failure("pipe snapshot does not match verified profile"));
        }
        pipe.verify_command(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::io::Read;

    // Medium: bind both forms to the verified bytes and the exact owned fd.
    #[test]
    fn profile_commands_bind_only_the_verified_source_and_pipe() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let directory = Directory::open_anchor(root.path())?;
        let profile = ProfileFile::create(b"(version 1)\n(allow default)\n", &directory)?;
        let argv = vec![
            "/usr/bin/sandbox-exec".into(),
            "-f".into(),
            profile.path(),
            "/usr/bin/true".into(),
        ];
        profile.verify_command(&argv)?;
        let pipe = profile.prepare(&argv)?;
        profile.verify_pipe_command(&pipe, pipe.argv())?;
        assert_ne!(pipe.argv()[2], profile.path());
        assert!(profile.verify_command(pipe.argv()).is_err());
        for (index, value) in [
            (0, "/bin/sh"),
            (1, "-p"),
            (2, "/dev/fd/999"),
            (2, "/dev/stdin"),
            (2, "/tmp/profile.sb"),
            (3, "-p"),
            (3, "-f"),
            (3, ""),
        ] {
            let mut changed = argv.clone();
            changed[index] = value.into();
            assert!(profile.verify_command(&changed).is_err());
            let mut changed = pipe.argv().to_vec();
            changed[index] = value.into();
            assert!(profile.verify_pipe_command(&pipe, &changed).is_err());
        }
        assert!(profile.verify_command(&argv[..3]).is_err());
        assert!(
            profile
                .verify_pipe_command(&pipe, &pipe.argv()[..3])
                .is_err()
        );
        let forged = ProfilePipe::new(b"(version 1)\n(allow default)\n;different".to_vec(), &argv)?;
        assert!(profile.verify_pipe_command(&forged, forged.argv()).is_err());
        Ok(())
    }

    // Medium: real filesystem custody and repeated pipe consumers; no Seatbelt.
    #[test]
    fn anonymous_profile_preserves_large_bytes_and_refuses_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = Directory::open_anchor(root.path())?;
        let mut bytes = vec![b';'; 1024 * 1024];
        bytes.extend_from_slice(b"\n(deny file-write* (subpath \"/mandatory-tail\"))\n");
        let profile = ProfileFile::create(&bytes, &directory)?;
        assert_eq!(profile.identity.bytes, bytes.len() as u64);
        assert_eq!(profile.file.metadata()?.nlink(), 0);
        assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
        #[cfg(target_os = "macos")]
        {
            for truncate in [false, true] {
                assert!(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(truncate)
                        .open(profile.path())
                        .is_err()
                );
            }
            assert!(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(profile.path())
                    .is_err()
            );
            assert!(profile.file.set_len(0).is_err());
            assert!((&profile.file).write_all(b"changed").is_err());
        }
        // Verification must leave the consumer's shared offset at the start.
        profile.verify()?;
        let mut applied = Vec::new();
        profile.file.try_clone()?.read_to_end(&mut applied)?;
        assert_eq!(applied, bytes);
        let identity = profile.identity().clone();
        for _ in 0..2 {
            let argv = vec![
                "/usr/bin/sandbox-exec".into(),
                "-f".into(),
                profile.path(),
                "/bin/cat".into(),
            ];
            let pipe = profile.prepare(&argv)?;
            let output = tempfile::tempfile()?;
            let mut command = std::process::Command::new("/bin/cat");
            command.arg(&pipe.argv()[2]).stdout(output.try_clone()?);
            provenant_os::sys::child::configure_command(&mut command, &[pipe.fd()], None, false)?;
            let mut child = command.spawn()?;
            pipe.finish(&mut child)?;
            assert!(child.wait()?.success());
            let mut applied = vec![0; bytes.len()];
            output.read_exact_at(&mut applied, 0)?;
            assert_eq!(output.metadata()?.len(), bytes.len() as u64);
            assert_eq!(applied, bytes);
            assert_eq!(profile.identity(), &identity);
        }
        std::fs::remove_dir(root.path())?;
        let error = ProfileFile::create(b"profile", &directory).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("sandbox profile transport refused")
        );
        Ok(())
    }

    // Medium: emulate an invalid sender retaining its staging writer. The
    // production constructor drops it before verification and never exposes it.
    #[test]
    fn changed_or_replaceable_profiles_are_refused() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = Directory::open_anchor(root.path())?;
        let name = OsStr::new("profile");
        let mut writer = directory.create_new(name)?;
        writer.write_all(b"deny")?;
        let reader = directory.open_read(name)?.handoff_file().try_clone()?;
        let metadata = reader.metadata()?;
        let identity = ProfileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: 4,
            sha256: Sha256::digest(b"deny").into(),
        };
        assert!(
            ProfileFile::inherit(reader.try_clone()?, identity.clone()).is_err(),
            "linked profile accepted"
        );
        directory.unlink(name)?;
        assert!(
            ProfileFile::inherit(writer.handoff_file().try_clone()?, identity.clone()).is_err(),
            "writable profile accepted"
        );
        let profile = ProfileFile::inherit(reader, identity)?;
        writer.handoff_file().write_all_at(b"open", 0)?;
        let error = profile.verify().unwrap_err();
        assert!(error.to_string().contains("file content changed"));
        assert!(
            profile
                .verify_command(&[
                    "/usr/bin/sandbox-exec".into(),
                    "-f".into(),
                    profile.path(),
                    "/usr/bin/touch".into()
                ])
                .is_err()
        );
        Ok(())
    }
}
