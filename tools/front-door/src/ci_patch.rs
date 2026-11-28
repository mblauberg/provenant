//! Schema and execution for the generated-artefact CI handoff.
//!
//! `provenant ci patch` reads the `generated-<head>` artefact that the
//! `generated` CI job uploads, checks it against the run it came from and the
//! worktree it would change, then optionally applies it. Verification is a
//! pure, testable function; Git and forge access live behind a small runtime
//! seam so the head-mismatch and dirty-work contracts can be pinned without a
//! forge account.
use crate::pr::status::batch;
use crate::pr::{forge_execute, successful};
use provenant_cli::Globals;
use provenant_core::{AttemptId, GitOid};
use provenant_records::Layer;
use provenant_records::command_result::{
    CommandError, ErrorClass, NextStep, SettingRef, Subject as ErrorSubject,
};
use provenant_records::custody::{Subject as CustodySubject, SubjectKind, guard_destructive};
use provenant_records::warning::{Warning, WarningCode, detected_source, warn};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The setting that governs generated patches. A `Configured` refusal names it
/// so the error catalogue's "every configured error has a setting" rule holds.
pub(crate) const GENERATED_PATCH_KEY: &str = "domains.ci.generated_patch";
const LEAF: &str = "ci patch";
const COMMIT_MESSAGE: &str = "chore: apply generated patch";
const READ_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GeneratorStatus {
    Clean,
    Drift,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GeneratorManifest {
    pub name: String,
    pub status: GeneratorStatus,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GeneratedManifest {
    pub repo: String,
    pub workflow: String,
    pub run_id: u64,
    pub run_attempt: u32,
    pub head_sha: GitOid,
    pub generators: Vec<GeneratorManifest>,
    pub patch_sha256: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GeneratedPatchMode {
    Off,
    #[default]
    Offer,
    Apply,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CiSettings {
    generated_patch: GeneratedPatchMode,
}

#[derive(Clone, Debug)]
struct ResolvedGeneratedPatch {
    mode: GeneratedPatchMode,
    setting: SettingRef,
}

impl ResolvedGeneratedPatch {
    fn read(state: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let (settings, keys): (CiSettings, std::collections::BTreeSet<String>) =
            provenant_config::read_domain_with_keys(state, "ci")?;
        let source = keys.contains("generated_patch");
        let value = match settings.generated_patch {
            GeneratedPatchMode::Off => "off",
            GeneratedPatchMode::Offer => "offer",
            GeneratedPatchMode::Apply => "apply",
        };
        Ok(Self {
            mode: settings.generated_patch,
            setting: SettingRef {
                key: GENERATED_PATCH_KEY.to_owned(),
                value: value.to_owned(),
                layer: if source { Layer::Host } else { Layer::Default },
                origin: if source {
                    state.join("config.toml").display().to_string()
                } else {
                    "harness-knobs.json".to_owned()
                },
            },
        })
    }

    fn permits(self, apply: bool) -> Result<Self, PatchError> {
        let (code, message) = match (self.mode, apply) {
            (GeneratedPatchMode::Off, _) => (
                "CI_PATCH_DISABLED",
                "generated patch handling is disabled by configuration",
            ),
            (GeneratedPatchMode::Offer, false) => (
                "CI_PATCH_APPLY_REQUIRED",
                "generated patch handling requires --apply under the offer policy",
            ),
            _ => return Ok(self),
        };
        Err(PatchError::configured(
            code,
            message.to_owned(),
            ErrorSubject {
                host: None,
                project: None,
                graph: None,
                node: None,
                attempt: None,
                item: None,
                pr: None,
                head: None,
            },
            self.setting,
        ))
    }
}

/// The run facts the artefact must agree with before it is trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunIdentity {
    pub repo: String,
    pub workflow: String,
    pub run_attempt: u32,
}

/// What applying the verified patch did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PatchReport {
    pub head: GitOid,
    pub applied: bool,
    pub committed: bool,
    pub paths: Vec<String>,
}

/// The one typed failure shape for this command. `code` is stable for the
/// catalogue; `next` is never empty so the error contract holds.
#[derive(Debug)]
pub(crate) struct PatchError {
    pub code: String,
    pub message: Box<str>,
    pub cause: Option<Box<str>>,
    pub class: ErrorClass,
    pub setting: Option<Box<SettingRef>>,
    pub subject: Option<Box<ErrorSubject>>,
    pub next: Vec<NextStep>,
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PatchError {}

impl PatchError {
    fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        cause: Option<String>,
        class: ErrorClass,
        next: Vec<NextStep>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into().into_boxed_str(),
            cause: cause.map(String::into_boxed_str),
            class,
            setting: None,
            subject: None,
            next,
        }
    }

    fn operation(
        code: impl Into<String>,
        message: impl Into<String>,
        cause: Option<String>,
    ) -> Self {
        Self::new(
            code,
            message,
            cause,
            ErrorClass::Operation,
            vec![NextStep::new(
                Vec::new(),
                "correct the reported condition and rerun the command",
            )],
        )
    }

    fn configured(code: &str, message: String, subject: ErrorSubject, setting: SettingRef) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into_boxed_str(),
            cause: None,
            class: ErrorClass::Configured,
            setting: Some(Box::new(setting)),
            subject: Some(Box::new(subject)),
            next: vec![NextStep::new(
                Vec::new(),
                "check out the manifest head or rerun ci patch without --apply",
            )],
        }
    }

    fn from_command(error: CommandError) -> Self {
        let error = error.into_payload();
        Self {
            code: error.code,
            message: error.message.into_boxed_str(),
            cause: error.cause.map(String::into_boxed_str),
            class: error.class,
            setting: error.setting.map(Box::new),
            subject: error.subject.map(Box::new),
            next: error.next,
        }
    }

    /// The public transport error, carrying the same typed facts the renderer
    /// uses for every other command.
    pub(crate) fn into_command_error(self) -> CommandError {
        let Self {
            code,
            message,
            cause,
            class,
            setting,
            subject,
            next,
        } = self;
        let mut error = CommandError::new(
            code,
            message.into_string(),
            cause.as_deref(),
            "correct the reported condition and rerun the command",
        )
        .with_class(class);
        error.next = next;
        if let Some(setting) = setting {
            error = error.with_setting(*setting);
        }
        if let Some(subject) = subject {
            error = error.with_subject(*subject);
        }
        error
    }
}

