use provenant_records::{SourceDigest, SourceInput, source_digest};
use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, Item};

type BuildResult<T> = Result<T, Box<dyn Error>>;

const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_FILES: usize = 100_000;
const MAX_SOURCE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

struct InputFile {
    name: String,
    bytes: Vec<u8>,
    executable: bool,
}

#[derive(Default)]
struct Inputs {
    files: Vec<InputFile>,
    total_bytes: u64,
}

impl Inputs {
    fn get(&self, name: &str) -> Option<&Vec<u8>> {
        self.files
            .iter()
            .find(|file| file.name == name)
            .map(|file| &file.bytes)
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &Vec<u8>, bool)> {
        self.files
            .iter()
            .map(|file| (&file.name, &file.bytes, file.executable))
    }

    fn read_file(&mut self, path: &Path, name: String, metadata: &fs::Metadata) -> BuildResult<()> {
        if self.files.len() >= MAX_SOURCE_FILES {
            return Err(invalid("source inventory exceeds the file-count limit").into());
        }

        let remaining = MAX_SOURCE_TOTAL_BYTES
            .checked_sub(self.total_bytes)
            .ok_or_else(|| invalid("source inventory exceeds the total-byte limit"))?;
        let limit = MAX_SOURCE_FILE_BYTES.min(remaining);
        let read_limit = limit
            .checked_add(1)
            .ok_or_else(|| invalid("source inventory byte limit overflowed"))?;
        let executable = metadata.permissions().mode() & 0o111 != 0;
        let file = fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(read_limit).read_to_end(&mut bytes)?;
        let byte_count = u64::try_from(bytes.len())
            .map_err(|_| invalid("source inventory input is too large"))?;
        if byte_count > limit {
            return Err(invalid("source inventory exceeds a file or total-byte limit").into());
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(byte_count)
            .ok_or_else(|| invalid("source inventory total-byte count overflowed"))?;
        self.files.push(InputFile {
            name,
            bytes,
            executable,
        });
        Ok(())
    }
}

struct TrackedFiles {
    repository: PathBuf,
    names: BTreeSet<Vec<u8>>,
}

impl TrackedFiles {
    fn load(workspace: &Path, repository: &Path) -> BuildResult<Self> {
        let top_level = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(workspace)
            .output()?;
        if !top_level.status.success() {
            return Err(invalid("source inventory requires a git checkout").into());
        }
        let top_level = top_level
            .stdout
            .strip_suffix(b"\n")
            .ok_or_else(|| invalid("git did not return a repository root"))?;
        let top_level = fs::canonicalize(std::str::from_utf8(top_level)?)?;
        if top_level != repository {
            return Err(invalid("source workspace is outside its expected git checkout").into());
        }

        let output = Command::new("git")
            .args(["ls-files", "--full-name", "-z", "--", ":/"])
            .current_dir(workspace)
            .output()?;
        if !output.status.success() {
            return Err(invalid("cannot list tracked source files").into());
        }
        let names = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        Ok(Self {
            repository: repository.to_path_buf(),
            names,
        })
    }

    fn contains(&self, path: &Path) -> BuildResult<bool> {
        let relative = path
            .strip_prefix(&self.repository)
            .map_err(|_| invalid("source inventory path escapes its repository"))?;
        Ok(self.names.contains(relative.as_os_str().as_bytes()))
    }

