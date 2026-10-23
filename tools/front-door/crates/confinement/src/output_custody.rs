//! Identity-bound publication over the supervisor's safe filesystem API.
use crate::pending::{python_decimal_digit, python_repr_os};
use provenant_core::py::python_repr;
pub use provenant_supervisor::fs::{FileKind, LocalFs, Metadata, SafeDir, SafeFile, SafeFs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("{0}")]
    Refused(&'static str),
    #[error("{operation}: {source}")]
    Io {
        operation: &'static str,
        filename: Option<OsString>,
        filename2: Option<OsString>,
        #[source]
        source: io::Error,
    },
}

impl CustodyError {
    /// Render the exception text printed by the Python command at its edge.
    pub fn compatibility_message(&self) -> String {
        match self {
            Self::Refused(message) => (*message).to_owned(),
            Self::Io {
                filename,
                filename2,
                source,
                ..
            } => {
                let mut message = match source.raw_os_error() {
                    Some(errno) => {
                        let rendered = source.to_string();
                        let suffix = format!(" (os error {errno})");
                        let strerror = rendered.strip_suffix(&suffix).unwrap_or(&rendered);
                        format!("[Errno {errno}] {strerror}")
                    }
                    None => source.to_string(),
                };
                if let Some(filename) = filename {
                    message.push_str(": ");
                    message.push_str(&python_repr_os(filename));
                    if let Some(filename2) = filename2 {
                        message.push_str(" -> ");
                        message.push_str(&python_repr_os(filename2));
                    }
                }
                message
            }
        }
    }
}

fn io_error(operation: &'static str, path: &OsStr, source: io::Error) -> CustodyError {
    io_error_with_filenames(operation, Some(path), None, source)
}

fn io_error_without_filename(operation: &'static str, source: io::Error) -> CustodyError {
    io_error_with_filenames(operation, None, None, source)
}

fn io_error_with_filenames(
    operation: &'static str,
    filename: Option<&OsStr>,
    filename2: Option<&OsStr>,
    source: io::Error,
) -> CustodyError {
    CustodyError::Io {
        operation,
        filename: filename.map(OsStr::to_owned),
        filename2: filename2.map(OsStr::to_owned),
        source,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledOutput {
    pub digest: String,
    pub device: u64,
    pub inode: u64,
}

pub enum Source<'a> {
    Path(&'a Path),
    Handle(&'a dyn SafeFile),
}

/// Split lexical bytes: `Path::components` would discard a trailing slash or final dot.
#[cfg(unix)]
pub fn destination_parts(path: &Path) -> Result<(bool, Vec<OsString>, OsString), CustodyError> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = path.as_os_str().as_bytes();
    let mut parts: Vec<_> = bytes.split(|c| *c == b'/').collect();
    let leaf = parts.pop().unwrap_or_default();
    if leaf.is_empty() || leaf == b"." || leaf == b".." {
        return Err(CustodyError::Refused("output destination has no file name"));
    }
    if parts.contains(&&b".."[..]) {
        return Err(CustodyError::Refused(
            "output destination may not traverse a parent directory",
        ));
    }
    Ok((
        bytes.starts_with(b"/"),
        parts
            .into_iter()
            .filter(|s| !s.is_empty() && *s != b".")
            .map(|s| OsString::from_vec(s.to_vec()))
            .collect(),
        OsString::from_vec(leaf.to_vec()),
    ))
}

fn open_parent(os: &dyn SafeFs, path: &Path) -> Result<(Box<dyn SafeDir>, OsString), CustodyError> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let bytes = path.as_os_str().as_bytes();
    let absolute = bytes.starts_with(b"/");
    let root_name = if absolute {
        OsStr::new("/")
    } else {
        OsStr::new(".")
    };
    let mut current = os
        .root(absolute)
        .map_err(|error| io_error("open parent", root_name, error))?;

    let mut components: Vec<_> = bytes.split(|byte| *byte == b'/').collect();
    if absolute && !components.is_empty() {
        components.remove(0);
    }
    if components
        .last()
        .is_none_or(|leaf| leaf.is_empty() || *leaf == b"." || *leaf == b"..")
    {
        return Err(CustodyError::Refused("output destination has no file name"));
    }
    let leaf = OsString::from_vec(components.pop().unwrap_or_default().to_vec());
    for component in components {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            return Err(CustodyError::Refused(
                "output destination may not traverse a parent directory",
            ));
        }
        let part = OsString::from_vec(component.to_vec());
        let entry = current
            .metadata_at(&part, false)
            .map_err(|error| io_error("stat parent", &part, error))?;
        let follow = entry.kind == FileKind::Symlink;
        if follow {
            let container = current
                .metadata()
                .map_err(|error| io_error_without_filename("stat directory", error))?;
            if entry.uid != 0 || container.uid != 0 || container.mode & 0o022 != 0 {
                return Err(CustodyError::Refused(
                    "output destination parent is a symlink",
                ));
            }
        }
        current = current
            .open_directory(&part, follow)
            .map_err(|error| io_error("open directory", &part, error))?;
    }
    Ok((current, leaf))
}