/// Verify the artefact against the run it came from and the worktree it would
/// change. Every check is data-only: nothing is applied or written here.
pub(crate) fn verify(
    manifest: &GeneratedManifest,
    patch: &[u8],
    expected: &RunIdentity,
    worktree_head: GitOid,
    subject: &ErrorSubject,
    setting: &SettingRef,
) -> Result<(), PatchError> {
    if let Some(failed) = manifest
        .generators
        .iter()
        .find(|generator| generator.status == GeneratorStatus::Failed)
    {
        return Err(PatchError::operation(
            "CI_PATCH_GENERATOR_FAILED",
            format!(
                "generated patch is unavailable because generator {} failed",
                failed.name
            ),
            None,
        ));
    }
    if manifest.repo != expected.repo {
        return Err(PatchError::operation(
            "CI_PATCH_REPO_MISMATCH",
            format!(
                "manifest names repo {} but the run belongs to {}",
                manifest.repo, expected.repo
            ),
            None,
        ));
    }
    if manifest.workflow != expected.workflow {
        return Err(PatchError::operation(
            "CI_PATCH_WORKFLOW_MISMATCH",
            format!(
                "manifest names workflow {} but the run used {}",
                manifest.workflow, expected.workflow
            ),
            None,
        ));
    }
    if manifest.run_attempt != expected.run_attempt {
        return Err(PatchError::operation(
            "CI_PATCH_RUN_MISMATCH",
            format!(
                "manifest is from run attempt {} but the run is at attempt {}",
                manifest.run_attempt, expected.run_attempt
            ),
            None,
        ));
    }
    if manifest.head_sha != worktree_head {
        return Err(PatchError::configured(
            "CI_PATCH_HEAD_MISMATCH",
            format!(
                "manifest head {} does not match worktree HEAD {}",
                manifest.head_sha, worktree_head
            ),
            subject.clone(),
            setting.clone(),
        ));
    }
    let hash = Sha256::digest(patch);
    let digest = provenant_core::hex(&hash);
    if digest != manifest.patch_sha256 {
        return Err(PatchError::operation(
            "CI_PATCH_DIGEST_MISMATCH",
            format!(
                "patch digest {digest} does not match manifest digest {}",
                manifest.patch_sha256
            ),
            None,
        ));
    }
    Ok(())
}

/// Every path a patch can change, including the source of a rename and the
/// removed side of a deletion. Ambiguous or unsafe paths fail before mutation.
pub(crate) fn patch_paths(patch: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(patch).map_err(|_| "patch headers are not UTF-8")?;
    let mut paths = Vec::new();
    let mut section: Option<&str> = None;
    let mut section_has_file_headers = false;
    let mut hunk_remaining: Option<(usize, usize)> = None;
    let mut pending_old_path: Option<String> = None;
    for line in text.lines() {
        if let Some((old_remaining, new_remaining)) = hunk_remaining {
            if old_remaining == 0 && new_remaining == 0 {
                hunk_remaining = None;
            } else {
                consume_hunk_line(line, &mut hunk_remaining)?;
                continue;
            }
        }
        if let Some(header) = line.strip_prefix("diff --git ") {
            if pending_old_path.is_some() {
                return Err("patch has an unmatched old file header".to_owned());
            }
            if !section_has_file_headers && let Some(header) = section {
                add_paths(&mut paths, parse_diff_header_paths(header)?)?;
            }
            section = Some(header);
            section_has_file_headers = false;
            continue;
        }
        if line.starts_with("@@@ ") {
            return Err("combined diff hunks are not supported".to_owned());
        }
        if line.starts_with("@@ ") {
            if pending_old_path.is_some() {
                return Err("patch has an unmatched old file header".to_owned());
            }
            hunk_remaining = Some(parse_hunk_counts(line)?);
            continue;
        }
        if let Some(header) = line.strip_prefix("--- ") {
            if pending_old_path.is_some() {
                return Err("patch has consecutive old file headers".to_owned());
            }
            pending_old_path = parse_file_header_path(header, b"a/")?;
            if pending_old_path.is_none() {
                // `/dev/null` is the old side of an added file, represented
                // by an empty path while still requiring its paired new side.
                pending_old_path = Some(String::new());
            }
            continue;
        }
        if let Some(header) = line.strip_prefix("+++ ") {
            let Some(old_path) = pending_old_path.take() else {
                return Err("patch has a new file header without an old header".to_owned());
            };
            let new_path = parse_file_header_path(header, b"b/")?;
            section_has_file_headers = true;
            let candidates = if old_path.is_empty() {
                Vec::new()
            } else {
                vec![old_path]
            };
            add_paths(&mut paths, candidates)?;
            if let Some(path) = new_path {
                add_paths(&mut paths, [path])?;
            }
        }
    }
    if pending_old_path.is_some() {
        return Err("patch has an unmatched old file header".to_owned());
    }
    if hunk_remaining.is_some_and(|(old, new)| old != 0 || new != 0) {
        return Err("patch ends before a hunk is complete".to_owned());
    }
    if !section_has_file_headers && let Some(header) = section {
        add_paths(&mut paths, parse_diff_header_paths(header)?)?;
    }
    if paths.is_empty() && !patch.is_empty() {
        return Err("patch contains no unambiguous file paths".to_owned());
    }
    Ok(paths)
}

fn parse_hunk_counts(header: &str) -> Result<(usize, usize), String> {
    let ranges = header
        .strip_prefix("@@ ")
        .and_then(|tail| tail.split_once(" @@").map(|(ranges, _)| ranges))
        .ok_or_else(|| "patch has a malformed hunk header".to_owned())?;
    let mut ranges = ranges.split_ascii_whitespace();
    let old = ranges
        .next()
        .ok_or_else(|| "patch hunk is missing its old range".to_owned())?;
    let new = ranges
        .next()
        .ok_or_else(|| "patch hunk is missing its new range".to_owned())?;
    if ranges.next().is_some() {
        return Err("patch hunk has an unexpected range".to_owned());
    }
    Ok((parse_hunk_count(old, b'-')?, parse_hunk_count(new, b'+')?))
}

fn parse_hunk_count(range: &str, prefix: u8) -> Result<usize, String> {
    let range = range
        .as_bytes()
        .strip_prefix(&[prefix])
        .ok_or_else(|| "patch hunk has an invalid range prefix".to_owned())?;
    let count = range
        .iter()
        .position(|byte| *byte == b',')
        .map_or(&b"1"[..], |index| &range[index + 1..]);
    std::str::from_utf8(count)
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .ok_or_else(|| "patch hunk has an invalid line count".to_owned())
}