    fn has_descendant(&self, path: &Path) -> BuildResult<bool> {
        let relative = path
            .strip_prefix(&self.repository)
            .map_err(|_| invalid("source inventory path escapes its repository"))?;
        let mut prefix = relative.as_os_str().as_bytes().to_vec();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        Ok(self
            .names
            .iter()
            .any(|tracked| tracked.starts_with(&prefix)))
    }
}

#[derive(Clone, Debug)]
struct Member {
    directory: PathBuf,
    logical_directory: String,
}

fn main() -> BuildResult<()> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("Cargo did not provide CARGO_MANIFEST_DIR"))?;
    let workspace = fs::canonicalize(manifest_dir)?;
    let repository = workspace
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("workspace is not nested below the repository root"))?;
    let repository = fs::canonicalize(repository)?;
    let tracked = TrackedFiles::load(&workspace, &repository)?;

    let mut watched = BTreeSet::new();
    let mut inputs = Inputs::default();

    let workspace_manifest = workspace.join("Cargo.toml");
    watch(&mut watched, workspace_manifest.clone())?;
    add_required_file(
        &workspace,
        &workspace_manifest,
        "Cargo.toml".to_owned(),
        &tracked,
        &mut inputs,
    )?;
    let workspace_document = parse_toml(
        inputs
            .get("Cargo.toml")
            .ok_or_else(|| invalid("workspace manifest was not inventoried"))?,
        "workspace Cargo.toml",
    )?;

    let lockfile = workspace.join("Cargo.lock");
    watch(&mut watched, lockfile.clone())?;
    add_required_file(
        &workspace,
        &lockfile,
        "Cargo.lock".to_owned(),
        &tracked,
        &mut inputs,
    )?;

    let members = discover_members(&workspace, &workspace_document, &mut watched, &tracked)?;
    for member in &members {
        let manifest = member.directory.join("Cargo.toml");
        let logical_manifest = join_logical(&member.logical_directory, "Cargo.toml");
        watch(&mut watched, manifest.clone())?;
        if logical_manifest != "Cargo.toml" {
            add_required_file(
                &workspace,
                &manifest,
                logical_manifest,
                &tracked,
                &mut inputs,
            )?;
        }

        let build_script = member.directory.join("build.rs");
        let logical_build_script = join_logical(&member.logical_directory, "build.rs");
        watch(&mut watched, build_script.clone())?;
        add_optional_file(
            &workspace,
            &build_script,
            logical_build_script,
            &tracked,
            &mut inputs,
        )?;

        for source_directory in ["src", "tests", "examples", "benches", "assets"] {
            let directory = member.directory.join(source_directory);
            watch(&mut watched, directory.clone())?;
            collect_workspace_tree(
                &workspace,
                &directory,
                &mut inputs,
                source_directory == "src",
                &tracked,
            )?;
        }
    }

    // External product inputs are assigned their release asset names so the
    // pure digest API only sees normal relative POSIX paths (never ../ paths).
    for (source, release_name) in [
        ("skills", "skills"),
        ("library", "library"),
        ("templates", "templates"),
        ("config", "config"),
        ("scripts", "scripts"),
        ("runtime", "runtime"),
    ] {
        let directory = repository.join(source);
        watch(&mut watched, directory.clone())?;
        let required = source != "templates";
        collect_external_tree(&directory, release_name, &mut inputs, required, &tracked)?;
    }
    for (source, release_name) in [
        ("AGENTS.md", "instructions/AGENTS.md"),
        ("HARNESS.md", "instructions/HARNESS.md"),
    ] {
        let path = repository.join(source);
        watch(&mut watched, path.clone())?;
        add_required_file(
            &repository,
            &path,
            release_name.to_owned(),
            &tracked,
            &mut inputs,
        )?;
    }

    add_workspace_configuration(&workspace, &repository, &mut watched, &tracked, &mut inputs)?;
    let toolchain = pinned_toolchain(&workspace, &repository, &mut watched, &tracked, &mut inputs)?;
    let target = env::var("TARGET")
        .map_err(|_| invalid("Cargo did not provide TARGET to the build script"))?;
    validate_metadata_value(&target)?;
    validate_metadata_value(&toolchain)?;

    let encoded_assets = release_asset_inputs(&inputs)?;
    let borrowed_assets: Vec<SourceInput<'_>> = encoded_assets
        .iter()
        .map(|(name, bytes)| SourceInput { name, bytes })
        .collect();
    let asset_stamp = source_digest(&borrowed_assets)?;
    let asset_stamp_text = asset_stamp.to_string();
    let mut borrowed_inputs: Vec<SourceInput<'_>> = inputs
        .iter()
        .map(|(name, bytes, _)| SourceInput { name, bytes })
        .collect();
    borrowed_inputs.push(SourceInput {
        name: "provenant-release/asset-digest",
        bytes: asset_stamp_text.as_bytes(),
    });
    let source_stamp: SourceDigest = source_digest(&borrowed_inputs)?;

    for path in watched {
        emit_path_instruction("rerun-if-changed", &path)?;
    }
    emit_instruction(
        "rustc-env",
        &format!("PROVENANT_SOURCE_DIGEST={source_stamp}"),
    )?;
    emit_instruction(
        "rustc-env",
        &format!("PROVENANT_ASSET_DIGEST={asset_stamp}"),
    )?;
    emit_instruction("rustc-env", &format!("PROVENANT_TARGET_TRIPLE={target}"))?;
    emit_instruction("rustc-env", &format!("PROVENANT_TOOLCHAIN={toolchain}"))?;
    Ok(())
}