fn validate_leaf(os: &dyn SafeDir, leaf: &OsStr) -> Result<(), CustodyError> {
    let entry = match os.metadata_at(leaf, false) {
        Ok(entry) => entry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("stat destination", leaf, error)),
    };
    match entry.kind {
        FileKind::Directory => Err(CustodyError::Refused("output destination is a directory")),
        FileKind::Symlink => match os.metadata_at(leaf, true) {
            Ok(target) if target.kind != FileKind::Regular => Err(CustodyError::Refused(
                "output destination symlink does not target a regular file",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error("stat destination target", leaf, error)),
        },
        FileKind::Regular => Ok(()),
        FileKind::Other => Err(CustodyError::Refused(
            "output destination is not a regular file",
        )),
    }
}

fn digest_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::from("sha256:");
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 15)]));
    }
    result
}

pub fn hash_file(file: &mut (impl Read + Seek)) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(digest_string(&hasher.finalize()))
}

struct Temporary<'a> {
    dir: &'a dyn SafeDir,
    name: Option<OsString>,
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if let Some(name) = &self.name {
            let _ = self.dir.unlink(name);
        }
    }
}

pub fn install(
    os: &dyn SafeFs,
    source: Source<'_>,
    destination: &Path,
) -> Result<InstalledOutput, CustodyError> {
    let (dir, leaf) = open_parent(os, destination)?;
    validate_leaf(&*dir, &leaf)?;
    let mut source = match source {
        Source::Path(path) => match os.open_source(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Err(CustodyError::Refused(
                    "dispatch output source is not a regular file",
                ));
            }
            Err(error) => return Err(io_error("open source", path.as_os_str(), error)),
        },
        Source::Handle(file) => {
            let mut file = file
                .duplicate()
                .map_err(|error| io_error_without_filename("duplicate source", error))?;
            file.seek(SeekFrom::Start(0))
                .map_err(|error| io_error_without_filename("seek source", error))?;
            file
        }
    };
    if source
        .metadata()
        .map_err(|error| io_error_without_filename("stat source", error))?
        .kind
        != FileKind::Regular
    {
        return Err(CustodyError::Refused(
            "dispatch output source is not a regular file",
        ));
    }

    let mut temporary = Temporary {
        dir: &*dir,
        name: None,
    };
    let mut allocated = None;
    for _ in 0..32 {
        let name = os
            .temporary_name()
            .map_err(|error| io_error_without_filename("allocate temporary", error))?;
        match dir.create_new(&name) {
            Ok(file) => {
                temporary.name = Some(name);
                allocated = Some(file);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create temporary", &name, error)),
        }
    }
    let mut file = allocated.ok_or(CustodyError::Refused(
        "could not allocate an output temporary file",
    ))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| io_error_without_filename("read source", error))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|error| io_error_without_filename("write temporary", error))?;
    }
    file.sync()
        .map_err(|error| io_error_without_filename("sync temporary", error))?;
    drop(file);
    if let Some(name) = &temporary.name {
        dir.replace(name, &leaf).map_err(|error| {
            io_error_with_filenames("replace destination", Some(name), Some(&leaf), error)
        })?;
    }
    temporary.name = None;
    dir.sync()
        .map_err(|error| io_error_without_filename("sync parent", error))?;
    let mut installed = dir
        .open_read(&leaf)
        .map_err(|error| io_error("open installed", &leaf, error))?;
    let meta = installed
        .metadata()
        .map_err(|error| io_error_without_filename("stat installed", error))?;
    if meta.kind != FileKind::Regular || meta.links != 1 {
        return Err(CustodyError::Refused(
            "installed output is not a retained regular file",
        ));
    }
    let digest = digest_string(&hash.finalize());
    if hash_file(&mut installed)
        .map_err(|error| io_error_without_filename("hash installed", error))?
        != digest
    {
        return Err(CustodyError::Refused("installed output identity changed"));
    }
    Ok(InstalledOutput {
        digest,
        device: meta.device,
        inode: meta.inode,
    })
}