fn consume_hunk_line(line: &str, remaining: &mut Option<(usize, usize)>) -> Result<(), String> {
    let Some((old, new)) = remaining.as_mut() else {
        return Err("patch hunk state was lost".to_owned());
    };
    match line.as_bytes().first() {
        Some(b' ') if *old > 0 && *new > 0 => {
            *old -= 1;
            *new -= 1;
        }
        Some(b'-') if *old > 0 => *old -= 1,
        Some(b'+') if *new > 0 => *new -= 1,
        Some(b'\\') if line == "\\ No newline at end of file" => {}
        _ => return Err("patch has an invalid or overlong hunk body".to_owned()),
    }
    Ok(())
}

fn add_paths(
    paths: &mut Vec<String>,
    candidates: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    for path in candidates {
        validate_patch_path(&path)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(())
}

fn validate_patch_path(path: &str) -> Result<(), String> {
    use std::path::Component;

    if path.is_empty()
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        || Path::new(path).components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "patch path is not a safe worktree-relative path: {path:?}"
        ));
    }
    Ok(())
}

fn parse_file_header_path(header: &str, prefix: &[u8]) -> Result<Option<String>, String> {
    let (bytes, suffix_is_valid) = if header.starts_with('"') {
        let (path, trailing) = decode_quoted_path(header.as_bytes())?;
        (path, trailing.is_empty() || trailing.starts_with(b"\t"))
    } else {
        let (path, _) = header
            .split_once('\t')
            .map_or((header, ""), |(path, trailing)| (path, trailing));
        (path.as_bytes().to_vec(), true)
    };
    if !suffix_is_valid {
        return Err("patch file header has trailing data".to_owned());
    }
    parse_prefixed_path(&bytes, prefix)
}