fn release_asset_inputs(inputs: &Inputs) -> BuildResult<Vec<(String, Vec<u8>)>> {
    let mut result = Vec::new();
    for (name, bytes, executable) in inputs.iter() {
        if [
            "skills/",
            "library/",
            "templates/",
            "config/",
            "instructions/",
            "scripts/",
            "runtime/",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        {
            let mut fingerprint = Vec::with_capacity(bytes.len() + 1);
            fingerprint.push(u8::from(executable));
            fingerprint.extend_from_slice(bytes);
            result.push((name.clone(), fingerprint));
        }
    }
    if result.is_empty() {
        return Err(invalid("release asset inventory is empty").into());
    }
    Ok(result)
}

fn discover_members(
    workspace: &Path,
    manifest: &DocumentMut,
    watched: &mut BTreeSet<PathBuf>,
    tracked: &TrackedFiles,
) -> BuildResult<Vec<Member>> {
    let members = manifest
        .as_table()
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|workspace| workspace.get("members"))
        .and_then(Item::as_array)
        .ok_or_else(|| invalid("workspace manifest has no string members array"))?;
    let mut result = Vec::new();

    for member in members.iter() {
        let pattern = member
            .as_str()
            .ok_or_else(|| invalid("workspace member pattern is not a string"))?;
        match pattern {
            "." => {
                result.push(Member {
                    directory: workspace.to_path_buf(),
                    logical_directory: String::new(),
                });
            }
            "crates/*" => {
                let crates = workspace.join("crates");
                watch(watched, crates.clone())?;
                let metadata = optional_metadata(&crates)?
                    .ok_or_else(|| invalid("workspace crates directory is missing"))?;
                if !metadata.is_dir() {
                    return Err(invalid("workspace crates path is not a directory").into());
                }
                for child in fs::read_dir(&crates)? {
                    let child = child?;
                    let kind = child.file_type()?;
                    if kind.is_symlink() {
                        if tracked.contains(&child.path())?
                            || tracked.has_descendant(&child.path())?
                        {
                            return Err(invalid("workspace member path is a symlink").into());
                        }
                        continue;
                    }
                    if !kind.is_dir() {
                        continue;
                    }
                    let directory = child.path();
                    if !tracked.contains(&directory.join("Cargo.toml"))? {
                        continue;
                    }
                    let name = child
                        .file_name()
                        .into_string()
                        .map_err(|_| invalid("workspace member name is not UTF-8"))?;
                    let logical_directory = format!("crates/{name}");
                    result.push(Member {
                        directory,
                        logical_directory,
                    });
                }
            }
            _ => {
                return Err(invalid(
                    "workspace member pattern is outside the build stamp's supported patterns",
                )
                .into());
            }
        }
    }

    if result.is_empty() {
        return Err(invalid("workspace has no package members").into());
    }
    result.sort_by(|left, right| left.logical_directory.cmp(&right.logical_directory));
    Ok(result)
}

