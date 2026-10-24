//! Input observations collected by existing readers, without additional I/O.
use provenant_records::{CanonicalV1, FileDigest};
use provenant_supervisor::fs::{FileKind, Metadata};
use std::path::Path;

#[derive(Default)]
pub(crate) struct InspectionEvidence {
    observations: Vec<[u8; 32]>,
}
impl InspectionEvidence {
    /// Hash length-framed native path bytes, the observation kind, presence,
    /// and exact content bytes. Raw bytes use file_digest, not record_digest.
    pub(crate) fn observe(&mut self, kind: &[u8], path: &Path, bytes: Option<&[u8]>) {
        let mut frame = b"provenant-check-observation-v1\0".to_vec();
        for part in [kind, path.as_os_str().as_encoded_bytes()] {
            frame.extend_from_slice(&(part.len() as u64).to_be_bytes());
            frame.extend_from_slice(part);
        }
        frame.push(u8::from(bytes.is_some()));
        if let Some(bytes) = bytes {
            frame.extend_from_slice(CanonicalV1::file_digest(bytes).bytes());
        }
        self.observations
            .push(*CanonicalV1::file_digest(&frame).bytes());
    }
    pub(crate) fn metadata(&mut self, path: &Path, metadata: Metadata) {
        let mut bytes = vec![match metadata.kind {
            FileKind::Regular => 0,
            FileKind::Directory => 1,
            FileKind::Symlink => 2,
            FileKind::Other => 3,
        }];
        for value in [
            metadata.device,
            metadata.inode,
            metadata.links,
            metadata.size,
            metadata.allocated,
            u64::from(metadata.uid),
            u64::from(metadata.mode),
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        // Filesystem timestamps are already i128; preserve their bytes rather
        // than passing an out-of-range JSON integer to CanonicalV1.
        bytes.extend_from_slice(&metadata.modified_ns.to_be_bytes());
        self.observe(b"metadata", path, Some(&bytes));
    }
    pub(crate) fn finish(mut self) -> FileDigest {
        self.observations.sort();
        let mut bytes = b"provenant-check-inputs-v1\0".to_vec();
        for digest in self.observations {
            bytes.extend_from_slice(&digest);
        }
        CanonicalV1::file_digest(&bytes)
    }
}