fn parse_diff_header_paths(header: &str) -> Result<Vec<String>, String> {
    let bytes = header.as_bytes();
    let (old, new) = if bytes.starts_with(b"\"") {
        let (old, rest) = decode_quoted_path(bytes)?;
        let Some(new_header) = rest.strip_prefix(b" ") else {
            return Err("quoted diff header has no second path".to_owned());
        };
        if new_header.starts_with(b"\"") {
            let (new, trailing) = decode_quoted_path(new_header)?;
            if !trailing.is_empty() {
                return Err("quoted diff header has trailing data".to_owned());
            }
            (old, new)
        } else {
            (old, new_header.to_vec())
        }
    } else {
        if !bytes.starts_with(b"a/") {
            return Err("diff header does not start with a worktree path".to_owned());
        }
        let separators = bytes
            .windows(3)
            .enumerate()
            .filter(|(_, window)| *window == b" b/")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if separators.len() != 1 {
            return Err("unquoted diff header path is ambiguous".to_owned());
        }
        let split = separators[0];
        (bytes[..split].to_vec(), bytes[split + 1..].to_vec())
    };
    let mut paths = Vec::new();
    for (path, prefix) in [(old, b"a/".as_slice()), (new, b"b/".as_slice())] {
        if let Some(path) = parse_prefixed_path(&path, prefix)? {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn parse_prefixed_path(bytes: &[u8], prefix: &[u8]) -> Result<Option<String>, String> {
    if bytes == b"/dev/null" {
        return Ok(None);
    }
    let Some(path) = bytes.strip_prefix(prefix) else {
        return Err("patch file header has an unexpected path prefix".to_owned());
    };
    let path =
        String::from_utf8(path.to_vec()).map_err(|_| "patch path is not valid UTF-8".to_owned())?;
    validate_patch_path(&path)?;
    Ok(Some(path))
}

fn decode_quoted_path(input: &[u8]) -> Result<(Vec<u8>, &[u8]), String> {
    if !input.starts_with(b"\"") {
        return Err("quoted path does not start with a quote".to_owned());
    }
    let mut decoded = Vec::new();
    let mut index = 1;
    while let Some(&byte) = input.get(index) {
        index += 1;
        match byte {
            b'"' => return Ok((decoded, &input[index..])),
            b'\\' => {
                let Some(&escaped) = input.get(index) else {
                    return Err("quoted path ends in an escape".to_owned());
                };
                index += 1;
                match escaped {
                    b'a' => decoded.push(0x07),
                    b'b' => decoded.push(0x08),
                    b't' => decoded.push(b'\t'),
                    b'n' => decoded.push(b'\n'),
                    b'v' => decoded.push(0x0b),
                    b'f' => decoded.push(0x0c),
                    b'r' => decoded.push(b'\r'),
                    b'\\' | b'"' => decoded.push(escaped),
                    b'0'..=b'7' => {
                        let mut value = u16::from(escaped - b'0');
                        for _ in 0..2 {
                            if input
                                .get(index)
                                .is_some_and(|byte| (b'0'..=b'7').contains(byte))
                            {
                                value = value * 8 + u16::from(input[index] - b'0');
                                index += 1;
                            } else {
                                break;
                            }
                        }
                        let byte = u8::try_from(value)
                            .map_err(|_| "quoted path octal escape exceeds one byte".to_owned())?;
                        decoded.push(byte);
                    }
                    _ => return Err("quoted path uses an unknown escape".to_owned()),
                }
            }
            value => decoded.push(value),
        }
    }
    Err("quoted path has no closing quote".to_owned())
}

/// The filesystem and Git facts `ci patch` needs. The pure verification and
/// apply-plan tests inject a fake; production uses [`SystemRuntime`].
pub(crate) trait PatchRuntime {
    fn head(&self, tree: &Path) -> Result<String, String>;
    fn apply_check(&self, tree: &Path, patch: &[u8]) -> Result<(), String>;
    fn apply(&self, tree: &Path, patch: &[u8]) -> Result<(), String>;
    fn commit(
        &self,
        tree: &Path,
        paths: &[String],
        patch: &[u8],
        expected: GitOid,
        message: &str,
    ) -> Result<(), String>;
    /// The live lease and dirty paths currently recorded in this worktree.
    fn live_holder(
        &self,
        state: &Path,
        common_dir: &Path,
        tree: &Path,
    ) -> Result<Option<LiveWriter>, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveWriter {
    pub holder: AttemptId,
    pub dirty_paths: Vec<String>,
}

/// The inputs to one apply, grouped so the operation stays a small seam.
pub(crate) struct ApplyRequest<'a> {
    pub manifest: &'a GeneratedManifest,
    pub patch: &'a [u8],
    pub expected: &'a RunIdentity,
    pub worktree: &'a Path,
    pub state: &'a Path,
    pub common_dir: &'a Path,
    pub commit: bool,
    pub subject: ErrorSubject,
    pub setting: &'a SettingRef,
}

/// Re-check the head under the runtime, run the destruction guard, then apply
/// with `git apply --check` first so dirty work and unrelated staged files are
/// preserved on any failure.
pub(crate) fn apply_patch(
    runtime: &dyn PatchRuntime,
    mut request: ApplyRequest<'_>,
) -> Result<PatchReport, PatchError> {
    let head_text = runtime.head(request.worktree).map_err(|error| {
        PatchError::operation(
            "CI_PATCH_HEAD_UNREADABLE",
            "cannot read worktree HEAD",
            Some(error),
        )
    })?;
    let head = GitOid::parse(&head_text).map_err(|error| {
        PatchError::operation(
            "CI_PATCH_HEAD_UNREADABLE",
            "worktree HEAD is not a Git object id",
            Some(error.to_string()),
        )
    })?;
    request.subject.head = Some(head);
    verify(
        request.manifest,
        request.patch,
        request.expected,
        head,
        &request.subject,
        request.setting,
    )?;

    let paths = patch_paths(request.patch).map_err(|error| {
        PatchError::operation(
            "CI_PATCH_PATHS_INVALID",
            "cannot safely identify every path changed by the patch",
            Some(error),
        )
    })?;
    let (holder, commit_is_safe) = match runtime.live_holder(
        request.state,
        request.common_dir,
        request.worktree,
    ) {
        Ok(holder) => (holder, true),
        Err(error) => {
            warn(
                Warning::new(
                    provenant_records::warning::WORKTREE_INVENTORY_UNAVAILABLE,
                    format!(
                        "{}: could not inspect live writer custody: {error}; applying patch without committing",
                        provenant_records::warning::WORKTREE_INVENTORY_UNAVAILABLE.0
                    ),
                    detected_source(provenant_records::warning::WORKTREE_INVENTORY_UNAVAILABLE),
                )
                .subject(request.worktree.display().to_string()),
            );
            (None, false)
        }
    };
    let custody = CustodySubject::new(
        SubjectKind::Worktree,
        request.worktree.display().to_string(),
    );
    if let Some(holder) = holder {
        let overlaps = paths.iter().any(|patch_path| {
            holder
                .dirty_paths
                .iter()
                .any(|dirty_path| paths_overlap(patch_path, dirty_path))
        });
        guard_destructive("ci patch apply", &custody, overlaps, Some(holder.holder))
            .map_err(PatchError::from_command)?;
        if !overlaps {
            warn(
                Warning::new(
                    WarningCode(provenant_records::policy::WORKTREE_SHARED),
                    format!(
                        "{}: applying a generated patch to paths disjoint from live attempt {}",
                        provenant_records::policy::WORKTREE_SHARED,
                        holder.holder
                    ),
                    detected_source(WarningCode(provenant_records::policy::WORKTREE_SHARED)),
                )
                .subject(custody.to_string()),
            );
        }
    }

    runtime
        .apply_check(request.worktree, request.patch)
        .map_err(|error| {
            PatchError::operation(
                "CI_PATCH_APPLY_CHECK_FAILED",
                "patch does not apply cleanly; no files were changed",
                Some(error),
            )
        })?;
    runtime
        .apply(request.worktree, request.patch)
        .map_err(|error| {
            PatchError::operation("CI_PATCH_APPLY_FAILED", "cannot apply patch", Some(error))
        })?;

    let committed = request.commit && commit_is_safe;
    if committed {
        runtime
            .commit(
                request.worktree,
                &paths,
                request.patch,
                head,
                COMMIT_MESSAGE,
            )
            .map_err(|error| {
                PatchError::operation(
                    "CI_PATCH_COMMIT_FAILED",
                    "patch applied but the path-limited commit failed",
                    Some(error),
                )
            })?;
    }
    Ok(PatchReport {
        head,
        applied: true,
        committed,
        paths,
    })
}

/// Git execution bound to the selected worktree. The patch travels on stdin,
/// so no scratch copy of it is written to the tree being changed.
pub(crate) struct SystemRuntime;

fn git_bytes(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let argv: Vec<std::ffi::OsString> = std::iter::once("git")
        .chain(args.iter().copied())
        .map(std::ffi::OsString::from)
        .collect();
    let mut options = ExecOptions::new(&argv, cwd, READ_BUDGET);
    options.stdin = stdin;
    options.process_group = true;
    options.stdout = StreamPolicy::Capture {
        limit: 8 * 1024 * 1024,
    };
    options.stderr = StreamPolicy::Capture { limit: 256 * 1024 };
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let outcome = executor
        .execute(options)
        .map_err(|error| error.to_string())?;
    if outcome.termination != Termination::Exit(0) || outcome.timed_out {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            provenant_core::bounded_line(&String::from_utf8_lossy(&outcome.stderr.bytes), 600)
        ));
    }
    Ok(outcome.stdout.bytes)
}

fn git(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    git_bytes(cwd, args, stdin).map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
}

fn status_dirty_paths(status: &[u8]) -> Result<Vec<String>, String> {
    let mut fields = status.split(|byte| *byte == 0);
    let mut paths = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        if entry.len() < 4 || entry[2] != b' ' {
            return Err("Git returned a malformed worktree status record".to_owned());
        }
        let path = String::from_utf8(entry[3..].to_vec())
            .map_err(|_| "live writer has a non-UTF-8 dirty path".to_owned())?;
        if !paths.contains(&path) {
            paths.push(path);
        }
        if entry[..2].contains(&b'R') || entry[..2].contains(&b'C') {
            let original = fields
                .next()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| "Git omitted a rename source path".to_owned())?;
            let original = String::from_utf8(original.to_vec())
                .map_err(|_| "live writer has a non-UTF-8 dirty path".to_owned())?;
            if !paths.contains(&original) {
                paths.push(original);
            }
        }
    }
    Ok(paths)
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let left = Path::new(left);
    let right = Path::new(right);
    left == right || left.starts_with(right) || right.starts_with(left)
}

impl PatchRuntime for SystemRuntime {
    fn head(&self, tree: &Path) -> Result<String, String> {
        git(tree, &["rev-parse", "HEAD"], None)
    }

    fn apply_check(&self, tree: &Path, patch: &[u8]) -> Result<(), String> {
        git(tree, &["apply", "--check"], Some(patch)).map(|_| ())
    }

    fn apply(&self, tree: &Path, patch: &[u8]) -> Result<(), String> {
        git(tree, &["apply"], Some(patch)).map(|_| ())
    }