fn add_workspace_configuration(
    workspace: &Path,
    repository: &Path,
    watched: &mut BTreeSet<PathBuf>,
    tracked: &TrackedFiles,
    inputs: &mut Inputs,
) -> BuildResult<()> {
    for config_name in ["config.toml", "config", "rustc-wrapper.sh"] {
        let workspace_path = workspace.join(".cargo").join(config_name);
        watch(watched, workspace_path.clone())?;
        add_optional_file(
            workspace,
            &workspace_path,
            posix_relative(workspace, &workspace_path)?,
            tracked,
            inputs,
        )?;

        let repository_path = repository.join(".cargo").join(config_name);
        watch(watched, repository_path.clone())?;
        add_optional_file(
            repository,
            &repository_path,
            format!("repo/.cargo/{config_name}"),
            tracked,
            inputs,
        )?;
    }
    for config_name in [
        "rustfmt.toml",
        ".rustfmt.toml",
        "clippy.toml",
        ".clippy.toml",
    ] {
        let workspace_path = workspace.join(config_name);
        watch(watched, workspace_path.clone())?;
        add_optional_file(
            workspace,
            &workspace_path,
            config_name.to_owned(),
            tracked,
            inputs,
        )?;

        let repository_path = repository.join(config_name);
        watch(watched, repository_path.clone())?;
        add_optional_file(
            repository,
            &repository_path,
            format!("repo/{config_name}"),
            tracked,
            inputs,
        )?;
    }
    Ok(())
}

fn pinned_toolchain(
    workspace: &Path,
    repository: &Path,
    watched: &mut BTreeSet<PathBuf>,
    tracked: &TrackedFiles,
    inputs: &mut Inputs,
) -> BuildResult<String> {
    let candidates = [
        (workspace, "rust-toolchain.toml", "rust-toolchain.toml"),
        (workspace, "rust-toolchain", "rust-toolchain"),
        (
            repository,
            "rust-toolchain.toml",
            "repo/rust-toolchain.toml",
        ),
        (repository, "rust-toolchain", "repo/rust-toolchain"),
    ];
    let mut selected = None;
    for (anchor, relative, logical_name) in candidates {
        let path = anchor.join(relative);
        watch(watched, path.clone())?;
        if add_optional_file(anchor, &path, logical_name.to_owned(), tracked, inputs)?
            && selected.is_none()
        {
            selected = Some((path, logical_name.to_owned()));
        }
    }
    let (path, logical_name) =
        selected.ok_or_else(|| invalid("no pinned Rust toolchain file exists"))?;
    let bytes = inputs
        .get(&logical_name)
        .ok_or_else(|| invalid("pinned toolchain file was not inventoried"))?;
    let contents = std::str::from_utf8(bytes)?;
    if path
        .extension()
        .is_some_and(|extension| extension == "toml")
    {
        let document = contents.parse::<DocumentMut>()?;
        let channel = document
            .as_table()
            .get("toolchain")
            .and_then(Item::as_table)
            .and_then(|toolchain| toolchain.get("channel"))
            .and_then(Item::as_str)
            .ok_or_else(|| invalid("toolchain TOML has no string channel"))?;
        if channel.is_empty() {
            return Err(invalid("toolchain channel is empty").into());
        }
        Ok(channel.to_owned())
    } else {
        let channel = contents.trim();
        if channel.is_empty() || channel.contains(char::is_whitespace) {
            return Err(invalid("plain rust-toolchain file is not one channel name").into());
        }
        Ok(channel.to_owned())
    }
}

fn collect_workspace_tree(
    workspace: &Path,
    directory: &Path,
    inputs: &mut Inputs,
    required: bool,
    tracked: &TrackedFiles,
) -> BuildResult<()> {
    let mut add = |path: &Path| {
        let name = posix_relative(workspace, path)?;
        add_file(workspace, path, name, tracked, inputs)
    };
    collect_tree(directory, &mut add, required, tracked)
}

fn collect_external_tree(
    directory: &Path,
    release_prefix: &str,
    inputs: &mut Inputs,
    required: bool,
    tracked: &TrackedFiles,
) -> BuildResult<()> {
    let mut add = |path: &Path| {
        let relative = posix_relative(directory, path)?;
        add_file(
            directory,
            path,
            format!("{release_prefix}/{relative}"),
            tracked,
            inputs,
        )
    };
    collect_tree(directory, &mut add, required, tracked)
}