fn retains(meta: Metadata, device: Option<u64>, inode: Option<u64>) -> bool {
    meta.kind == FileKind::Regular
        && meta.links == 1
        && device.is_some_and(|device| meta.device == device)
        && inode.is_some_and(|inode| meta.inode == inode)
}

fn verify_expected(
    os: &dyn SafeFs,
    destination: &Path,
    digest: Option<&str>,
    device: Option<u64>,
    inode: Option<u64>,
) -> Result<(), CustodyError> {
    {
        let (dir, leaf) = open_parent(os, destination)?;
        let mut file = dir
            .open_read(&leaf)
            .map_err(|error| io_error("open output", &leaf, error))?;
        let before = file
            .metadata()
            .map_err(|error| io_error_without_filename("stat output", error))?;
        if !retains(before, device, inode)
            || Some(
                hash_file(&mut file)
                    .map_err(|error| io_error_without_filename("hash output", error))?
                    .as_str(),
            ) != digest
        {
            return Err(CustodyError::Refused(
                "output path no longer names the installed identity",
            ));
        }
        let after = file
            .metadata()
            .map_err(|error| io_error_without_filename("stat output", error))?;
        if (after.device, after.inode, after.size, after.modified_ns)
            != (before.device, before.inode, before.size, before.modified_ns)
        {
            return Err(CustodyError::Refused(
                "output identity changed while being verified",
            ));
        }
    }
    let (dir, leaf) = open_parent(os, destination)?;
    let file = dir
        .open_read(&leaf)
        .map_err(|error| io_error("reopen output", &leaf, error))?;
    if !retains(
        file.metadata()
            .map_err(|error| io_error_without_filename("stat output", error))?,
        device,
        inode,
    ) {
        return Err(CustodyError::Refused(
            "output path no longer names the installed identity",
        ));
    }
    Ok(())
}

pub fn verify(
    os: &dyn SafeFs,
    destination: &Path,
    output: &InstalledOutput,
) -> Result<(), CustodyError> {
    verify_expected(
        os,
        destination,
        Some(&output.digest),
        Some(output.device),
        Some(output.inode),
    )
}

/// Parsed decimal accepted by Python's `int` command-line conversion. Values outside
/// the filesystem identity range remain valid integers and become verification refusals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PythonInteger {
    InRange(u64),
    NumberOutOfRange,
}

impl FromStr for PythonInteger {
    type Err = io::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Python's int parser accepts Unicode whitespace but not the extra
        // U+001C..U+001F characters accepted by str.strip().
        let value = value.trim_matches(char::is_whitespace);
        let mut digits = value.chars().peekable();
        let negative = match digits.peek() {
            Some('-') => {
                digits.next();
                true
            }
            Some('+') => {
                digits.next();
                false
            }
            _ => false,
        };
        let mut number = 0_u64;
        let mut overflow = false;
        let mut had_digit = false;
        let mut digit_count = 0_usize;
        let mut previous_digit = false;
        while let Some(character) = digits.next() {
            if let Some(digit) = python_decimal_digit(character) {
                digit_count += 1;
                if digit_count > 4300 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "integer string conversion exceeds Python's digit limit",
                    ));
                }
                had_digit = true;
                previous_digit = true;
                if !overflow {
                    match number
                        .checked_mul(10)
                        .and_then(|current| current.checked_add(u64::from(digit)))
                    {
                        Some(value) => number = value,
                        None => overflow = true,
                    }
                }
            } else if character == '_' {
                if !previous_digit
                    || digits
                        .peek()
                        .is_none_or(|next| python_decimal_digit(*next).is_none())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid integer",
                    ));
                }
                previous_digit = false;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid integer",
                ));
            }
        }
        if !had_digit || !previous_digit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid integer",
            ));
        }
        if (negative && number != 0) || overflow {
            Ok(Self::NumberOutOfRange)
        } else {
            Ok(Self::InRange(number))
        }
    }
}