    fn commit(
        &self,
        tree: &Path,
        paths: &[String],
        patch: &[u8],
        expected: GitOid,
        message: &str,
    ) -> Result<(), String> {
        use provenant_core::{CancelToken, ThreadPause};
        use provenant_os::exec::SystemExecutor;

        let git_dir =
            provenant_os::git::repository_path(tree, provenant_os::git::RepositoryPath::GitDir)
                .map_err(|error| error.to_string())?;
        let ref_name = git(tree, &["symbolic-ref", "--quiet", "HEAD"], None)?;
        let env = std::env::vars_os()
            .filter(|(key, _)| key != "GIT_INDEX_FILE")
            .collect::<Vec<_>>();
        let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
        let cancel = CancelToken::new();
        provenant_run::commit_own_paths(provenant_run::OwnCommitRequest {
            worktree: tree,
            git_dir: &git_dir,
            ref_name: &ref_name,
            expected,
            input: provenant_run::OwnCommitInput::Patch {
                bytes: patch,
                paths,
            },
            message,
            retries: 2,
            env: &env,
            executor: &executor,
            cancel: &cancel,
            pause: &ThreadPause,
            timeout: READ_BUDGET,
        })
        .map(|_| ())
        .map_err(|error| error.into_payload().message)
    }

    fn live_holder(
        &self,
        state: &Path,
        common_dir: &Path,
        tree: &Path,
    ) -> Result<Option<LiveWriter>, String> {
        let location = provenant_config::ProjectLocation::new(common_dir)
            .map_err(|error| error.to_string())?;
        let binding = provenant_config::lookup_project_at_state(state, &location)
            .map_err(|error| error.to_string())?;
        let Some(binding) = binding else {
            return Ok(None);
        };
        let Some(holder) = provenant_confinement::worktrees::recorded_writer_holder(
            state,
            binding.project_id(),
            tree,
        ) else {
            return Ok(None);
        };
        let status = git_bytes(
            tree,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            None,
        )?;
        Ok(Some(LiveWriter {
            holder,
            dirty_paths: status_dirty_paths(&status)?,
        }))
    }
}

/// Find the Actions run that produced a pull request's generated artefact.
fn run_for_pr(
    state: &Path,
    project: &Path,
    repo: &str,
    number: u64,
    request: &provenant_config::ports::Request,
) -> Result<u64, Box<dyn std::error::Error>> {
    use crate::pr::observer::Observer;
    let mut observer = Observer::open(state, project, repo, request)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    let pull = observer.resource(&format!("pulls/{number}"), Duration::ZERO, request)?;
    let head = pull["head"]["sha"]
        .as_str()
        .ok_or("pull request named no head sha")?;
    let endpoint = format!("repos/{repo}/actions/runs?head_sha={head}&per_page=1");
    let bytes = successful(forge_execute(
        state,
        project,
        &["gh", "api", &endpoint],
        request,
        false,
        None,
    )?)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    value["workflow_runs"][0]["id"]
        .as_u64()
        .ok_or_else(|| "no Actions run produced this pull request head".into())
}

fn run_head_sha(meta: &Value) -> Result<&str, &'static str> {
    meta["pull_requests"]
        .as_array()
        .and_then(|pull_requests| pull_requests.first())
        .and_then(|pull_request| pull_request["head"]["sha"].as_str())
        .or_else(|| meta["head_sha"].as_str())
        .ok_or("run metadata named no head sha")
}