fn collect_tree(
    directory: &Path,
    add: &mut dyn FnMut(&Path) -> BuildResult<()>,
    required: bool,
    tracked: &TrackedFiles,
) -> BuildResult<()> {
    let Some(metadata) = optional_metadata(directory)? else {
        if required {
            return Err(invalid("required source inventory directory is missing").into());
        }
        return Ok(());
    };
    if !metadata.is_dir() {
        return Err(invalid("source inventory root is not a directory").into());
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        let entry_name = entry.file_name();
        if entry_name.to_str() == Some(".git")
            || (kind.is_dir() && is_generated_directory(&entry_name))
        {
            continue;
        }
        if kind.is_symlink() {
            if tracked.contains(&path)? || tracked.has_descendant(&path)? {
                return Err(invalid("source inventory contains a symlink").into());
            }
            continue;
        }
        if kind.is_dir() {
            if tracked.contains(&path)? {
                return Err(invalid("tracked source file is not a regular file").into());
            }
            if tracked.has_descendant(&path)? {
                collect_tree(&path, add, true, tracked)?;
            }
        } else if kind.is_file() {
            if tracked.has_descendant(&path)? {
                return Err(invalid("tracked source directory is not a directory").into());
            }
            if tracked.contains(&path)? {
                add(&path)?;
            }
        } else if tracked.contains(&path)? || tracked.has_descendant(&path)? {
            return Err(invalid("source inventory contains a non-file entry").into());
        }
    }
    Ok(())
}

fn is_generated_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".ruff_cache"
                | ".cache"
                | ".tox"
                | ".venv"
                | "venv"
                | "node_modules"
                | "dist"
                | "build"
                | "target"
                | ".agent-run"
        )
    )
}

fn add_required_file(
    anchor: &Path,
    path: &Path,
    logical_name: String,
    tracked: &TrackedFiles,
    inputs: &mut Inputs,
) -> BuildResult<()> {
    if add_optional_file(anchor, path, logical_name, tracked, inputs)? {
        Ok(())
    } else {
        Err(invalid("required source inventory file is missing").into())
    }
}

fn add_optional_file(
    anchor: &Path,
    path: &Path,
    logical_name: String,
    tracked: &TrackedFiles,
    inputs: &mut Inputs,
) -> BuildResult<bool> {
    if !tracked.contains(path)? {
        return Ok(false);
    }
    let Some(metadata) = metadata_beneath(anchor, path)? else {
        return Ok(false);
    };
    if !metadata.is_file() {
        return Err(invalid("source inventory input is not a regular file").into());
    }
    inputs.read_file(path, logical_name, &metadata)?;
    Ok(true)
}

fn add_file(
    anchor: &Path,
    path: &Path,
    logical_name: String,
    tracked: &TrackedFiles,
    inputs: &mut Inputs,
) -> BuildResult<()> {
    if !tracked.contains(path)? {
        return Ok(());
    }
    let metadata = metadata_beneath(anchor, path)?
        .ok_or_else(|| invalid("tracked source inventory input is missing"))?;
    if !metadata.is_file() {
        return Err(invalid("source inventory input is not a regular file").into());
    }
    inputs.read_file(path, logical_name, &metadata)
}

fn optional_metadata(path: &Path) -> BuildResult<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(invalid("source inventory path is a symlink").into())
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn metadata_beneath(anchor: &Path, path: &Path) -> BuildResult<Option<fs::Metadata>> {
    let relative = path
        .strip_prefix(anchor)
        .map_err(|_| invalid("source inventory path escapes its root"))?;
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(invalid("source inventory file path is empty").into());
    }
    let mut current = anchor.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(invalid("source inventory path is not normal").into());
        };
        current.push(name);
        let Some(metadata) = optional_metadata(&current)? else {
            return Ok(None);
        };
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(invalid("source inventory ancestor is not a directory").into());
        }
        if index + 1 == components.len() {
            return Ok(Some(metadata));
        }
    }
    Err(invalid("source inventory file path is empty").into())
}