#[derive(Debug)]
pub enum Request {
    Install {
        source: PathBuf,
        destination: PathBuf,
    },
    Verify {
        destination: PathBuf,
        digest: OsString,
        device: PythonInteger,
        inode: PythonInteger,
    },
}

fn program_name(argv0: Option<&OsStr>) -> String {
    argv0
        .map(Path::new)
        .and_then(Path::file_name)
        .unwrap_or_else(|| OsStr::new("output_custody.py"))
        .to_string_lossy()
        .into_owned()
}

fn command_output(stdout: String, stderr: String, exit_code: i32) -> CommandOutput {
    CommandOutput {
        stdout,
        stderr,
        exit_code,
    }
}

const DEFAULT_COLUMNS: usize = 80;
const MAX_COLUMNS: usize = 4096;
const USAGE_PREFIX: &str = "usage: ";
struct HelpSpec {
    usage_options: &'static [&'static str],
    usage_positionals: &'static [&'static str],
    option_rows: &'static [(&'static str, &'static str)],
    positional_rows: &'static [&'static str],
}

const ROOT_HELP_SPEC: HelpSpec = HelpSpec {
    usage_options: &["[-h]"],
    usage_positionals: &["{install,verify} ..."],
    option_rows: &[("-h, --help", "show this help message and exit")],
    positional_rows: &["{install,verify}"],
};
const INSTALL_HELP_SPEC: HelpSpec = HelpSpec {
    usage_options: &["[-h]", "--source SOURCE", "--destination DESTINATION"],
    usage_positionals: &[],
    option_rows: &[
        ("-h, --help", "show this help message and exit"),
        ("--source SOURCE", ""),
        ("--destination DESTINATION", ""),
    ],
    positional_rows: &[],
};
const VERIFY_HELP_SPEC: HelpSpec = HelpSpec {
    usage_options: &[
        "[-h]",
        "--destination DESTINATION",
        "--digest DIGEST",
        "--device DEVICE",
        "--inode INODE",
    ],
    usage_positionals: &[],
    option_rows: &[
        ("-h, --help", "show this help message and exit"),
        ("--destination DESTINATION", ""),
        ("--digest DIGEST", ""),
        ("--device DEVICE", ""),
        ("--inode INODE", ""),
    ],
    positional_rows: &[],
};

fn help_spec(command: Option<&str>) -> &'static HelpSpec {
    match command {
        Some("install") => &INSTALL_HELP_SPEC,
        Some(_) => &VERIFY_HELP_SPEC,
        None => &ROOT_HELP_SPEC,
    }
}

fn formatter_width(columns: usize) -> usize {
    columns.min(MAX_COLUMNS).saturating_sub(2)
}

fn environment_columns() -> usize {
    let Some(value) = std::env::var_os("COLUMNS").and_then(|value| value.into_string().ok()) else {
        return DEFAULT_COLUMNS;
    };
    match value.parse::<PythonInteger>() {
        Ok(PythonInteger::InRange(columns)) if columns > 0 => usize::try_from(columns)
            .unwrap_or(MAX_COLUMNS)
            .min(MAX_COLUMNS),
        Ok(PythonInteger::NumberOutOfRange)
            if !value.trim_matches(char::is_whitespace).starts_with('-') =>
        {
            MAX_COLUMNS
        }
        _ => DEFAULT_COLUMNS,
    }
}

fn usage_lines(
    parts: &[&str],
    indent: usize,
    prefix_width: Option<usize>,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut line_width =
        prefix_width.map_or(indent.saturating_sub(1), |prefix| prefix.saturating_sub(1));
    for part in parts {
        let next_width = line_width
            .saturating_add(1)
            .saturating_add(part.chars().count());
        if !line.is_empty() && next_width > width {
            lines.push(format!("{}{}", " ".repeat(indent), line.join(" ")));
            line.clear();
            line_width = indent.saturating_sub(1);
        }
        line_width = line_width
            .saturating_add(1)
            .saturating_add(part.chars().count());
        line.push(*part);
    }
    if !line.is_empty() {
        lines.push(format!("{}{}", " ".repeat(indent), line.join(" ")));
    }
    if prefix_width.is_some() {
        if let Some(first) = lines.first_mut() {
            *first = first.chars().skip(indent).collect();
        }
    }
    lines
}