/// Read the artefact and the run facts it must agree with through the one
/// sourced forge port.
fn fetch(
    state: &Path,
    project: &Path,
    target: &provenant_cli::ci::PatchTarget,
    request: &provenant_config::ports::Request,
) -> Result<(GeneratedManifest, Vec<u8>, RunIdentity), Box<dyn std::error::Error>> {
    let repo = batch::repository(state, project)?;
    let run = match target {
        provenant_cli::ci::PatchTarget::Run { id } => *id,
        provenant_cli::ci::PatchTarget::Pr { number } => {
            run_for_pr(state, project, &repo, *number, request)?
        }
    };

    let endpoint = format!("repos/{repo}/actions/runs/{run}");
    let bytes = successful(forge_execute(
        state,
        project,
        &["gh", "api", &endpoint],
        request,
        false,
        None,
    )?)?;
    let meta: Value = serde_json::from_slice(&bytes)?;
    let head = run_head_sha(&meta)?;
    let workflow_id = meta["workflow_id"]
        .as_u64()
        .ok_or("run metadata named no workflow id")?;
    let run_attempt = meta["run_attempt"].as_u64().unwrap_or(1) as u32;
    let manifest_repo = meta["repository"]["full_name"]
        .as_str()
        .unwrap_or(&repo)
        .to_owned();

    let workflow_endpoint = format!("repos/{repo}/actions/workflows/{workflow_id}");
    let bytes = successful(forge_execute(
        state,
        project,
        &["gh", "api", &workflow_endpoint],
        request,
        false,
        None,
    )?)?;
    let workflow_meta: Value = serde_json::from_slice(&bytes)?;
    let workflow_path = workflow_meta["path"]
        .as_str()
        .unwrap_or(".github/workflows/ci.yml");
    let workflow = Path::new(workflow_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ci.yml".to_owned());

    // The binary is built without the `fixture` feature, so the fixture-only
    // `tempfile` dependency is not linked here. Use the production scratch
    // directory helper instead: exclusive, owner-only (0700) and auto-removed.
    let scratch =
        crate::scratch_dir::ScratchDir::new_in(&std::env::temp_dir(), "provenant-ci-patch-")?;
    let scratch_path = scratch
        .path()
        .to_str()
        .ok_or("scratch directory is not UTF-8")?;
    let artifact = format!("generated-{head}");
    let id = run.to_string();
    successful(forge_execute(
        state,
        project,
        &[
            "gh",
            "run",
            "download",
            &id,
            "--name",
            &artifact,
            "-R",
            &repo,
            "--dir",
            scratch_path,
        ],
        request,
        false,
        None,
    )?)?;

    let dir = provenant_os::fs::Directory::open_anchor(scratch.path())?;
    let manifest_bytes = dir.read_bounded(OsStr::new("manifest.json"), 1024 * 1024)?;
    let patch = dir.read_bounded(OsStr::new("patch"), 8 * 1024 * 1024)?;
    let manifest: GeneratedManifest = serde_json::from_slice(&manifest_bytes)?;
    Ok((
        manifest,
        patch,
        RunIdentity {
            repo: manifest_repo,
            workflow,
            run_attempt,
        },
    ))
}

fn query_subject(head: GitOid, target: &provenant_cli::ci::PatchTarget) -> ErrorSubject {
    ErrorSubject {
        host: None,
        project: None,
        graph: None,
        node: None,
        attempt: None,
        item: match target {
            provenant_cli::ci::PatchTarget::Run { id } => Some(format!("run:{id}")),
            provenant_cli::ci::PatchTarget::Pr { number } => Some(format!("pr:{number}")),
        },
        pr: match target {
            provenant_cli::ci::PatchTarget::Pr { number } => Some(*number),
            provenant_cli::ci::PatchTarget::Run { .. } => None,
        },
        head: Some(head),
    }
}

fn emit_error(error: PatchError, globals: &Globals) -> u8 {
    let mut result = CommandResult::new(LEAF, Outcome::Error, ());
    result.next = error.next.clone();
    result.error = Some(error.into_command_error());
    crate::emit(&result, globals, "", false)
}

/// `provenant ci patch`: verify the generated artefact, then apply it only
/// under `--apply` and only after the destruction guard, `git apply --check`
/// and the head re-check all pass.
pub(crate) fn run(
    command: provenant_cli::ci::Patch,
    globals: &Globals,
    read_budget: std::time::Duration,
) -> u8 {
    type FetchedPatch = (
        GeneratedManifest,
        Vec<u8>,
        RunIdentity,
        PathBuf,
        PathBuf,
        SettingRef,
    );
    let fetched = (|| -> Result<FetchedPatch, Box<dyn std::error::Error>> {
        let project = crate::roots::selected_project(globals.project.as_deref(), true)?;
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )?;
        let state = resolved.roots.state.as_path();
        let policy = ResolvedGeneratedPatch::read(state)?.permits(command.apply)?;
        let request = provenant_config::ports::Request::within(READ_BUDGET.min(read_budget));
        let (manifest, patch, identity) = fetch(state, &project, &command.target, &request)?;
        let worktree = command
            .worktree
            .as_deref()
            .unwrap_or(project.as_path())
            .to_path_buf();
        Ok((
            manifest,
            patch,
            identity,
            worktree,
            state.to_path_buf(),
            policy.setting,
        ))
    })();
    let (manifest, patch, identity, worktree, state, setting) = match fetched {
        Ok(fetched) => fetched,
        Err(error) => {
            return crate::fail(
                LEAF,
                Outcome::Error,
                "CI_PATCH_FETCH_FAILED",
                &error.to_string(),
                "check the reported forge read error and rerun provenant ci patch",
                globals,
            );
        }
    };

    let common = match git(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        None,
    ) {
        Ok(common) => PathBuf::from(common),
        Err(error) => {
            return crate::fail(
                LEAF,
                Outcome::Error,
                "CI_PATCH_HEAD_UNREADABLE",
                &error,
                "run from a Git worktree or pass --worktree",
                globals,
            );
        }
    };
    let subject = query_subject(manifest.head_sha, &command.target);

    if !command.apply {
        let head_text = match SystemRuntime.head(&worktree) {
            Ok(head) => head,
            Err(error) => {
                return emit_error(
                    PatchError::operation(
                        "CI_PATCH_HEAD_UNREADABLE",
                        "cannot read worktree HEAD",
                        Some(error),
                    ),
                    globals,
                );
            }
        };
        let head = match GitOid::parse(&head_text) {
            Ok(head) => head,
            Err(error) => {
                return emit_error(
                    PatchError::operation(
                        "CI_PATCH_HEAD_UNREADABLE",
                        "worktree HEAD is not a Git object id",
                        Some(error.to_string()),
                    ),
                    globals,
                );
            }
        };
        if let Err(error) = verify(&manifest, &patch, &identity, head, &subject, &setting) {
            return emit_error(error, globals);
        }
        let paths = match patch_paths(&patch) {
            Ok(paths) => paths,
            Err(error) => {
                return emit_error(
                    PatchError::operation(
                        "CI_PATCH_PATHS_INVALID",
                        "cannot safely identify every path changed by the patch",
                        Some(error),
                    ),
                    globals,
                );
            }
        };
        let report = PatchReport {
            head,
            applied: false,
            committed: false,
            paths,
        };
        let human = format!(
            "ci patch {}: verified {} paths at {head} (not applied)",
            report.paths.len(),
            manifest.head_sha
        );
        return crate::emit(
            &CommandResult::new(LEAF, Outcome::Ok, report),
            globals,
            &human,
            true,
        );
    }

    match apply_patch(
        &SystemRuntime,
        ApplyRequest {
            manifest: &manifest,
            patch: &patch,
            expected: &identity,
            worktree: &worktree,
            state: &state,
            common_dir: &common,
            commit: command.commit,
            subject,
            setting: &setting,
        },
    ) {
        Ok(report) => {
            let human = format!(
                "ci patch: applied {} path(s) at {}{}",
                report.paths.len(),
                report.head,
                if report.committed {
                    " and committed"
                } else {
                    ""
                }
            );
            crate::emit(
                &CommandResult::new(LEAF, Outcome::Ok, report),
                globals,
                &human,
                false,
            )
        }
        Err(error) => emit_error(error, globals),
    }
}

