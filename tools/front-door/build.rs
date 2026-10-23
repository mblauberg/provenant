use provenant_records::{SourceDigest, SourceInput, source_digest};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use toml_edit::{DocumentMut, Item};

type BuildResult<T> = Result<T, Box<dyn Error>>;

const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_FILES: usize = 100_000;
const MAX_SOURCE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Default)]
struct Inputs {
    files: BTreeMap<String, Vec<u8>>,
    total_bytes: u64,
}

impl Inputs {
    fn get(&self, name: &str) -> Option<&Vec<u8>> {
        self.files.get(name)
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &Vec<u8>)> {
        self.files.iter()
    }

    fn read_file(&mut self, path: &Path, name: String) -> BuildResult<()> {
        validate_name(&name)?;
        if self.files.contains_key(&name) {
            return Err(invalid("source inventory contains duplicate names").into());
        }
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
        if self.files.insert(name, bytes).is_some() {
            return Err(invalid("source inventory contains duplicate names").into());
        }
        Ok(())
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

    let mut watched = BTreeSet::new();
    let mut inputs = Inputs::default();

    let workspace_manifest = workspace.join("Cargo.toml");
    watch(&mut watched, workspace_manifest.clone())?;
    add_required_file(
        &workspace,
        &workspace_manifest,
        "Cargo.toml".to_owned(),
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
    add_required_file(&workspace, &lockfile, "Cargo.lock".to_owned(), &mut inputs)?;

    let members = discover_members(&workspace, &workspace_document, &mut watched)?;
    for member in &members {
        let manifest = member.directory.join("Cargo.toml");
        let logical_manifest = join_logical(&member.logical_directory, "Cargo.toml");
        watch(&mut watched, manifest.clone())?;
        if logical_manifest != "Cargo.toml" {
            add_required_file(&workspace, &manifest, logical_manifest, &mut inputs)?;
        }

        let build_script = member.directory.join("build.rs");
        let logical_build_script = join_logical(&member.logical_directory, "build.rs");
        watch(&mut watched, build_script.clone())?;
        add_optional_file(&workspace, &build_script, logical_build_script, &mut inputs)?;

        for source_directory in ["src", "tests", "examples", "benches", "assets"] {
            let directory = member.directory.join(source_directory);
            watch(&mut watched, directory.clone())?;
            collect_workspace_tree(
                &workspace,
                &directory,
                &mut inputs,
                source_directory == "src",
            )?;
        }
    }

    // External product inputs are assigned their release asset names so the
    // pure digest API only sees normal relative POSIX paths (never ../ paths).
    for (source, release_name) in [
        ("skills", "assets/skills"),
        ("library", "assets/library"),
        ("templates", "assets/templates"),
        ("config", "assets/config"),
    ] {
        let directory = repository.join(source);
        watch(&mut watched, directory.clone())?;
        let required = source != "templates";
        collect_external_tree(&directory, release_name, &mut inputs, required)?;
    }
    for (source, release_name) in [
        ("AGENTS.md", "assets/instructions/AGENTS.md"),
        ("HARNESS.md", "assets/instructions/HARNESS.md"),
    ] {
        let path = repository.join(source);
        watch(&mut watched, path.clone())?;
        add_required_file(&repository, &path, release_name.to_owned(), &mut inputs)?;
    }

    add_workspace_configuration(&workspace, &repository, &mut watched, &mut inputs)?;
    let toolchain = pinned_toolchain(&workspace, &repository, &mut watched, &mut inputs)?;
    let target = env::var("TARGET")
        .map_err(|_| invalid("Cargo did not provide TARGET to the build script"))?;
    validate_metadata_value(&target)?;
    validate_metadata_value(&toolchain)?;

    let borrowed_inputs: Vec<SourceInput<'_>> = inputs
        .iter()
        .map(|(name, bytes)| SourceInput { name, bytes })
        .collect();
    let source_stamp: SourceDigest = source_digest(&borrowed_inputs)?;

    for path in watched {
        emit_path_instruction("rerun-if-changed", &path)?;
    }
    emit_instruction(
        "rustc-env",
        &format!("PROVENANT_SOURCE_DIGEST={source_stamp}"),
    )?;
    emit_instruction("rustc-env", &format!("PROVENANT_TARGET_TRIPLE={target}"))?;
    emit_instruction("rustc-env", &format!("PROVENANT_TOOLCHAIN={toolchain}"))?;
    Ok(())
}

fn discover_members(
    workspace: &Path,
    manifest: &DocumentMut,
    watched: &mut BTreeSet<PathBuf>,
) -> BuildResult<Vec<Member>> {
    let members = manifest
        .as_table()
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|workspace| workspace.get("members"))
        .and_then(Item::as_array)
        .ok_or_else(|| invalid("workspace manifest has no string members array"))?;
    let mut result = Vec::new();
    let mut logical_names = BTreeSet::new();

    for member in members.iter() {
        let pattern = member
            .as_str()
            .ok_or_else(|| invalid("workspace member pattern is not a string"))?;
        match pattern {
            "." => {
                if !logical_names.insert(".".to_owned()) {
                    return Err(invalid("workspace member is listed more than once").into());
                }
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
                        return Err(invalid("workspace member path is a symlink").into());
                    }
                    if !kind.is_dir() {
                        continue;
                    }
                    let name = child
                        .file_name()
                        .into_string()
                        .map_err(|_| invalid("workspace member name is not UTF-8"))?;
                    let logical_directory = format!("crates/{name}");
                    if !logical_names.insert(logical_directory.clone()) {
                        return Err(invalid("workspace member is listed more than once").into());
                    }
                    let directory = child.path();
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
    inputs: &mut Inputs,
) -> BuildResult<()> {
    for config_name in ["config.toml", "config"] {
        let workspace_path = workspace.join(".cargo").join(config_name);
        watch(watched, workspace_path.clone())?;
        add_optional_file(
            workspace,
            &workspace_path,
            posix_relative(workspace, &workspace_path)?,
            inputs,
        )?;

        let repository_path = repository.join(".cargo").join(config_name);
        watch(watched, repository_path.clone())?;
        add_optional_file(
            repository,
            &repository_path,
            format!("repo/.cargo/{config_name}"),
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
        add_optional_file(workspace, &workspace_path, config_name.to_owned(), inputs)?;

        let repository_path = repository.join(config_name);
        watch(watched, repository_path.clone())?;
        add_optional_file(
            repository,
            &repository_path,
            format!("repo/{config_name}"),
            inputs,
        )?;
    }
    Ok(())
}

fn pinned_toolchain(
    workspace: &Path,
    repository: &Path,
    watched: &mut BTreeSet<PathBuf>,
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
        if add_optional_file(anchor, &path, logical_name.to_owned(), inputs)? && selected.is_none()
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
) -> BuildResult<()> {
    let mut add = |path: &Path| {
        let name = posix_relative(workspace, path)?;
        add_file(path, name, inputs)
    };
    collect_tree(directory, &mut add, required)
}

fn collect_external_tree(
    directory: &Path,
    release_prefix: &str,
    inputs: &mut Inputs,
    required: bool,
) -> BuildResult<()> {
    let mut add = |path: &Path| {
        let relative = posix_relative(directory, path)?;
        add_file(path, format!("{release_prefix}/{relative}"), inputs)
    };
    collect_tree(directory, &mut add, required)
}

fn collect_tree(
    directory: &Path,
    add: &mut dyn FnMut(&Path) -> BuildResult<()>,
    required: bool,
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
            return Err(invalid("source inventory contains a symlink").into());
        }
        if kind.is_dir() {
            collect_tree(&path, add, true)?;
        } else if kind.is_file() {
            add(&path)?;
        } else {
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
                | "target"
                | ".agent-run"
        )
    )
}

fn add_required_file(
    anchor: &Path,
    path: &Path,
    logical_name: String,
    inputs: &mut Inputs,
) -> BuildResult<()> {
    if add_optional_file(anchor, path, logical_name, inputs)? {
        Ok(())
    } else {
        Err(invalid("required source inventory file is missing").into())
    }
}

fn add_optional_file(
    anchor: &Path,
    path: &Path,
    logical_name: String,
    inputs: &mut Inputs,
) -> BuildResult<bool> {
    let Some(metadata) = metadata_beneath(anchor, path)? else {
        return Ok(false);
    };
    if !metadata.is_file() {
        return Err(invalid("source inventory input is not a regular file").into());
    }
    inputs.read_file(path, logical_name)?;
    Ok(true)
}

fn add_file(path: &Path, logical_name: String, inputs: &mut Inputs) -> BuildResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid("source inventory contains a symlink").into());
    }
    if !metadata.is_file() {
        return Err(invalid("source inventory input is not a regular file").into());
    }
    inputs.read_file(path, logical_name)
}

fn validate_name(name: &str) -> BuildResult<()> {
    if name.is_empty() || name.starts_with('/') || name.contains('\\') || name.contains('\0') {
        return Err(invalid("source inventory name is not a normal POSIX path").into());
    }
    if name
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid("source inventory name is not a normal POSIX path").into());
    }
    Ok(())
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
        if name.is_empty() || name.contains('\\') || name.contains('\0') {
            return Err(invalid("source inventory path is not a POSIX name").into());
        }
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
