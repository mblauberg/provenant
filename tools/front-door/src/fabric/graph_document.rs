//! Private, content-addressed publication of compiled graph documents.
pub(crate) fn publish_graph_document(
    scratch: &std::path::Path,
    document: &serde_json::Value,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use provenant_os::fs::{Directory, PublishMode};
    use std::ffi::OsStr;
    let bytes = serde_json::to_vec(document)?;
    let digest = provenant_records::CanonicalV1::file_digest(&bytes).to_string();
    let filename = format!("graph-{}.json", &digest[7..]);
    let graphs = scratch.join("graphs");
    let dir = Directory::open_anchor(&graphs)?;
    let uid = provenant_os::fs::effective_uid();
    let mut directory_metadata = dir.metadata()?;
    if directory_metadata.uid == uid
        && directory_metadata.mode & 0o077 != 0
        && dir.make_private().is_ok()
    {
        directory_metadata = dir.metadata()?;
    }
    if let Some(refusal) = private_graph_directory_refusal(
        &graphs,
        directory_metadata.uid,
        directory_metadata.mode,
        uid,
    ) {
        return Err(refusal.into());
    }
    match dir.atomic_write(
        OsStr::new(&filename),
        ".graph-",
        &bytes,
        PublishMode::NoReplace,
    ) {
        Ok(publication) => publication.require_durable()?,
        Err(error) if error.is_exists() => {
            let metadata = dir.metadata_at(OsStr::new(&filename), false)?;
            if metadata.uid != provenant_os::fs::effective_uid()
                || metadata.mode & 0o077 != 0
                || metadata.links != 1
                || dir.read_stable(
                    OsStr::new(&filename),
                    provenant_fabric_runtime::config::GraphLimits::document_bytes_bound()
                        .maximum_u64(),
                )? != bytes
            {
                return Err(
                    "existing graph document is not private or does not match its content hash"
                        .into(),
                );
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(scratch.join("graphs").join(filename))
}

/// `None` when the graph directory is owned by `current` and private (mode 0700
/// after any repair). A foreign owner is told to remove or take over the
/// directory; an owned one is told the observed mode and `chmod 700`.
fn private_graph_directory_refusal(
    path: &std::path::Path,
    owner: u32,
    mode: u32,
    current: u32,
) -> Option<String> {
    if owner != current {
        return Some(format!(
            "graph document directory {} is owned by uid {owner}, not the current user; remove it or chown it to yourself",
            path.display()
        ));
    }
    (mode & 0o077 != 0).then(|| {
        format!(
            "graph document directory {} has mode {:04o} and is not private to the current user; run `chmod 700 {}`",
            path.display(),
            mode & 0o7777,
            path.display()
        )
    })
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree driver includes the library without libtest"
)]
mod tests {
    use super::*;
    #[test]
    fn graph_publication_is_private_and_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let graphs = temp.path().join("graphs");
        std::fs::create_dir(&graphs)?;
        std::fs::set_permissions(&graphs, std::fs::Permissions::from_mode(0o700))?;
        let graph = serde_json::json!({"graph_id":"test","nodes":[{"id":"task","access":"read_only","prompt":"brief"}]});
        let first = publish_graph_document(temp.path(), &graph)?;
        let second = publish_graph_document(temp.path(), &graph)?;
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&first)?, serde_json::to_vec(&graph)?);
        assert_eq!(std::fs::metadata(&first)?.permissions().mode() & 0o077, 0);
        std::fs::write(&first, b"different")?;
        assert!(publish_graph_document(temp.path(), &graph).is_err());
        std::fs::write(&first, serde_json::to_vec(&graph)?)?;
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o644))?;
        assert!(publish_graph_document(temp.path(), &graph).is_err());
        Ok(())
    }

    #[test]
    fn graph_publication_repairs_owned_loose_directory() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let graphs = temp.path().join("graphs");
        std::fs::create_dir(&graphs)?;
        std::fs::set_permissions(&graphs, std::fs::Permissions::from_mode(0o755))?;
        let graph = serde_json::json!({"graph_id":"test","nodes":[{"id":"task","access":"read_only","prompt":"brief"}]});
        let published = publish_graph_document(temp.path(), &graph)?;
        assert_eq!(
            std::fs::metadata(&graphs)?.permissions().mode() & 0o777,
            0o700
        );
        assert!(published.starts_with(&graphs));
        assert_eq!(std::fs::read(&published)?, serde_json::to_vec(&graph)?);
        Ok(())
    }
}