/// Kept as executable JSON so the contract fixture can be consumed by the
/// batch-level tests without coupling them to CI or a forge account.
#[cfg(test)]
pub(crate) const MANIFEST_FIXTURE: &str = r#"{
  "repo": "owner/repo",
  "workflow": "ci.yml",
  "run_id": 123,
  "run_attempt": 1,
  "head_sha": "0123456789abcdef0123456789abcdef01234567",
  "generators": [
    {"name": "swift-fixtures", "status": "drift", "files": ["fixtures/a.swift"]}
  ],
  "patch_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn fixture(head: &str) -> (GeneratedManifest, Vec<u8>, RunIdentity, ErrorSubject) {
        let patch = b"diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n"
            .to_vec();
        let hash = Sha256::digest(&patch);
        let digest = provenant_core::hex(&hash);
        let manifest: GeneratedManifest = serde_json::from_str(&format!(
            r#"{{
              "repo": "owner/repo",
              "workflow": "ci.yml",
              "run_id": 123,
              "run_attempt": 1,
              "head_sha": "{head}",
              "generators": [],
              "patch_sha256": "{digest}"
            }}"#
        ))
        .unwrap();
        let identity = RunIdentity {
            repo: "owner/repo".to_owned(),
            workflow: "ci.yml".to_owned(),
            run_attempt: 1,
        };
        let subject = ErrorSubject {
            host: None,
            project: None,
            graph: None,
            node: None,
            attempt: None,
            item: Some("run:123".to_owned()),
            pr: None,
            head: None,
        };
        (manifest, patch, identity, subject)
    }

    fn target() -> provenant_cli::ci::PatchTarget {
        provenant_cli::ci::PatchTarget::Run { id: 123 }
    }

    fn setting_ref() -> SettingRef {
        SettingRef {
            key: GENERATED_PATCH_KEY.to_owned(),
            value: "offer".to_owned(),
            layer: Layer::Default,
            origin: "harness-knobs.json".to_owned(),
        }
    }

    #[test]
    fn manifest_fixture_covers_generated_patch_identity() {
        let manifest: GeneratedManifest = serde_json::from_str(MANIFEST_FIXTURE).unwrap();
        assert_eq!(manifest.repo, "owner/repo");
        assert_eq!(manifest.run_attempt, 1);
        assert_eq!(manifest.generators[0].status, GeneratorStatus::Drift);
        assert_eq!(manifest.generators[0].files, ["fixtures/a.swift"]);
    }

    #[test]
    fn pull_request_run_uses_pr_head_identity_and_falls_back_to_run_head() {
        let pull_request = serde_json::json!({
            "head_sha": "merge-sha",
            "pull_requests": [{"head": {"sha": HEAD}}]
        });
        let pr_head = run_head_sha(&pull_request).unwrap();
        assert_eq!(pr_head, HEAD);
        let push_run = serde_json::json!({"head_sha": HEAD});
        let push_head = run_head_sha(&push_run).unwrap();
        assert_eq!(push_head, HEAD);
    }

    #[test]
    fn failed_generator_manifest_is_refused_by_verification() {
        let (mut manifest, patch, identity, subject) = fixture(HEAD);
        manifest.generators.push(GeneratorManifest {
            name: "writer".into(),
            status: GeneratorStatus::Failed,
            files: Vec::new(),
        });
        let error = verify(
            &manifest,
            &patch,
            &identity,
            GitOid::parse(HEAD).unwrap(),
            &subject,
            &setting_ref(),
        )
        .unwrap_err();
        assert_eq!(error.code, "CI_PATCH_GENERATOR_FAILED");
        assert!(error.message.contains("writer"));
    }

    #[test]
    fn generated_patch_policy_resolves_configured_value_and_source() {
        let state = tempfile::tempdir().unwrap();
        let default = ResolvedGeneratedPatch::read(state.path()).unwrap();
        assert_eq!(default.mode, GeneratedPatchMode::Offer);
        assert_eq!(default.setting.layer, Layer::Default);

        std::fs::write(
            state.path().join("config.toml"),
            "schema_version = 1\n[domains.ci]\ngenerated_patch = \"apply\"\n",
        )
        .unwrap();
        let configured = ResolvedGeneratedPatch::read(state.path()).unwrap();
        assert_eq!(configured.mode, GeneratedPatchMode::Apply);
        assert_eq!(configured.setting.value, "apply");
        assert_eq!(configured.setting.layer, Layer::Host);
        assert!(configured.setting.origin.ends_with("config.toml"));
    }

    #[test]
    fn generated_patch_policy_off_refuses_and_offer_requires_apply() {
        let mut policy = ResolvedGeneratedPatch {
            mode: GeneratedPatchMode::Off,
            setting: setting_ref(),
        };
        policy.setting.value = "off".into();
        assert_eq!(policy.permits(true).unwrap_err().code, "CI_PATCH_DISABLED");

        let policy = ResolvedGeneratedPatch {
            mode: GeneratedPatchMode::Offer,
            setting: setting_ref(),
        };
        assert_eq!(
            policy.clone().permits(false).unwrap_err().code,
            "CI_PATCH_APPLY_REQUIRED"
        );
        assert!(policy.permits(true).is_ok());
    }

    #[test]
    fn patch_paths_cover_deletions_renames_and_quoted_names() {
        let deleted = b"diff --git a/old.txt b/old.txt\n--- a/old.txt\n+++ /dev/null\n";
        assert_eq!(patch_paths(deleted).unwrap(), ["old.txt"]);

        let renamed = b"diff --git a/old.txt b/new.txt\n--- a/old.txt\n+++ b/new.txt\n";
        assert_eq!(patch_paths(renamed).unwrap(), ["old.txt", "new.txt"]);

        let quoted = b"diff --git \"a/old\\tname\" \"b/new\\tname\"\n--- \"a/old\\tname\"\n+++ \"b/new\\tname\"\n";
        assert_eq!(patch_paths(quoted).unwrap(), ["old\tname", "new\tname"]);
    }

    #[test]
    fn patch_paths_reject_traversal_and_ambiguous_headers() {
        let traversal =
            b"diff --git a/../outside b/../outside\n--- a/../outside\n+++ b/../outside\n";
        assert!(patch_paths(traversal).is_err());

        let ambiguous = b"diff --git a/foo b/bar b/baz\nold mode 100644\nnew mode 100644\n";
        assert!(patch_paths(ambiguous).is_err());
    }

    #[test]
    fn patch_paths_ignore_header_like_lines_inside_hunks() {
        let patch = b"diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n--- a/evil.txt\n+++ b/evil.txt\n";
        assert_eq!(patch_paths(patch).unwrap(), ["src/a.rs"]);
    }

    #[test]
    fn dirty_path_status_keeps_leading_status_column_and_rename_source() {
        let status = b" M src/modified.rs\0R  src/new.rs\0src/old.rs\0?? untracked file\0";
        assert_eq!(
            status_dirty_paths(status).unwrap(),
            [
                "src/modified.rs",
                "src/new.rs",
                "src/old.rs",
                "untracked file"
            ]
        );
    }

    #[test]
    fn ci_patch_refuses_head_or_run_mismatch() {
        let (manifest, patch, identity, subject) = fixture(HEAD);
        let setting = setting_ref();
        let head = GitOid::parse(HEAD).unwrap();
        assert!(verify(&manifest, &patch, &identity, head, &subject, &setting).is_ok());

        // Head mismatch is a Configured error naming both heads, with a next
        // step and a setting so the error catalogue contract holds.
        let other = GitOid::parse(OTHER).unwrap();
        let error = verify(&manifest, &patch, &identity, other, &subject, &setting).unwrap_err();
        assert_eq!(error.code, "CI_PATCH_HEAD_MISMATCH");
        assert_eq!(error.class, ErrorClass::Configured);
        assert!(error.message.contains(HEAD));
        assert!(error.message.contains(OTHER));
        assert!(!error.next.is_empty());
        assert_eq!(error.setting.as_ref().unwrap().key, GENERATED_PATCH_KEY);

        // Repo, workflow and run-attempt mismatches carry their own codes.
        let wrong_repo = RunIdentity {
            repo: "other/repo".into(),
            ..identity.clone()
        };
        assert_eq!(
            verify(&manifest, &patch, &wrong_repo, head, &subject, &setting)
                .unwrap_err()
                .code,
            "CI_PATCH_REPO_MISMATCH"
        );
        let wrong_workflow = RunIdentity {
            workflow: "other.yml".into(),
            ..identity.clone()
        };
        assert_eq!(
            verify(&manifest, &patch, &wrong_workflow, head, &subject, &setting)
                .unwrap_err()
                .code,
            "CI_PATCH_WORKFLOW_MISMATCH"
        );
        let wrong_attempt = RunIdentity {
            run_attempt: 2,
            ..identity
        };
        assert_eq!(
            verify(&manifest, &patch, &wrong_attempt, head, &subject, &setting)
                .unwrap_err()
                .code,
            "CI_PATCH_RUN_MISMATCH"
        );
    }

    #[test]
    fn patch_error_stays_small_and_preserves_cause() {
        assert!(std::mem::size_of::<PatchError>() < 128);
        let error = PatchError::operation("CI_PATCH_FAILED", "failed", Some("detail".into()))
            .into_command_error();
        assert_eq!(error.cause.as_deref(), Some("detail"));
    }

    #[test]
    fn patch_error_transport_preserves_configured_failure() {
        let (manifest, patch, identity, subject) = fixture(HEAD);
        let setting = setting_ref();
        let error = verify(
            &manifest,
            &patch,
            &identity,
            GitOid::parse(OTHER).unwrap(),
            &subject,
            &setting,
        )
        .unwrap_err()
        .into_command_error();
        assert_eq!(error.code, "CI_PATCH_HEAD_MISMATCH");
        assert_eq!(error.class, ErrorClass::Configured);
        assert!(error.message.contains(HEAD));
        assert!(error.message.contains(OTHER));
        assert_eq!(error.setting.as_ref().unwrap().key, GENERATED_PATCH_KEY);
        assert!(error.subject.is_some());
        assert!(!error.next.is_empty());
    }

    struct Fake {
        head: String,
        check_error: Option<String>,
        applied: RefCell<Vec<Vec<u8>>>,
        committed: RefCell<Vec<(Vec<String>, String)>>,
        holder: Option<AttemptId>,
        dirty_paths: Vec<String>,
    }

    impl PatchRuntime for Fake {
        fn head(&self, _: &Path) -> Result<String, String> {
            Ok(self.head.clone())
        }
        fn apply_check(&self, _: &Path, _: &[u8]) -> Result<(), String> {
            match &self.check_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
        fn apply(&self, _: &Path, patch: &[u8]) -> Result<(), String> {
            self.applied.borrow_mut().push(patch.to_vec());
            Ok(())
        }
        fn commit(
            &self,
            _: &Path,
            paths: &[String],
            _: &[u8],
            _: GitOid,
            message: &str,
        ) -> Result<(), String> {
            self.committed
                .borrow_mut()
                .push((paths.to_vec(), message.to_owned()));
            Ok(())
        }
        fn live_holder(&self, _: &Path, _: &Path, _: &Path) -> Result<Option<LiveWriter>, String> {
            Ok(self.holder.map(|holder| LiveWriter {
                holder,
                dirty_paths: self.dirty_paths.clone(),
            }))
        }
    }

    fn fake(head: &str, check_error: Option<String>) -> Fake {
        Fake {
            head: head.to_owned(),
            check_error,
            applied: RefCell::new(Vec::new()),
            committed: RefCell::new(Vec::new()),
            holder: None,
            dirty_paths: Vec::new(),
        }
    }

    #[test]
    fn ci_patch_preserves_dirty_and_unrelated_staged() {
        let (manifest, patch, identity, subject) = fixture(HEAD);
        let setting = setting_ref();
        let tree = Path::new("/worktree");
        let state = Path::new("/state");
        let common = Path::new("/common");

        // An unclean tree fails `git apply --check`: no apply, no commit, and
        // the error says local work was not touched.
        let dirty = fake(HEAD, Some("local changes would be overwritten".into()));
        let error = apply_patch(
            &dirty,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: true,
                subject: subject.clone(),
                setting: &setting,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "CI_PATCH_APPLY_CHECK_FAILED");
        assert!(error.message.contains("no files were changed"));
        assert!(dirty.applied.borrow().is_empty());
        assert!(dirty.committed.borrow().is_empty());

        // A clean tree applies, and --commit uses a pathspec limited to the
        // patch's own paths, leaving unrelated staged files alone.
        let clean = fake(HEAD, None);
        let report = apply_patch(
            &clean,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: true,
                subject,
                setting: &setting,
            },
        )
        .unwrap();
        assert_eq!(report.paths, ["src/a.rs"]);
        assert!(report.committed && report.applied);
        assert_eq!(clean.applied.borrow().len(), 1);
        assert_eq!(clean.committed.borrow()[0].0, vec!["src/a.rs".to_owned()]);

        // A live holder's work refuses before any Git mutation.
        let mut held = fake(HEAD, None);
        held.holder = Some(AttemptId::from_bytes([7; 16]));
        held.dirty_paths = vec!["src/a.rs".to_owned()];
        let error = apply_patch(
            &held,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: false,
                subject: query_subject(GitOid::parse(HEAD).unwrap(), &target()),
                setting: &setting,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "DESTROYS_LIVE_WORK");
        assert!(held.applied.borrow().is_empty());
    }

    #[test]
    fn ci_patch_allows_disjoint_live_work_and_refuses_overlap() {
        let (manifest, patch, identity, subject) = fixture(HEAD);
        let setting = setting_ref();
        let tree = Path::new("/worktree");
        let state = Path::new("/state");
        let common = Path::new("/common");
        let holder = AttemptId::from_bytes([8; 16]);

        let mut disjoint = fake(HEAD, None);
        disjoint.holder = Some(holder);
        disjoint.dirty_paths = vec!["src/b.rs".to_owned()];
        let report = apply_patch(
            &disjoint,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: true,
                subject: subject.clone(),
                setting: &setting,
            },
        )
        .unwrap();
        assert!(report.applied && report.committed);
        assert_eq!(
            disjoint.committed.borrow()[0].0,
            vec!["src/a.rs".to_owned()]
        );

        let mut overlapping = fake(HEAD, None);
        overlapping.holder = Some(holder);
        overlapping.dirty_paths = vec!["src/a.rs".to_owned()];
        let error = apply_patch(
            &overlapping,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: true,
                subject,
                setting: &setting,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "DESTROYS_LIVE_WORK");
        assert!(overlapping.applied.borrow().is_empty());
        assert!(overlapping.committed.borrow().is_empty());
    }
}