fn posix_relative(root: &Path, path: &Path) -> BuildResult<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| invalid("source inventory path escapes its root"))?;
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid("source inventory path is not normal").into());
        };
        let name = name
            .to_str()
            .ok_or_else(|| invalid("source inventory path is not UTF-8"))?;
        components.push(name);
    }
    if components.is_empty() {
        return Err(invalid("source inventory path is empty").into());
    }
    Ok(components.join("/"))
}

fn parse_toml(bytes: &[u8], context: &'static str) -> BuildResult<DocumentMut> {
    let text = std::str::from_utf8(bytes)?;
    text.parse::<DocumentMut>()
        .map_err(|_| invalid(context).into())
}

fn join_logical(directory: &str, child: &str) -> String {
    if directory.is_empty() {
        child.to_owned()
    } else {
        format!("{directory}/{child}")
    }
}

fn watch(paths: &mut BTreeSet<PathBuf>, path: PathBuf) -> BuildResult<()> {
    path.to_str()
        .ok_or_else(|| invalid("Cargo rerun path is not UTF-8"))?;
    paths.insert(path);
    Ok(())
}

fn emit_path_instruction(kind: &str, path: &Path) -> io::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 Cargo path"))?;
    if value.contains(['\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Cargo path contains a line break",
        ));
    }
    emit_instruction(kind, value)
}

fn emit_instruction(kind: &str, value: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "cargo:{kind}={value}")
}

fn validate_metadata_value(value: &str) -> BuildResult<()> {
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(invalid("build metadata value is empty or contains a line break").into());
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::{BuildResult, Inputs, TrackedFiles, collect_external_tree, invalid, source_digest};
    use provenant_records::SourceInput;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn untracked_and_ignored_files_do_not_change_the_source_digest() -> BuildResult<()> {
        let temporary = tempfile::tempdir()?;
        let repository = fs::canonicalize(temporary.path())?;
        let workspace = repository.join("tools/front-door");
        let assets = repository.join("assets");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&assets)?;
        fs::write(repository.join(".gitignore"), b"assets/ignored.txt\n")?;
        fs::write(assets.join("tracked.txt"), b"tracked\n")?;
        run_git(&repository, &["init", "--quiet"])?;
        run_git(&repository, &["add", ".gitignore", "assets/tracked.txt"])?;
        run_git(
            &repository,
            &["check-ignore", "--quiet", "assets/ignored.txt"],
        )?;

        let initial = digest_assets(&workspace, &repository, &assets)?;
        fs::write(assets.join("untracked.txt"), b"untracked\n")?;
        fs::write(assets.join("ignored.txt"), b"ignored\n")?;
        let after_untracked_files = digest_assets(&workspace, &repository, &assets)?;

        assert_eq!(initial, after_untracked_files);
        Ok(())
    }

    #[test]
    fn source_inventory_requires_a_git_checkout() -> BuildResult<()> {
        let temporary = tempfile::tempdir()?;
        let repository = fs::canonicalize(temporary.path())?;
        let workspace = repository.join("tools/front-door");
        fs::create_dir_all(&workspace)?;

        assert!(TrackedFiles::load(&workspace, &repository).is_err());
        Ok(())
    }

    fn digest_assets(
        workspace: &Path,
        repository: &Path,
        assets: &Path,
    ) -> BuildResult<provenant_records::SourceDigest> {
        let tracked = TrackedFiles::load(workspace, repository)?;
        let mut inputs = Inputs::default();
        collect_external_tree(assets, "assets", &mut inputs, true, &tracked)?;
        let borrowed: Vec<SourceInput<'_>> = inputs
            .iter()
            .map(|(name, bytes, _)| SourceInput { name, bytes })
            .collect();
        Ok(source_digest(&borrowed)?)
    }

    fn run_git(repository: &Path, arguments: &[&str]) -> BuildResult<()> {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(repository)
            .status()?;
        if !status.success() {
            return Err(invalid("git test fixture command failed").into());
        }
        Ok(())
    }
}