fn argparse_usage(program: &str, command: Option<&str>, width: usize) -> String {
    let prefix = USAGE_PREFIX;
    let program = match command {
        Some(command) => format!("{program} {command}"),
        None => program.to_owned(),
    };
    let spec = help_spec(command);
    let option_parts = spec.usage_options;
    let positional_parts = spec.usage_positionals;
    let prog_len = program.chars().count();
    let width = width.max(1);
    let prefix_width = prefix.chars().count();
    let one_line_len = prefix_width
        + prog_len
        + option_parts
            .iter()
            .chain(positional_parts.iter())
            .map(|part| 1 + part.chars().count())
            .sum::<usize>();
    if one_line_len <= width {
        let parts = option_parts
            .iter()
            .chain(positional_parts.iter())
            .copied()
            .collect::<Vec<_>>();
        return format!(
            "{prefix}{program}{}",
            if parts.is_empty() {
                String::new()
            } else {
                format!(" {}", parts.join(" "))
            }
        );
    }

    let mut lines = Vec::new();
    if (prefix_width + prog_len).saturating_mul(4) <= width.saturating_mul(3) {
        let indent = prefix_width + prog_len + 1;
        if !option_parts.is_empty() {
            let mut parts = vec![program.as_str()];
            parts.extend_from_slice(option_parts);
            lines.extend(usage_lines(&parts, indent, Some(prefix_width), width));
        }
        if !positional_parts.is_empty() {
            lines.extend(usage_lines(positional_parts, indent, None, width));
        }
        if lines.is_empty() {
            lines.push(program.clone());
        }
        format!("{prefix}{}", lines.join("\n"))
    } else {
        let indent = prefix_width;
        let mut parts = option_parts.to_vec();
        parts.extend_from_slice(positional_parts);
        let mut wrapped = usage_lines(&parts, indent, None, width);
        if wrapped.len() > 1 {
            wrapped.clear();
            if !option_parts.is_empty() {
                wrapped.extend(usage_lines(option_parts, indent, None, width));
            }
            if !positional_parts.is_empty() {
                wrapped.extend(usage_lines(positional_parts, indent, None, width));
            }
        }
        lines.push(format!("{prefix}{program}"));
        lines.extend(wrapped);
        lines.join("\n")
    }
}

fn wrap_help(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_ascii_whitespace() {
        let next_width =
            line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
        if !line.is_empty() && next_width > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn argparse_options(command: Option<&str>, width: usize) -> String {
    let spec = help_spec(command);
    let rows = spec.option_rows;
    let root_positionals = !spec.positional_rows.is_empty();
    let current_indent = 2;
    let action_max_length = rows
        .iter()
        .map(|(invocation, _)| invocation.chars().count() + current_indent)
        .chain(root_positionals.then_some("{install,verify}".chars().count() + current_indent))
        .max()
        .unwrap_or(0);
    let max_help_position = 24.min(width.saturating_sub(20).max(current_indent * 2));
    let help_position = (action_max_length + 2).min(max_help_position);
    let action_width = help_position.saturating_sub(current_indent + 2);
    let help_width = width.saturating_sub(help_position).max(11);
    let mut output = String::new();

    if root_positionals {
        output.push_str("positional arguments:\n");
        for row in spec.positional_rows {
            output.push_str("  ");
            output.push_str(row);
            output.push('\n');
        }
        output.push('\n');
    }
    output.push_str("options:\n");
    for (invocation, help) in rows {
        output.push_str("  ");
        output.push_str(invocation);
        if help.is_empty() {
            output.push('\n');
            continue;
        }
        let invocation_width = invocation.chars().count();
        if action_width > 0 {
            output.push_str(&" ".repeat(action_width.saturating_sub(invocation_width)));
            output.push_str("  ");
            let lines = wrap_help(help, help_width);
            if let Some((first, rest)) = lines.split_first() {
                output.push_str(first);
                output.push('\n');
                for line in rest {
                    output.push_str(&" ".repeat(help_position));
                    output.push_str(line);
                    output.push('\n');
                }
            } else {
                output.push('\n');
            }
        } else {
            output.push('\n');
            for line in wrap_help(help, help_width) {
                output.push_str(&" ".repeat(help_position));
                output.push_str(&line);
                output.push('\n');
            }
        }
    }
    output
}

fn root_help(program: &str, width: usize) -> String {
    let width = formatter_width(width);
    let description = wrap_help(
        "Install and verify one identity-bound dispatch output.",
        width,
    )
    .join("\n");
    format!(
        "{}\n\n{}\n\n{}",
        argparse_usage(program, None, width),
        description,
        argparse_options(None, width)
    )
}

fn subcommand_help(program: &str, command: &str, width: usize) -> String {
    let width = formatter_width(width);
    format!(
        "{}\n\n{}",
        argparse_usage(program, Some(command), width),
        argparse_options(Some(command), width)
    )
}

fn root_usage(program: &str, width: usize) -> String {
    argparse_usage(program, None, formatter_width(width))
}

fn subcommand_usage(program: &str, command: &str, width: usize) -> String {
    argparse_usage(program, Some(command), formatter_width(width))
}

fn parser_error(_program: &str, prefix: &str, usage: &str, message: &str) -> CommandOutput {
    command_output(
        String::new(),
        format!("{usage}\n{prefix}: error: {message}\n"),
        2,
    )
}

fn python_integer(value: &OsStr, option: &str) -> Result<PythonInteger, String> {
    match value
        .to_str()
        .and_then(|value| value.parse::<PythonInteger>().ok())
    {
        Some(value) => Ok(value),
        None => Err(format!(
            "argument {option}: invalid int value: {}",
            python_repr_os(value)
        )),
    }
}

fn option_matches<'a>(value: &str, options: &'a [&'a str]) -> Option<&'a str> {
    if let Some(exact) = options.iter().copied().find(|option| *option == value) {
        return Some(exact);
    }
    if !value.starts_with("--") {
        return None;
    }
    let mut matches = options
        .iter()
        .copied()
        .filter(|option| option.starts_with(value));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn ambiguous_options<'a>(value: &str, options: &'a [&'a str]) -> Vec<&'a str> {
    if !value.starts_with("--") || options.contains(&value) {
        return Vec::new();
    }
    let matches: Vec<_> = options
        .iter()
        .copied()
        .filter(|option| option.starts_with(value))
        .collect();
    if matches.len() > 1 {
        matches
    } else {
        Vec::new()
    }
}

fn looks_like_option(value: &OsStr, numeric: bool) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    // argparse treats a lone dash as a value. Its Python 3.14 numeric matcher
    // is a prefix match for `-\.?\d`, so malformed integer strings such as
    // `-1x` are consumed as values and rejected by int() afterward.
    if value == "-" {
        return false;
    }
    if numeric {
        if let Some(number) = value.strip_prefix('-') {
            let digits = number.strip_prefix('.').unwrap_or(number);
            if digits
                .chars()
                .next()
                .is_some_and(|character| python_decimal_digit(character).is_some())
            {
                return false;
            }
        }
    }
    value.starts_with('-')
}

fn parse_subcommand(
    program: &str,
    command: &str,
    arguments: &[OsString],
    columns: usize,
) -> Result<Request, CommandOutput> {
    let usage = subcommand_usage(program, command, columns);
    let prefix = format!("{program} {command}");
    let options: &[&str] = if command == "install" {
        &["--help", "--source", "--destination"]
    } else {
        &["--help", "--destination", "--digest", "--device", "--inode"]
    };
    let mut source = None;
    let mut destination = None;
    let mut digest = None;
    let mut device = None;
    let mut inode = None;
    let mut unknown = Vec::new();
    let mut options_enabled = true;
    let mut index = 0;

    while index < arguments.len() {
        let argument = &arguments[index];
        let text = argument.to_str();
        if options_enabled && text == Some("--") {
            options_enabled = false;
            index += 1;
            continue;
        }
        if !options_enabled {
            unknown.push(argument.to_string_lossy().into_owned());
            index += 1;
            continue;
        }
        if let Some(text) = text.filter(|text| text.starts_with("-h")) {
            if let Some(value) = text.strip_prefix("-h=") {
                return Err(parser_error(
                    program,
                    &prefix,
                    &usage,
                    &format!(
                        "argument -h/--help: ignored explicit argument {}",
                        python_repr(value)
                    ),
                ));
            }
            return Err(command_output(
                subcommand_help(program, command, columns),
                String::new(),
                0,
            ));
        }
        if text.is_some_and(|text| option_matches(text, &["--help"]) == Some("--help")) {
            return Err(command_output(
                subcommand_help(program, command, columns),
                String::new(),
                0,
            ));
        }
        let Some(text) = text else {
            unknown.push(argument.to_string_lossy().into_owned());
            index += 1;
            continue;
        };
        let (name, inline_value) = text
            .split_once('=')
            .map_or((text, None), |(name, value)| (name, Some(value)));
        let ambiguous = ambiguous_options(name, options);
        if !ambiguous.is_empty() {
            return Err(parser_error(
                program,
                &prefix,
                &usage,
                &format!(
                    "ambiguous option: {name} could match {}",
                    ambiguous.join(", ")
                ),
            ));
        }
        if option_matches(name, &["--help"]) == Some("--help") {
            if let Some(value) = inline_value {
                return Err(parser_error(
                    program,
                    &prefix,
                    &usage,
                    &format!(
                        "argument -h/--help: ignored explicit argument {}",
                        python_repr(value)
                    ),
                ));
            }
            return Err(command_output(
                subcommand_help(program, command, columns),
                String::new(),
                0,
            ));
        }
        let Some(option) = option_matches(name, options) else {
            unknown.push(text.to_owned());
            index += 1;
            if !text.starts_with('-') && index < arguments.len() {
                // Argparse reports an unexpected positional and the following
                // positional values together as unrecognized arguments.
            }
            continue;
        };
        if option == "--help" {
            return Err(command_output(
                subcommand_help(program, command, columns),
                String::new(),
                0,
            ));
        }
        let numeric = matches!(option, "--device" | "--inode");
        let value = if let Some(value) = inline_value {
            OsString::from(value)
        } else {
            let next = arguments.get(index + 1);
            if next.is_none_or(|next| looks_like_option(next, numeric)) {
                return Err(parser_error(
                    program,
                    &prefix,
                    &usage,
                    &format!("argument {option}: expected one argument"),
                ));
            }
            index += 1;
            next.cloned().unwrap_or_default()
        };
        match option {
            "--source" => source = Some(PathBuf::from(value)),
            "--destination" => destination = Some(PathBuf::from(value)),
            "--digest" => digest = Some(value),
            "--device" => {
                device = Some(
                    python_integer(&value, option)
                        .map_err(|message| parser_error(program, &prefix, &usage, &message))?,
                )
            }
            "--inode" => {
                inode = Some(
                    python_integer(&value, option)
                        .map_err(|message| parser_error(program, &prefix, &usage, &message))?,
                )
            }
            _ => {}
        }
        index += 1;
    }

    let missing: Vec<_> = if command == "install" {
        [
            source.is_none().then_some("--source"),
            destination.is_none().then_some("--destination"),
        ]
        .into_iter()
        .flatten()
        .collect()
    } else {
        [
            destination.is_none().then_some("--destination"),
            digest.is_none().then_some("--digest"),
            device.is_none().then_some("--device"),
            inode.is_none().then_some("--inode"),
        ]
        .into_iter()
        .flatten()
        .collect()
    };
    if !missing.is_empty() {
        return Err(parser_error(
            program,
            &prefix,
            &usage,
            &format!(
                "the following arguments are required: {}",
                missing.join(", ")
            ),
        ));
    }
    if !unknown.is_empty() {
        return Err(parser_error(
            program,
            program,
            &root_usage(program, columns),
            &format!("unrecognized arguments: {}", unknown.join(" ")),
        ));
    }

    if command == "install" {
        Ok(Request::Install {
            source: source.unwrap_or_default(),
            destination: destination.unwrap_or_default(),
        })
    } else {
        Ok(Request::Verify {
            destination: destination.unwrap_or_default(),
            digest: digest.unwrap_or_default(),
            device: device.unwrap_or(PythonInteger::NumberOutOfRange),
            inode: inode.unwrap_or(PythonInteger::NumberOutOfRange),
        })
    }
}

/// Parse the source command's argparse wire format into a typed request.
pub fn parse_request<I, T>(args: I) -> Result<Request, CommandOutput>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    parse_request_with_width(args, environment_columns())
}

fn parse_request_with_width<I, T>(args: I, columns: usize) -> Result<Request, CommandOutput>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let program = program_name(args.first().map(OsString::as_os_str));
    let root_args = args.get(1..).unwrap_or_default();
    if root_args.is_empty() {
        return Err(parser_error(
            &program,
            &program,
            &root_usage(&program, columns),
            "the following arguments are required: command",
        ));
    }

    let mut options_enabled = true;
    let first_command = root_args.iter().position(|argument| {
        if !options_enabled {
            return true;
        }
        if argument == OsStr::new("--") {
            options_enabled = false;
            return false;
        }
        argument
            .to_str()
            .is_none_or(|text| text == "-" || !text.starts_with('-'))
    });
    let option_prefix_end = first_command.unwrap_or(root_args.len());
    for argument in &root_args[..option_prefix_end] {
        if let Some(text) = argument.to_str() {
            if text == "--" {
                break;
            }
            let name = text.split_once('=').map_or(text, |(name, _)| name);
            if text.starts_with("-h") || option_matches(name, &["--help"]) == Some("--help") {
                if let Some(value) = text
                    .strip_prefix("-h=")
                    .or_else(|| text.strip_prefix("--help="))
                {
                    return Err(parser_error(
                        &program,
                        &program,
                        &root_usage(&program, columns),
                        &format!(
                            "argument -h/--help: ignored explicit argument {}",
                            python_repr(value)
                        ),
                    ));
                }
                return Err(command_output(
                    root_help(&program, columns),
                    String::new(),
                    0,
                ));
            }
        }
    }
    if first_command.is_none() {
        return Err(parser_error(
            &program,
            &program,
            &root_usage(&program, columns),
            "the following arguments are required: command",
        ));
    }
    let command_index = first_command.unwrap_or_default();
    let command = &root_args[command_index];
    let Some(command_name) = command.to_str() else {
        return Err(parser_error(
            &program,
            &program,
            &root_usage(&program, columns),
            &format!(
                "argument command: invalid choice: {} (choose from 'install', 'verify')",
                python_repr_os(command)
            ),
        ));
    };
    if command_name != "install" && command_name != "verify" {
        return Err(parser_error(
            &program,
            &program,
            &root_usage(&program, columns),
            &format!(
                "argument command: invalid choice: {} (choose from 'install', 'verify')",
                python_repr(command_name)
            ),
        ));
    }
    let parsed = parse_subcommand(
        &program,
        command_name,
        &root_args[command_index + 1..],
        columns,
    );
    let unknown_root: Vec<_> = root_args[..command_index]
        .iter()
        .filter(|value| value.as_os_str() != OsStr::new("--"))
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    match parsed {
        Ok(_request) if !unknown_root.is_empty() => Err(parser_error(
            &program,
            &program,
            &root_usage(&program, columns),
            &format!("unrecognized arguments: {}", unknown_root.join(" ")),
        )),
        other => other,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl CommandOutput {
    pub fn write_to(&self, stdout: &mut impl Write, stderr: &mut impl Write) -> io::Result<i32> {
        stdout.write_all(self.stdout.as_bytes())?;
        stderr.write_all(self.stderr.as_bytes())?;
        Ok(self.exit_code)
    }
}

pub fn execute(os: &dyn SafeFs, request: Request) -> CommandOutput {
    match request {
        Request::Install {
            source,
            destination,
        } => match install(os, Source::Path(&source), &destination) {
            Ok(output) => CommandOutput {
                stdout: format!("{} {} {}\n", output.digest, output.device, output.inode),
                stderr: String::new(),
                exit_code: 0,
            },
            Err(error) => failed(error),
        },
        Request::Verify {
            destination,
            digest,
            device,
            inode,
        } => {
            let number = |value| match value {
                PythonInteger::InRange(value) => Some(value),
                PythonInteger::NumberOutOfRange => None,
            };
            match verify_expected(
                os,
                &destination,
                digest.to_str(),
                number(device),
                number(inode),
            ) {
                Ok(()) => CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                },
                Err(error) => failed(error),
            }
        }
    }
}

fn failed(error: CustodyError) -> CommandOutput {
    CommandOutput {
        stdout: String::new(),
        stderr: format!("output custody failed: {}\n", error.compatibility_message()),
        exit_code: 1,
    }
}

/// Parse and run the small standalone command. Arguments include argv[0].
pub fn run_cli<I, T>(args: I) -> CommandOutput
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    match parse_request(args) {
        Ok(request) => execute(&LocalFs, request),
        Err(output) => output,
    }
}

#[cfg(test)]
mod tests;
