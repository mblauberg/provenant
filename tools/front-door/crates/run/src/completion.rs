//! Settlement snapshots of the shared handoff, independent of provider history.
use crate::{
    artifacts::{read_sealed, read_settled, seal},
    *,
};
use provenant_os::fs::Directory;
use provenant_records::{
    AttemptFile, Layer, Source,
    handoff::{HANDOFF_BYTES, Handoff, HandoffError, HandoffResult},
    run::{PathScope, WriteSource},
    warning::{CONFIG_MALFORMED, CONFIG_UNAVAILABLE, PORT_STATIC_CHECK_INVALID, Warning, warn},
};
use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};
use toml_edit::Item;

/// A handoff was accepted with an advisory parse notice.
/// Spec: docs/specs/cli.md#warnings-are-typed-values
fn handoff_advisory(attempt: provenant_core::AttemptId, detail: impl Into<String>) {
    use provenant_records::warning::{HANDOFF_PARSE_NOTICE, Warning, warn};
    warn(Warning::new(HANDOFF_PARSE_NOTICE, detail.into()).subject(format!("attempt {attempt}")));
}

pub(crate) fn guidance() -> &'static str {
    static GUIDANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    GUIDANCE.get_or_init(|| {
        [
            "Work until the brief is finished. Do not end your turn because a slice is done, a commit is a natural checkpoint or the work is large; start the next item. Ending your turn ends your work, so a report of what remains is not progress. End your final answer with exactly one fenced handoff block, with nothing after it. Use schema_version 2 and status done, blocked or stopped_early; include note, remaining[], decisions[] and verify[] (verification performed and results). When a criterion is wrong, add spec_concerns entries with criterion, claim, evidence, proposed_change and status (as_written, diverged or stopped); disclose any divergence. A write scope names the expected files, not a fence: when the fix needs another file, edit it minimally and list each such path in decisions; only a strict WRITE_SCOPE lane policy refuses those paths at settlement.",
            provenant_records::handoff::STATUS_GUIDANCE,
            "The JSON is at most 12288 bytes; note and each array entry are 1..4096 bytes, each array at most 64 strings. Example:\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"what was done\",\"remaining\":[],\"decisions\":[],\"verify\":[\"check and result\"]}\n```"
        ]
        .join(" ")
    })
}

impl RunStore {
    /// Read the host's immutable settlement snapshot, never provider history.
    pub fn handoff(&self, record: &AttemptRecord) -> Result<HandoffResult> {
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        match read_snapshot(&attempt)? {
            Some(handoff) => with_scope_report(&attempt, handoff),
            None => Ok(Ok(None)),
        }
    }

    pub(crate) fn capture_handoff(
        &self,
        attempt: &Directory,
        record: &AttemptRecord,
        decoded: &Decoded,
    ) -> Result<provenant_records::handoff::HandoffParse> {
        // Settlement owns the snapshot slot. A provider that finds its scratch
        // inside the attempt directory can write there before it exits; bytes
        // that are not a settled envelope are evidence, never a result (#3844).
        set_aside_foreign_snapshot(attempt, record)?;
        let parsed = decoded.handoff_report();
        if let Some(repeated) =
            provenant_records::handoff::Handoff::repeated_blocks(&decoded.result)
            && parsed.handoff.as_ref().is_ok_and(Option::is_some)
        {
            // Accepted either way: the lane did its work and declared it.
            handoff_advisory(record.attempt_id, repeated.warning());
        }
        if let Some(fallback) = provenant_records::handoff::Handoff::json_fence_fallback(
            &decoded.result,
            &decoded.turns,
        ) {
            // Accepted: the answer ended in a handoff-shaped json block.
            handoff_advisory(record.attempt_id, fallback.warning());
        }
        if let Ok(Some(handoff)) = &parsed.handoff
            && let Some(warning) = handoff.blocked_on_warning()
        {
            // Accepted: the field is additive, so the handoff stands without it.
            handoff_advisory(record.attempt_id, warning);
        }
        if let Some(handoff) = read_snapshot(attempt)? {
            let handoff = with_scope_report(attempt, handoff)?;
            return Ok(provenant_records::handoff::HandoffParse {
                trailing_verdict_tolerated: parsed.trailing_verdict_tolerated
                    && handoff.as_ref().is_ok_and(Option::is_some),
                trailing_prose_tolerated: parsed.trailing_prose_tolerated
                    && handoff.as_ref().is_ok_and(Option::is_some),
                handoff,
            });
        }
        let interrupted = !decoded.complete
            || matches!(
                decoded.outcome,
                Outcome::QuotaLimited
                    | Outcome::ModelUnavailable
                    | Outcome::Failed
                    | Outcome::TimedOut
                    | Outcome::Lost
            );
        let handoff = match parsed.handoff {
            Ok(None) if interrupted => self
                .progress_handoff(attempt, record)
                .unwrap_or_else(|_| Err(HandoffError::new("$", "progress handoff unreadable"))),
            // A malformed final declaration is evidence even on interruption.
            // Never substitute an older progress declaration for it.
            handoff => handoff,
        };
        if handoff != Ok(None) {
            seal_snapshot(
                attempt,
                &serde_json::to_vec(&handoff)
                    .map_err(|_| RunError::Validation("handoff encoding"))?,
            )?;
        }
        let settled = handoff.as_ref().is_ok_and(Option::is_some);
        let trailing_verdict_tolerated = parsed.trailing_verdict_tolerated && settled;
        let trailing_prose_tolerated = parsed.trailing_prose_tolerated && settled;
        Ok(provenant_records::handoff::HandoffParse {
            handoff,
            trailing_verdict_tolerated,
            trailing_prose_tolerated,
        })
    }

    fn progress_handoff(
        &self,
        attempt: &Directory,
        record: &AttemptRecord,
    ) -> Result<HandoffResult> {
        let expected = self
            .context
            .state_root
            .as_path()
            .join("attempts")
            .join(record.attempt_id.to_string())
            .join("provider-state");
        let mut grants = record
            .write_grants
            .iter()
            .filter(|grant| grant.source == WriteSource::PrivateState);
        let Some(grant) = grants.next() else {
            return Ok(Ok(None));
        };
        if grants.next().is_some() || grant.scope != PathScope::Subtree {
            return Ok(Ok(None));
        }
        // The applied grant pins the launched candidate. Never scan other
        // candidates, follow paths from model text or open provider credentials.
        let granted = grant.path.to_path_buf()?;
        let relative = match granted.strip_prefix(&expected) {
            Ok(relative) => relative,
            Err(_) => return Ok(Ok(None)),
        };
        let mut parts = relative.components();
        let Some(Component::Normal(candidate)) = parts.next() else {
            return Ok(Ok(None));
        };
        if parts.next().is_some() {
            return Ok(Ok(None));
        }
        let state = match attempt.open_verified_directory(OsStr::new("provider-state")) {
            Ok(state) => state,
            Err(error) if error.is_not_found() => return Ok(Ok(None)),
            Err(error) => return Err(error.into()),
        };
        let candidate = match state.open_verified_directory(candidate) {
            Ok(candidate) => candidate,
            Err(error) if error.is_not_found() => return Ok(Ok(None)),
            Err(error) => return Err(error.into()),
        };
        let Some(bytes) = read_settled(&candidate, AttemptFile::Handoff, HANDOFF_BYTES as u64)?
        else {
            return Ok(Ok(None));
        };
        Ok(Handoff::from_json(&bytes).map(Some))
    }
}

/// Attach the sealed write-scope evidence to the in-memory handoff view. The
/// original `handoff.json` is an immutable provider snapshot, so derived scope
/// facts are never resealed over it. Oversized full reports fall back to a
/// pointer to the artifact; if that also exceeds the handoff bound, return the
/// validation error so the scope evidence is never silently omitted.
/// A sealed declaration with the host's scope evidence attached; malformed or
/// absent outcomes pass through unchanged.
fn with_scope_report(attempt: &Directory, handoff: HandoffResult) -> Result<HandoffResult> {
    Ok(match handoff {
        Ok(Some(handoff)) => Ok(Some(attach_scope_report(attempt, &handoff)?)),
        other => other,
    })
}

pub(crate) fn attach_scope_report(attempt: &Directory, handoff: &Handoff) -> Result<Handoff> {
    let Some(raw) = read_sealed(
        attempt,
        "policy-write-scope.json",
        provenant_records::run::TERMINAL_RECORD_LIMIT,
    )?
    else {
        return Ok(handoff.clone());
    };
    let report: provenant_records::handoff::WriteScopeReport = crate::db::decode(&raw)?;
    let mut full = handoff.clone();
    full.write_scope = Some(report.clone());
    if let Ok(validated) = validate_handoff(&full) {
        return Ok(validated);
    }

    let mut compact = handoff.clone();
    compact.write_scope = Some(report.compact());
    validate_handoff(&compact)
}

fn validate_handoff(handoff: &Handoff) -> Result<Handoff> {
    let bytes =
        serde_json::to_vec(handoff).map_err(|_| RunError::Validation("handoff encoding"))?;
    Handoff::from_json(&bytes).map_err(RunError::Handoff)
}

// The envelope includes a typed error and a retained partial object, in
// addition to the bounded provider payload. This is not the input byte cap.
const HANDOFF_RECORD_BYTES: usize = HANDOFF_BYTES * 4;

// Settlement alone publishes the attempt's handoff snapshot. The bytes are a
// `HandoffResult` envelope; a lane that writes its own bare handoff into the
// slot produces different bytes, so the shape is the provenance (#3844).

/// Every host write of `<attempt>/handoff.json` goes through here. The sealed
/// publish is the whole settlement act: a resume after a crash either finds
/// these bytes or writes them, and an earlier build's snapshot is the same
/// envelope shape.
pub(crate) fn seal_snapshot(attempt: &Directory, bytes: &[u8]) -> Result<()> {
    seal(
        attempt,
        AttemptFile::Handoff.name(),
        bytes,
        HANDOFF_RECORD_BYTES as u64,
    )
}

fn is_settlement_snapshot(bytes: &[u8]) -> bool {
    serde_json::from_slice::<HandoffResult>(bytes).is_ok()
}

/// An entry in the snapshot slot that settlement cannot read safely. The
/// provider is gone at settlement, so the entry cannot change; retrying a typed
/// read error forever would strand a finished attempt (#3844). Kind, link count
/// and size are judged from a no-follow stat, so the rename never follows a
/// link or reads the bytes. A regular, singly linked entry within the bound is
/// left to `read_settled`, which still reports transient IO as retriable and
/// repairs an over-broad mode on a provider-written input.
fn unsafe_snapshot_entry(meta: provenant_os::fs::Metadata) -> bool {
    meta.kind != provenant_os::fs::FileKind::Regular
        || meta.links != 1
        || meta.size > HANDOFF_RECORD_BYTES as u64
}

/// A non-regular entry is renamed to the declared unsafe evidence name so a
/// symlink, directory or other type is not published under a `.json` name. Both
/// names are declared in `AttemptFile`, so disk reporting and the external run
/// file list classify them (#3844).
fn snapshot_evidence_name(meta: provenant_os::fs::Metadata) -> &'static str {
    if meta.kind == provenant_os::fs::FileKind::Regular {
        AttemptFile::HandoffProviderWritten.name()
    } else {
        AttemptFile::HandoffProviderWrittenUnsafe.name()
    }
}

/// An evidence destination can take the rename when it is absent or a regular
/// file that `replace` overwrites atomically. A directory, symlink or other
/// occupant cannot be replaced by `renameat(Replace)` and must not strand a
/// finished attempt (#3844).
fn evidence_destination_usable(attempt: &Directory, name: &str) -> Result<bool> {
    match attempt.metadata_at(OsStr::new(name), false) {
        Ok(meta) => Ok(meta.kind == provenant_os::fs::FileKind::Regular),
        Err(error) if error.is_not_found() => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// Rename the slot entry aside without reading it, then warn that the bytes
/// were provider-written. The preferred declared name gives way to the unsafe
/// evidence name when its destination is occupied by a non-regular entry: a
/// lane-created directory at the preferred name cannot strand capture. Only
/// when both declared destinations are unusable does the typed error escape
/// (#3844).
fn retain_foreign_snapshot(
    attempt: &Directory,
    record: &AttemptRecord,
    source: provenant_os::fs::Metadata,
) -> Result<()> {
    let preferred = snapshot_evidence_name(source);
    let fallback = if preferred == AttemptFile::HandoffProviderWritten.name() {
        AttemptFile::HandoffProviderWrittenUnsafe.name()
    } else {
        AttemptFile::HandoffProviderWritten.name()
    };
    let retained = if evidence_destination_usable(attempt, preferred)? {
        preferred
    } else if evidence_destination_usable(attempt, fallback)? {
        fallback
    } else {
        return Err(RunError::Validation(
            "handoff evidence destinations are unusable",
        ));
    };
    attempt.replace(
        OsStr::new(AttemptFile::Handoff.name()),
        attempt,
        OsStr::new(retained),
    )?;
    attempt.sync()?;
    handoff_advisory(
        record.attempt_id,
        format!(
            "provider-written handoff.json kept as {retained}; the sealed snapshot comes from captured output"
        ),
    );
    Ok(())
}

/// Keep an entry that landed in the slot but is not a settlement snapshot as
/// evidence and clear it before captured output is sealed (#3844). An unsafe
/// entry is moved aside by rename without following or reading it: the provider
/// is gone and cannot clear it, so it must not retry forever. Only a safe entry
/// whose bytes were actually read and do not decode as a `HandoffResult` is
/// moved aside; a transient read IO failure keeps its typed `RunError`.
fn set_aside_foreign_snapshot(attempt: &Directory, record: &AttemptRecord) -> Result<()> {
    let slot = OsStr::new(AttemptFile::Handoff.name());
    let meta = match attempt.metadata_at(slot, false) {
        Ok(meta) => meta,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if unsafe_snapshot_entry(meta) {
        return retain_foreign_snapshot(attempt, record, meta);
    }
    let Some(bytes) = read_settled(attempt, AttemptFile::Handoff, HANDOFF_RECORD_BYTES as u64)?
    else {
        return Ok(());
    };
    if is_settlement_snapshot(&bytes) {
        return Ok(());
    }
    retain_foreign_snapshot(attempt, record, meta)
}

fn read_snapshot(attempt: &Directory) -> Result<Option<HandoffResult>> {
    let Some(bytes) = read_settled(attempt, AttemptFile::Handoff, HANDOFF_RECORD_BYTES as u64)?
    else {
        return Ok(None);
    };
    // Only bytes that were read and do not decode are a malformed snapshot.
    // A failure to read the slot keeps its typed, retriable `RunError`.
    let handoff: HandoffResult = match serde_json::from_slice(&bytes) {
        Ok(handoff) => handoff,
        Err(_) => {
            return Ok(Some(Err(HandoffError::new(
                "$",
                "handoff snapshot unreadable",
            ))));
        }
    };
    Ok(Some(match handoff {
        Ok(Some(handoff)) => Handoff::from_json(
            &serde_json::to_vec(&handoff).map_err(|_| RunError::Validation("handoff encoding"))?,
        )
        .map(Some),
        other => other,
    }))
}

/// The verification and slot-wait rules every writer lane is told. A writer's `cargo` commands
/// share the host's build cap, and a lane that
/// stopped to report the wait cost a whole task (#2484).
pub const BUILD_SLOT_GUIDANCE: &str = "Build slots: `cargo` commands share the `build` admission class cap (source: default; `[admission.build].slots`), derived from cores and memory. Busy or unavailable slots wait a bounded time, then warn and the build proceeds. 0 disables the cap. Cargo manages its own build locks.\n";

/// Host build cap stated in the preamble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildCap {
    pub slots: u64,
    pub source: &'static str,
}

impl From<&provenant_config::ResolvedAdmissionClass> for BuildCap {
    fn from(class: &provenant_config::ResolvedAdmissionClass) -> Self {
        Self {
            slots: class.slots,
            source: class.slots_source.description(),
        }
    }
}

/// Where a writer lane works: its bound worktree, branch and base, and the
/// primary checkout it must leave alone. Harnesses such as opencode name the
/// primary checkout as the workspace root of a linked worktree, and an absolute
/// path there outranks a brief that only says "your worktree" (#3611).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneLocation {
    pub worktree: std::path::PathBuf,
    pub branch: String,
    pub base: String,
    pub primary_checkout: Option<std::path::PathBuf>,
}

impl LaneLocation {
    /// The paragraph every writer lane is told, whatever its adapter.
    pub fn preamble(&self) -> String {
        let branch = self
            .branch
            .strip_prefix("refs/heads/")
            .unwrap_or(&self.branch);
        let primary = self
            .primary_checkout
            .as_ref()
            .map(|path| {
                format!(
                    " The primary checkout {} is not your working copy: never run commands, edit files, create branches or commit there, even when your environment or workspace banner names it as the working directory, workspace root or project root.",
                    path.display()
                )
            })
            .unwrap_or_default();
        format!(
            "Lane location: your working copy is the bound worktree {worktree} on branch {branch}, started from base {base}. Set the working directory of every shell command to {worktree}, edit files only under it, and commit on {branch}.{primary} An absolute path elsewhere in your context does not override this.\n",
            worktree = self.worktree.display(),
            base = self.base,
        )
    }
}

/// The roles a layered brief-preamble file may scope itself to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BriefRole {
    #[default]
    Writer,
    Review,
    ReadOnly,
}

impl BriefRole {
    /// The one spelling of each role in a `<!-- provenant-roles: -->` header.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "writer" => Some(Self::Writer),
            "review" => Some(Self::Review),
            "read-only" => Some(Self::ReadOnly),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Review => "review",
            Self::ReadOnly => "read-only",
        }
    }
}

/// One layer of the standing brief preamble, with the exact file it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefLayer {
    pub source: Source,
    /// Roles this layer is scoped to. Empty means every role.
    pub roles: Vec<BriefRole>,
    pub text: String,
}

/// One project-declared no-build check for a writer to run before pushing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefStaticCheck {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_seconds: u64,
}

/// The layered standing rules and the project's static checks, carried into
/// every lane's prompt through [`PreambleExtras`].
#[derive(Clone, Debug, Default)]
pub struct BriefPreamble {
    /// Layers in Host, Project, Launch order. Every present layer is kept: the
    /// family concatenates rather than choosing a winner.
    pub layers: Vec<BriefLayer>,
    pub static_checks: Vec<BriefStaticCheck>,
    pub static_checks_source: Option<Source>,
}

/// The registry knob that names a preamble file in each config layer.
pub const BRIEF_PREAMBLE_KNOB: &str = "lanes.brief_preamble";
const ROLE_HEADER_OPEN: &str = "<!-- provenant-roles:";
const ROLE_HEADER_CLOSE: &str = "-->";

/// Read `lanes.brief_preamble` from one config document, accepting both the
/// quoted dotted key and the nested `[knobs.lanes]` form.
fn brief_preamble_knob(text: &str) -> Option<String> {
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    let knobs = document.get("knobs")?.as_table_like()?;
    if let Some(path) = knobs
        .get(BRIEF_PREAMBLE_KNOB)
        .and_then(Item::as_value)
        .and_then(|value| value.as_str())
    {
        return Some(path.to_owned());
    }
    knobs
        .get("lanes")
        .and_then(Item::as_table_like)?
        .get("brief_preamble")
        .and_then(Item::as_value)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// The roles declared in a first-line header. An unrecognised role warns and
/// is ignored; a file with no header applies to every role.
fn brief_roles(text: &str, origin: &str) -> Vec<BriefRole> {
    let Some(first) = text.lines().next() else {
        return Vec::new();
    };
    let first = first.trim();
    let Some(rest) = first.strip_prefix(ROLE_HEADER_OPEN) else {
        return Vec::new();
    };
    let Some(list) = rest.strip_suffix(ROLE_HEADER_CLOSE) else {
        return Vec::new();
    };
    let mut roles = Vec::new();
    for token in list
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        match BriefRole::parse(token) {
            Some(role) if !roles.contains(&role) => roles.push(role),
            Some(_) => {}
            None => warn(
                Warning::new(
                    CONFIG_MALFORMED,
                    format!("unknown brief preamble role {token:?}; expected writer, review or read-only"),
                )
                .subject(origin.to_owned()),
            ),
        }
    }
    roles
}

fn without_role_header(text: &str) -> &str {
    let Some(first) = text.lines().next() else {
        return text;
    };
    if first.trim().starts_with(ROLE_HEADER_OPEN) && first.trim().ends_with(ROLE_HEADER_CLOSE) {
        text[first.len()..].trim_start_matches(['\r', '\n'])
    } else {
        text
    }
}

impl BriefPreamble {
    /// Read the three layers in Host, Project, Launch order. The host layer's
    /// path comes from `<state>/config.toml`, the project layer's from
    /// `<project>/.provenant/config.toml`, and the launch layer's from
    /// `--preamble-file`. A missing or unreadable file warns through the typed
    /// sink and is skipped; the other layers still reach the preamble.
    pub fn read(host_config: Option<&Path>, project: Option<&Path>, launch: Option<&Path>) -> Self {
        let mut layers = Vec::new();
        let mut add = |layer: Layer, path: &Path| {
            let origin = path.display().to_string();
            match std::fs::read_to_string(path) {
                Ok(text) => layers.push(BriefLayer {
                    source: Source {
                        layer,
                        origin: Some(origin.clone()),
                    },
                    roles: brief_roles(&text, &origin),
                    text: without_role_header(&text).to_owned(),
                }),
                Err(error) => warn(
                    Warning::new(
                        CONFIG_UNAVAILABLE,
                        format!("brief preamble file {origin} could not be read: {error}; this layer is skipped and the launch proceeds"),
                    )
                    .subject(origin.clone())
                    .source(Source {
                        layer,
                        origin: Some(origin),
                    }),
                ),
            }
        };
        if let Some(config) = host_config
            && let Ok(text) = std::fs::read_to_string(config)
            && let Some(name) = brief_preamble_knob(&text)
        {
            add(Layer::Host, Path::new(name.as_str()));
        }
        if let Some(project) = project
            && let Ok(text) =
                std::fs::read_to_string(project.join(".provenant").join("config.toml"))
            && let Some(name) = brief_preamble_knob(&text)
        {
            add(Layer::Project, &project.join(name));
        }
        if let Some(path) = launch {
            add(Layer::Launch, path);
        }
        Self {
            layers,
            ..Default::default()
        }
    }

    /// The applicable layers, each labelled with its source, in layer order.
    fn render(&self, role: BriefRole) -> String {
        let mut text = String::new();
        for layer in &self.layers {
            if !layer.roles.is_empty() && !layer.roles.contains(&role) {
                continue;
            }
            text.push_str(&format!(
                "Brief preamble from {} ({}):\n{}\n",
                layer.source.layer.description(),
                layer.source.origin.as_deref().unwrap_or("unknown source"),
                layer.text.trim_end()
            ));
        }
        text
    }

    /// The project's no-build checks, added only to a writer's preamble.
    fn static_checks_paragraph(&self) -> String {
        if self.static_checks.is_empty() {
            return String::new();
        }
        let source = self.static_checks_source.as_ref().map(|source| {
            format!(
                "{}; {}",
                source.layer.description(),
                source.origin.as_deref().unwrap_or(".provenant/ports.json")
            )
        });
        let source = source.unwrap_or_else(|| "project; .provenant/ports.json".to_owned());
        let checks = self
            .static_checks
            .iter()
            .map(|check| {
                format!(
                    "`{}` in {} (cap {}s)",
                    check.argv.join(" "),
                    check.cwd.display(),
                    check.timeout_seconds
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "Static checks: before pushing, run the project's no-build checks (source: {source}): {checks}. Report any failure in the handoff; a failure does not refuse the push and CI still gates the merge.\n"
        )
    }

    /// Copy the project's `verify.static_checks` into the writer preamble.
    /// Only a writer renders the `Static checks:` paragraph, so a review or
    /// read-only lane neither reads nor validates the list: invalid entries
    /// there are not its advice and must not warn. Invalid declarations are
    /// advice, not a gate: each one is dropped with a typed warning rather
    /// than refusing the launch, and the surviving checks are labelled with
    /// the layer and key that declared them.
    pub fn set_static_checks_from_ports(
        &mut self,
        ports: &provenant_config::PortTable,
        role: BriefRole,
    ) {
        if role != BriefRole::Writer {
            return;
        }
        let origin = format!("{}#verify.static_checks", provenant_config::PORTS_CONFIG);
        let source = Source {
            layer: ports
                .provenance()
                .get("verify.static_checks")
                .copied()
                .unwrap_or(ports.verify.source),
            origin: Some(origin.clone()),
        };
        let mut checks = Vec::new();
        for (index, check) in ports.verify.static_checks.iter().enumerate() {
            if check.is_valid() {
                checks.push(BriefStaticCheck {
                    argv: check.argv.clone(),
                    cwd: check.cwd.clone(),
                    timeout_seconds: check.timeout_seconds,
                });
            } else {
                let subject = format!("{origin}[{index}]");
                warn(
                    Warning::new(
                        PORT_STATIC_CHECK_INVALID,
                        format!("dropped an invalid {subject} from the writer preamble"),
                    )
                    .subject(subject.clone())
                    .source(source.clone())
                    .repair(format!(
                        "give {subject} a non-empty argv, a relative cwd and a positive timeout_seconds, or remove it"
                    )),
                );
            }
        }
        self.static_checks = checks;
        if !self.static_checks.is_empty() {
            self.static_checks_source = Some(source);
        }
    }

    /// The declared checks and their single source, for the launch receipt.
    /// An absent or empty tier yields `None`, so it adds neither preamble text
    /// nor a receipt field. The config merge replaces arrays wholesale, so one
    /// source covers the whole list (Spec: docs/specs/run-launch.md#layered-brief-preamble).
    pub fn static_checks_receipt(&self) -> Option<provenant_records::run::StaticChecksReceipt> {
        if self.static_checks.is_empty() {
            return None;
        }
        let default_origin = format!("{}#verify.static_checks", provenant_config::PORTS_CONFIG);
        let (layer, key) = match &self.static_checks_source {
            Some(source) => (
                source.layer,
                source.origin.clone().unwrap_or(default_origin),
            ),
            None => (Layer::Project, default_origin),
        };
        Some(provenant_records::run::StaticChecksReceipt {
            checks: self
                .static_checks
                .iter()
                .map(|check| provenant_records::run::StaticCheckReceipt {
                    argv: check.argv.clone(),
                    cwd: check.cwd.display().to_string(),
                    timeout_seconds: check.timeout_seconds,
                    extra: Default::default(),
                })
                .collect(),
            source: layer,
            key,
            extra: Default::default(),
        })
    }
}

/// Optional preamble overrides. Default uses the targeted verification paragraph.
#[derive(Clone, Debug, Default)]
pub struct PreambleExtras {
    pub route: Option<provenant_records::run::RouteRecord>,
    pub location: Option<LaneLocation>,
    pub verification: Option<crate::VerificationResolution>,
    pub build_cap: Option<BuildCap>,
    /// The layered standing brief preamble and the project's static checks.
    pub brief: Option<BriefPreamble>,
    /// Which role this lane plays, for the preamble's role filters.
    pub role: BriefRole,
    /// The effective `lanes.pr` policy and its source. Writers state it in
    /// their preamble so the lane knows which pull-request writes are allowed.
    pub pr_policy: Option<crate::ResolvedPrPolicy>,
}

/// Format an applied route without repeating a provider already on the model.
pub fn route_label(
    provider: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> Option<String> {
    let model = model?;
    let mut label = match provider {
        Some(provider) if !model.starts_with(&format!("{provider}/")) => {
            format!("{provider}/{model}")
        }
        _ => model.to_owned(),
    };
    if let Some(effort) = effort {
        label.push('@');
        label.push_str(effort);
    }
    Some(label)
}

fn route_preamble(route: &provenant_records::run::RouteRecord) -> String {
    let label = route_label(
        route.provider.as_deref(),
        route.model.as_deref(),
        route.effort.as_deref(),
    )
    .or_else(|| route.host_native.clone())
    .or_else(|| route.provider.clone());
    let account = route
        .account_label
        .as_ref()
        .map(|account| format!(", account {account}"))
        .unwrap_or_default();
    let source = route
        .source
        .as_ref()
        .map(|source| format!(" (source {source})"))
        .unwrap_or_default();
    let access = route
        .lane_access
        .as_ref()
        .map(|access| {
            format!(
                "Access: {} ({} {})\n",
                access.access.as_str(),
                access.key,
                access.source.as_str()
            )
        })
        .unwrap_or_default();
    let route = label
        .map(|label| format!("Route: {label}{account}{source}\n"))
        .unwrap_or_default();
    format!("{route}{access}")
}

fn slot_guidance(cap: Option<BuildCap>) -> String {
    match cap {
        Some(cap) => format!(
            "Build slots: `cargo` commands share the `build` admission class cap of {} (source: {}; `[admission.build].slots`). Busy or unavailable slots wait a bounded time, then warn and the build proceeds. 0 disables the cap. Cargo manages its own build locks.\n",
            cap.slots, cap.source
        ),
        None => BUILD_SLOT_GUIDANCE.to_string(),
    }
}

/// The reading rule every lane is told. Tool output stays in a lane's context and is
/// paid for again on every later turn, so whole-file and large-range reads dominate its cost.
pub const READING_GUIDANCE: &str = "Reading: locate before you read. Search with line numbers (`rg -n`), then read a bounded range of the lines you need; avoid whole-file `cat` or `sed` reads and unbounded `git diff` or `rg` output. Everything you print stays in your context and is paid for again on every later turn.\n";

/// The text placed before a lane's task: model policy, then for writers
/// (`ci_scope` is `Some`) the build-slot rule, the CI scope, the gh budget and
/// the pull-request policy, then the handoff file and the reading rule.
pub fn lane_preamble(
    model_guidance: &str,
    limits: &provenant_records::LaneLimits,
    ci_scope: Option<crate::ResolvedCiScope>,
    handoff: Option<&Path>,
) -> String {
    lane_preamble_with(
        model_guidance,
        limits,
        ci_scope,
        handoff,
        &PreambleExtras::default(),
    )
}

pub fn lane_preamble_with(
    model_guidance: &str,
    limits: &provenant_records::LaneLimits,
    ci_scope: Option<crate::ResolvedCiScope>,
    handoff: Option<&Path>,
    extras: &PreambleExtras,
) -> String {
    let is_writer = ci_scope.is_some();
    let brief = extras
        .brief
        .as_ref()
        .map(|brief| {
            let mut text = brief.render(extras.role);
            if is_writer && extras.role == BriefRole::Writer {
                text.push_str(&brief.static_checks_paragraph());
            }
            text
        })
        .unwrap_or_default();
    let writer = ci_scope
        .map(|scope| {
            let pr = extras.pr_policy.unwrap_or_default().preamble();
            let budget = limits
                .gh_invocations
                .as_ref()
                .map(|limit| {
                    format!(
                        "GitHub calls: gh commands through your gh wrapper are counted, with a warning after {} (source: {}; lanes.gh_budget). They spend the user's API quota: batch reads, do not poll, and prefer one gh api graphql query over many calls. The warning never stops a command.\n",
                        limit.value,
                        limit.source.as_str()
                    )
                })
                .unwrap_or_default();
            let verification = extras
                .verification
                .as_ref()
                .map(crate::VerificationResolution::paragraph)
                .unwrap_or_else(|| crate::verification::CHECK_PARAGRAPH.to_string());
            let location = extras
                .location
                .as_ref()
                .map(LaneLocation::preamble)
                .unwrap_or_default();
            format!(
                "{location}{verification}In a shared worktree, commit only your own paths with `git commit -- <paths>`; coordinate overlapping edits with the owning lane.\n{}{}{budget}{pr}",
                slot_guidance(extras.build_cap),
                scope.preamble()
            )
        })
        .unwrap_or_default();
    format!(
        "{model_guidance}\n{brief}{}{writer}{}{}{READING_GUIDANCE}Shared notes: `provenant notes add [--tag T] [--scope GRAPH/NODE] -- TEXT`; `provenant notes read` reads new observations; use `--replay` to replay. Notes are unverified claims by default.\n",
        limits.preamble(),
        extras
            .route
            .as_ref()
            .map(route_preamble)
            .unwrap_or_default(),
        handoff.map(handoff_progress_guidance).unwrap_or_default()
    )
}

pub fn handoff_progress_guidance(path: &Path) -> String {
    format!(
        "Progress handoff JSON: {}. Update this file throughout the task by writing a temporary file beside it and renaming it into place (hard links are denied), and repeat its current value in the final handoff fence. A failed update never blocks the work: carry on and report the value in the fence. It is lane state, never a repository file or a commit.\n",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, exit, input, publication, seal_observation};
    use provenant_records::AbsolutePath;
    use std::os::unix::fs::PermissionsExt;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    /// Invariant: the preamble teaches the statuses from the text the routing
    /// policy's guidance also uses, and no sentence contradicts it.
    #[test]
    fn guidance_teaches_the_statuses_from_the_shared_text() {
        let text = guidance();
        assert!(text.contains(provenant_records::handoff::STATUS_GUIDANCE));
        assert!(text.contains("blocked_on"));
        assert!(!text.contains("real boundary"));
        assert_eq!(text.matches("Use blocked only").count(), 1, "{text}");
    }

    #[test]
    fn guidance_treats_write_scope_as_advisory_outside_strict_policy() {
        let text = guidance();
        assert!(text.contains("not a fence"));
        assert!(text.contains("list each such path in decisions"));
        assert!(text.contains("strict WRITE_SCOPE lane policy"));
        assert!(!text.contains("keep it inside the approved write scope"));
    }

    #[test]
    fn writer_preamble_states_advisory_build_policy_and_read_only_does_not() {
        let handoff = Path::new("/state/handoff.json");
        let writer = lane_preamble(
            "Model policy.",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            Some(handoff),
        );
        assert!(writer.contains(crate::verification::CHECK_PARAGRAPH));
        assert!(writer.contains("source: default"));
        // #3790: lanes never compile, so the writer preamble states the
        // formatting duty and no Cargo target or build directory.
        assert!(writer.contains("format only"));
        assert!(!writer.contains("Cargo target"));
        assert!(!writer.contains("CARGO_TARGET_DIR"));
        assert!(writer.contains("Build slots:"));
        assert!(writer.contains("CI scope:"));
        assert!(writer.contains("git commit -- <paths>"));
        assert!(writer.contains(&format!("Progress handoff JSON: {}", handoff.display())));
        let reader = lane_preamble("Model policy.", &Default::default(), None, Some(handoff));
        assert!(!reader.contains("Build slots"));
        assert!(!reader.contains("CI scope:"));
        assert!(!reader.contains("format only"));
        assert!(!reader.contains("Cargo target"));
        assert!(reader.contains("Progress handoff JSON:"));
        assert!(
            !lane_preamble("Model policy.", &Default::default(), None, None)
                .contains("handoff.json")
        );
    }

    /// Harnesses such as opencode name the primary checkout as the workspace
    /// root of a linked worktree. The brief must carry absolute paths so it
    /// outranks that banner (#3611), for every adapter alike.
    #[test]
    fn writer_preamble_names_the_bound_worktree_branch_base_and_primary_checkout() {
        let location = LaneLocation {
            worktree: "/repo/.worktrees/fix-1".into(),
            branch: "refs/heads/fix/1".into(),
            base: "a".repeat(40),
            primary_checkout: Some("/repo".into()),
        };
        let extras = PreambleExtras {
            location: Some(location.clone()),
            ..Default::default()
        };
        let writer = lane_preamble_with(
            "Model policy.",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            None,
            &extras,
        );
        assert!(writer.contains("Lane location:"));
        assert!(writer.contains("/repo/.worktrees/fix-1"));
        assert!(writer.contains("branch fix/1"));
        assert!(writer.contains(&"a".repeat(40)));
        assert!(writer.contains("The primary checkout /repo is not your working copy"));
        let reader = lane_preamble_with("Model policy.", &Default::default(), None, None, &extras);
        assert!(!reader.contains("Lane location:"));
        let bare = LaneLocation {
            primary_checkout: None,
            ..location
        };
        assert!(!bare.preamble().contains("primary checkout"));
        assert!(bare.preamble().contains("/repo/.worktrees/fix-1"));
    }

    #[test]
    fn preamble_reports_applied_route_verbatim_and_omits_unknown_parts() {
        let mut route = provenant_records::run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("opus[1m]".into()),
            effort: Some("high".into()),
            account_label: Some("work [east]".into()),
            source: Some("pool".into()),
            ..Default::default()
        };
        let extras = PreambleExtras {
            route: Some(route.clone()),
            ..Default::default()
        };
        let text = lane_preamble_with("Policy", &Default::default(), None, None, &extras);
        assert!(text.contains("Route: claude/opus[1m]@high, account work [east] (source pool)\n"));
        route.effort = None;
        route.account_label = None;
        route.source = None;
        let extras = PreambleExtras {
            route: Some(route),
            ..Default::default()
        };
        let text = lane_preamble_with("Policy", &Default::default(), None, None, &extras);
        assert!(text.contains("Route: claude/opus[1m]\n"));
    }

    #[test]
    fn configured_build_slots_appear_in_writer_preamble() {
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version=1\n[admission.build]\nslots=7\n",
        )
        .unwrap();
        let class = provenant_config::ResolvedAdmissionClass::resolve("build", state.path(), None);
        let extras = PreambleExtras {
            build_cap: Some(BuildCap::from(&class)),
            ..Default::default()
        };
        let preamble = lane_preamble_with(
            "Model policy.",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            None,
            &extras,
        );
        assert!(preamble.contains("build` admission class cap of 7"));
        // A setting reports its layer with the shared person-readable name:
        // Host is "host configuration", not the old free-form "host".
        // Spec: docs/specs/state.md#every-setting-reports-its-layer
        assert!(
            preamble.contains("source: host configuration; `[admission.build].slots`"),
            "{preamble}"
        );
    }

    /// Spec: docs/specs/run-launch.md#layered-brief-preamble
    #[test]
    fn layered_brief_preamble_concatenates_with_source_labels_and_role_filters() {
        fn layer(layer: Layer, origin: &str, roles: Vec<BriefRole>, text: &str) -> BriefLayer {
            BriefLayer {
                source: Source {
                    layer,
                    origin: Some(origin.to_owned()),
                },
                roles,
                text: text.to_owned(),
            }
        }
        let preamble = BriefPreamble {
            layers: vec![
                layer(Layer::Host, "host.md", vec![], "Standing host rule."),
                layer(
                    Layer::Project,
                    ".provenant/project.md",
                    vec![BriefRole::Writer],
                    "Writer-only project rule.",
                ),
                layer(
                    Layer::Launch,
                    "--preamble-file",
                    vec![],
                    "One-off launch rule.",
                ),
            ],
            static_checks: vec![BriefStaticCheck {
                argv: vec!["scripts/check-extra-fields".into()],
                cwd: PathBuf::from("."),
                timeout_seconds: 30,
            }],
            static_checks_source: Some(Source {
                layer: Layer::Project,
                origin: Some(".provenant/ports.json#verify.static_checks".into()),
            }),
        };
        let extras = PreambleExtras {
            brief: Some(preamble.clone()),
            role: BriefRole::Writer,
            ..Default::default()
        };
        let writer = lane_preamble_with(
            "Policy",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            None,
            &extras,
        );
        let host = writer.find("Brief preamble from host configuration (host.md)");
        let project = writer.find("Brief preamble from project (.provenant/project.md)");
        let launch = writer.find("Brief preamble from launch (--preamble-file)");
        assert!(
            host.is_some() && project.is_some() && launch.is_some(),
            "{writer}"
        );
        assert!(host < project && project < launch, "{writer}");
        assert!(writer.contains("Standing host rule."), "{writer}");
        assert!(writer.contains("Writer-only project rule."), "{writer}");
        assert!(writer.contains("One-off launch rule."), "{writer}");
        assert!(
            writer.contains("source: project; .provenant/ports.json#verify.static_checks"),
            "{writer}"
        );
        assert!(writer.contains("scripts/check-extra-fields"), "{writer}");
        assert!(writer.contains("cap 30s"), "{writer}");
        // A review lane sees only the layers whose roles match it, and no
        // static-check paragraph even when the project declared checks.
        let review_extras = PreambleExtras {
            brief: Some(preamble.clone()),
            role: BriefRole::Review,
            ..Default::default()
        };
        let review = lane_preamble_with(
            "Policy",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            None,
            &review_extras,
        );
        assert!(review.contains("Standing host rule."), "{review}");
        assert!(!review.contains("Writer-only project rule."), "{review}");
        assert!(review.contains("One-off launch rule."), "{review}");
        assert!(!review.contains("Static checks:"), "{review}");
        let reader_extras = PreambleExtras {
            brief: Some(preamble),
            role: BriefRole::ReadOnly,
            ..Default::default()
        };
        let reader = lane_preamble_with("Policy", &Default::default(), None, None, &reader_extras);
        assert!(!reader.contains("Writer-only project rule."), "{reader}");
        assert!(!reader.contains("Static checks:"), "{reader}");
    }

    /// A declared check travels from a written `.provenant/ports.json`, through
    /// the port table, into the rendered writer prompt with its project source.
    /// An invalid entry is dropped with a typed warning instead of refusing the
    /// launch. Spec: docs/specs/run-launch.md#layered-brief-preamble
    #[test]
    fn ports_json_static_checks_reach_the_writer_preamble_with_source() {
        let state = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".provenant")).unwrap();
        std::fs::write(
            project.path().join(".provenant/ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "verify": {
                    "static_checks": [
                        {"argv": ["scripts/check-extra-fields"], "cwd": ".", "timeout_seconds": 30},
                        {"argv": [], "cwd": ".", "timeout_seconds": 30},
                    ],
                },
            }))
            .unwrap(),
        )
        .unwrap();

        // An invalid static check is advice: it does not refuse the port table.
        let ports = provenant_config::PortTable::read_for(Some(project.path()), state.path())
            .expect("an invalid static check is not a table refusal");
        assert_eq!(ports.verify.static_checks.len(), 2);
        assert_eq!(
            ports.provenance().get("verify.static_checks"),
            Some(&Layer::Project)
        );

        let mut brief = BriefPreamble::default();
        let ((), warnings) = provenant_records::warning::capture(|| {
            brief.set_static_checks_from_ports(&ports, BriefRole::Writer);
        });
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = &warnings[0];
        assert_eq!(warning.code.as_ref(), PORT_STATIC_CHECK_INVALID.as_str());
        let warning_source = warning.source.as_ref().expect("warning has a source");
        assert_eq!(warning_source.layer, Layer::Project);
        assert_eq!(
            warning_source.origin.as_deref(),
            Some(".provenant/ports.json#verify.static_checks")
        );
        let repair = warning.repair.as_deref().expect("warning has a repair");
        assert!(repair.contains("verify.static_checks"), "{repair}");

        assert_eq!(brief.static_checks.len(), 1, "{brief:?}");
        assert_eq!(brief.static_checks[0].argv, ["scripts/check-extra-fields"]);
        let brief_source = brief.static_checks_source.as_ref().expect("source present");
        assert_eq!(brief_source.layer, Layer::Project);
        assert_eq!(
            brief_source.origin.as_deref(),
            Some(".provenant/ports.json#verify.static_checks")
        );

        let writer = lane_preamble_with(
            "Policy",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            None,
            &PreambleExtras {
                brief: Some(brief),
                role: BriefRole::Writer,
                ..Default::default()
            },
        );
        assert!(writer.contains("scripts/check-extra-fields"), "{writer}");
        assert!(writer.contains("cap 30s"), "{writer}");
        assert!(
            writer.contains("source: project; .provenant/ports.json#verify.static_checks"),
            "{writer}"
        );
    }

    /// An absent or empty `verify.static_checks` declaration adds no preamble
    /// text and no receipt: no tier is invented for stacks that declare none.
    /// Spec: docs/specs/run-launch.md#layered-brief-preamble
    #[test]
    fn no_declared_static_checks_add_no_preamble_text_or_receipt() {
        let state = tempfile::tempdir().unwrap();
        for declaration in [None, Some("{\"verify\":{\"static_checks\":[]}}")] {
            let project = tempfile::tempdir().unwrap();
            if let Some(text) = declaration {
                std::fs::create_dir_all(project.path().join(".provenant")).unwrap();
                std::fs::write(project.path().join(".provenant/ports.json"), text).unwrap();
            }
            let ports = provenant_config::PortTable::read_for(Some(project.path()), state.path())
                .expect("a project with no static checks is valid");
            assert!(ports.verify.static_checks.is_empty());

            let mut brief = BriefPreamble::default();
            let ((), warnings) = provenant_records::warning::capture(|| {
                brief.set_static_checks_from_ports(&ports, BriefRole::Writer);
            });
            assert!(warnings.is_empty(), "{warnings:?}");
            assert!(brief.static_checks.is_empty(), "{brief:?}");
            assert!(brief.static_checks_source.is_none(), "{brief:?}");
            assert!(brief.static_checks_receipt().is_none(), "{brief:?}");

            let writer = lane_preamble_with(
                "Policy",
                &Default::default(),
                Some(crate::ResolvedCiScope::resolve(&Default::default())),
                None,
                &PreambleExtras {
                    brief: Some(brief),
                    role: BriefRole::Writer,
                    ..Default::default()
                },
            );
            assert!(!writer.contains("Static checks:"), "{writer}");
        }
    }

    /// The declared tier reaches the launch receipt as one sourced list, beside
    /// `ci_scope` and `pr_policy`. Invalid entries are dropped from both the
    /// preamble and the receipt; one source covers the list because config
    /// merging replaces arrays wholesale.
    /// Spec: docs/specs/run-launch.md#layered-brief-preamble
    #[test]
    fn declared_static_checks_reach_the_launch_receipt_with_source() {
        let state = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".provenant")).unwrap();
        std::fs::write(
            project.path().join(".provenant/ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "verify": {
                    "static_checks": [
                        {"argv": ["scripts/check-extra-fields"], "cwd": ".", "timeout_seconds": 30},
                        {"argv": [], "cwd": ".", "timeout_seconds": 30},
                    ],
                },
            }))
            .unwrap(),
        )
        .unwrap();
        let ports = provenant_config::PortTable::read_for(Some(project.path()), state.path())
            .expect("an invalid static check is not a table refusal");

        let mut brief = BriefPreamble::default();
        let ((), warnings) = provenant_records::warning::capture(|| {
            brief.set_static_checks_from_ports(&ports, BriefRole::Writer);
        });
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let receipt = brief
            .static_checks_receipt()
            .expect("a declared tier has a receipt");
        assert_eq!(receipt.checks.len(), 1, "{receipt:?}");
        assert_eq!(receipt.checks[0].argv, ["scripts/check-extra-fields"]);
        assert_eq!(receipt.checks[0].cwd, ".");
        assert_eq!(receipt.checks[0].timeout_seconds, 30);
        assert_eq!(receipt.source, Layer::Project);
        assert_eq!(receipt.key, ".provenant/ports.json#verify.static_checks");
    }

    /// A review or read-only lane never renders the `Static checks:` paragraph,
    /// so it neither reads nor validates the project's list: an invalid entry
    /// must not emit the writer-preamble warning on those launches.
    /// Spec: docs/specs/run-launch.md#layered-brief-preamble
    #[test]
    fn non_writer_launches_do_not_warn_about_invalid_static_checks() {
        let state = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".provenant")).unwrap();
        std::fs::write(
            project.path().join(".provenant/ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "verify": {
                    "static_checks": [
                        {"argv": ["scripts/check-extra-fields"], "cwd": ".", "timeout_seconds": 30},
                        {"argv": [], "cwd": ".", "timeout_seconds": 30},
                    ],
                },
            }))
            .unwrap(),
        )
        .unwrap();
        let ports = provenant_config::PortTable::read_for(Some(project.path()), state.path())
            .expect("an invalid static check is not a table refusal");
        assert_eq!(ports.verify.static_checks.len(), 2);

        for role in [BriefRole::Review, BriefRole::ReadOnly] {
            let mut brief = BriefPreamble::default();
            let ((), warnings) = provenant_records::warning::capture(|| {
                brief.set_static_checks_from_ports(&ports, role);
            });
            assert!(
                warnings
                    .iter()
                    .all(|warning| warning.code.as_ref() != PORT_STATIC_CHECK_INVALID.as_str()),
                "{role:?} warned about a writer-only static check: {warnings:?}"
            );
            assert!(
                brief.static_checks.is_empty(),
                "{role:?} kept writer-only static checks"
            );
        }
    }

    /// Spec: docs/specs/run-launch.md#layered-brief-preamble
    #[test]
    fn a_missing_brief_preamble_file_warns_and_the_other_layers_still_launch() {
        let temp = tempfile::tempdir().unwrap();
        let host_file = temp.path().join("host.md");
        std::fs::write(&host_file, "Host rule.\n").unwrap();
        let host_config = temp.path().join("config.toml");
        std::fs::write(
            &host_config,
            format!(
                "schema_version=1\n[knobs]\nlanes.brief_preamble = {:?}\n",
                host_file.display().to_string()
            ),
        )
        .unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        // A project knob path is relative to the project root, so the warning
        // must name the path that was actually attempted.
        let missing = project.join("absent.md");
        std::fs::write(
            project.join(".provenant").join("config.toml"),
            "schema_version=1\n[knobs.lanes]\nbrief_preamble = \"absent.md\"\n",
        )
        .unwrap();
        let launch = temp.path().join("launch.md");
        std::fs::write(&launch, "Launch rule.\n").unwrap();

        let (brief, warnings) = provenant_records::warning::capture(|| {
            BriefPreamble::read(Some(&host_config), Some(&project), Some(&launch))
        });
        assert_eq!(brief.layers.len(), 2, "{brief:?}");
        assert_eq!(brief.layers[0].source.layer, Layer::Host);
        assert_eq!(brief.layers[1].source.layer, Layer::Launch);
        assert!(
            warnings.iter().any(|warning| {
                warning.code.as_ref() == CONFIG_UNAVAILABLE.as_str()
                    && warning.message.contains(missing.to_str().unwrap())
            }),
            "{warnings:?}"
        );

        // The preamble still contains the readable layers; the launch proceeds.
        let extras = PreambleExtras {
            brief: Some(brief),
            ..Default::default()
        };
        let text = lane_preamble_with("Policy", &Default::default(), None, None, &extras);
        assert!(text.contains("Host rule."), "{text}");
        assert!(text.contains("Launch rule."), "{text}");
        assert!(!text.contains("absent.md"), "{text}");
    }

    fn snapshot(status: &str) -> Vec<u8> {
        serde_json::json!({"schema_version":2,"status":status,"note":"Parser committed; renderer remains.","remaining":["Finish renderer"],"decisions":["Keep the parser API"],"verify":["Parser tests passed"]}).to_string().into_bytes()
    }
    fn decoded() -> Decoded {
        Decoded {
            terminal_cause: None,
            outcome: Outcome::QuotaLimited,
            result: String::new(),
            complete: false,
            observed_route: None,
            usage: Default::default(),
            session: None,
            reason: None,
            partial_signals: vec![],
            turns: Vec::new(),
        }
    }
    fn grant(record: &mut AttemptRecord, candidate: &Path) -> TestResult {
        record.write_grants = vec![WriteGrantRecord {
            extra: Default::default(),
            source: WriteSource::PrivateState,
            path: AbsolutePath::new(candidate)?,
            scope: PathScope::Subtree,
        }];
        Ok(())
    }

    #[test]
    fn scope_report_falls_back_to_its_artifact_without_replacing_snapshot() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let raw = snapshot("done");
        let original = Handoff::from_json(&raw).map_err(std::io::Error::other)?;
        seal(
            &attempt,
            AttemptFile::Handoff.name(),
            &serde_json::to_vec(&Ok::<_, HandoffError>(Some(original.clone())))?,
            HANDOFF_RECORD_BYTES as u64,
        )?;
        let mut paths = Vec::new();
        for index in 0..HANDOFF_BYTES / 32 {
            let relative = provenant_core::RelativePath::parse(format!(
                "outside/{index}/{}",
                "x".repeat(128)
            ))?;
            paths.push(provenant_records::PathWire::from_relative(&relative)?);
        }
        let path_count = paths.len();
        let report = provenant_records::handoff::WriteScopeReport::Full {
            schema_version: 1,
            code: "WRITE_SCOPE".into(),
            base: "1".repeat(40),
            head: "2".repeat(40),
            strength: provenant_records::policy::Strength::Warn,
            source: provenant_records::policy::Source::Project,
            out_of_scope_paths: paths,
        };
        seal(
            &attempt,
            "policy-write-scope.json",
            &crate::db::bytes(&report)?,
            provenant_records::run::TERMINAL_RECORD_LIMIT,
        )?;

        let attached = attach_scope_report(&attempt, &original)?;
        assert!(matches!(
            attached.write_scope,
            Some(provenant_records::handoff::WriteScopeReport::Compact {
                out_of_scope_count,
                paths_omitted_from_handoff: true,
                ..
            }) if out_of_scope_count == path_count
        ));
        assert_eq!(read_snapshot(&attempt)?, Some(Ok(Some(original))));
        assert_eq!(store.handoff(&record)??, Some(attached));
        Ok(())
    }

    #[test]
    fn read_only_plan_progress_is_collected_at_settlement() -> TestResult {
        use provenant_confinement::{
            ConfinedPlan, PlanRequest,
            permissions::{AccessMode, NetworkAccess, PathGrant, PlanPermissions, ProviderProfile},
            plan::{CodexPolicy, ExtensionPolicy},
        };
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempt_id, &call)?;
        assert!(input.request.task.access.write_roots.is_empty());
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
        let base = store
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{}", record.attempt_id));
        let subtree = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: provenant_core::WorkScope::Project(store.context.project_id),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                credential_roots: provenant_core::CredentialRoots::for_home(
                    &temp.path().join("home"),
                ),
                policy: None,
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: subtree(base.join("tmp/candidate-0")),
                private_state: Some(subtree(base.join("provider-state/candidate-0"))),
                read_roots: vec![],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
        })?;
        let profile = ProviderProfile {
            spec: Default::default(),
            observations: Default::default(),
        };
        record.write_grants = provenant_confinement::write_grants(plan.permissions(), &profile)
            .into_iter()
            .map(|grant| {
                Ok(WriteGrantRecord {
                    extra: Default::default(),
                    source: grant.source,
                    path: AbsolutePath::new(&grant.path)?,
                    scope: grant.scope,
                })
            })
            .collect::<std::result::Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let bytes = snapshot("stopped_early");
        seal(&candidate, "handoff.json", &bytes, HANDOFF_BYTES as u64)?;
        let expected = Handoff::from_json(&bytes)?;
        assert_eq!(
            store.progress_handoff(&attempt, &record)??,
            Some(expected.clone())
        );
        assert_eq!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .handoff?,
            Some(expected.clone())
        );
        // The provider's progress handoff is a settlement input too, so the
        // same class rule repairs a broad mode instead of stalling recovery.
        std::fs::set_permissions(
            candidate.current_path()?.join(AttemptFile::Handoff.name()),
            std::fs::Permissions::from_mode(0o644),
        )?;
        crate::artifacts::PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow_mut().clear());
        assert_eq!(
            store.progress_handoff(&attempt, &record)??,
            Some(expected.clone())
        );
        let warnings =
            crate::artifacts::PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow().clone());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(
                &candidate
                    .anchored_path()
                    .join(AttemptFile::Handoff.name())
                    .display()
                    .to_string()
            ),
            "warning must name the repaired progress handoff: {warnings:?}"
        );
        assert_eq!(
            candidate
                .metadata_at(OsStr::new(AttemptFile::Handoff.name()), false)?
                .mode
                & 0o077,
            0
        );
        assert_eq!(store.handoff(&record)??, Some(expected));
        Ok(())
    }

    #[test]
    fn settlement_repairs_a_broad_handoff_snapshot_and_warns() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let bytes = snapshot("done");
        let expected = Handoff::from_json(&bytes)?;
        seal(
            &attempt,
            AttemptFile::Handoff.name(),
            &serde_json::to_vec(&Ok::<_, HandoffError>(Some(expected.clone())))?,
            HANDOFF_RECORD_BYTES as u64,
        )?;
        let path = attempt.current_path()?.join(AttemptFile::Handoff.name());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        crate::artifacts::PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow_mut().clear());
        assert_eq!(store.handoff(&record)??, Some(expected));
        let warnings =
            crate::artifacts::PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow().clone());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(AttemptFile::Handoff.name()),
            "warning must name the repaired handoff: {warnings:?}"
        );
        assert!(
            warnings[0].contains("0644"),
            "warning must name the old mode: {warnings:?}"
        );
        let after = attempt.metadata_at(OsStr::new(AttemptFile::Handoff.name()), false)?;
        assert_eq!(after.mode & 0o077, 0);
        assert_eq!(PathCheck::file(after), None);
        Ok(())
    }

    /// #3844: undecodable bytes in the slot are a malformed settlement
    /// snapshot, not a retriable read error. The read accessor keeps the raw
    /// bytes and classifies them as malformed; capture moves them aside once
    /// and seals from output rather than looping on a decode that cannot change.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn an_undecodable_owned_snapshot_settles_malformed_and_keeps_the_raw_file() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;

        let raw = b"{ not a settlement handoff envelope";
        seal_snapshot(&attempt, raw)?;

        let observed = store.handoff(&record)?;
        let Err(malformed) = observed else {
            return Err("undecodable owned bytes must settle malformed".into());
        };
        assert!(
            malformed
                .to_string()
                .contains("handoff snapshot unreadable"),
            "{malformed:?}"
        );
        assert_eq!(
            provenant_records::run::SettlementReason::derive(
                Outcome::Succeeded,
                None,
                &[],
                None,
                Err(&malformed),
            ),
            provenant_records::run::SettlementReason::HandoffMalformed
        );
        assert_eq!(
            read_settled(&attempt, AttemptFile::Handoff, HANDOFF_RECORD_BYTES as u64)?,
            Some(raw.to_vec()),
            "the undecodable bytes stay as evidence"
        );

        // Capture treats undecodable bytes as foreign evidence: one move aside
        // clears the slot and seals from output. A repeat sees the settled
        // snapshot, so capture never loops on a decode that cannot change.
        let answer = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"from output\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
        let mut settled = decoded();
        settled.result = answer.to_owned();
        settled.complete = true;
        settled.outcome = Outcome::Succeeded;
        let first = store
            .capture_handoff(&attempt, &record, &settled)?
            .handoff?;
        assert_eq!(first.as_ref().map(|h| h.note.as_str()), Some("from output"));
        let record = store.record(record.attempt_id, &call)?;
        let second = store
            .capture_handoff(&attempt, &record, &settled)?
            .handoff?;
        assert_eq!(second, first, "a settled snapshot is stable on retry");
        Ok(())
    }

    /// #3844: an envelope sealed by the previous build has no tag and must
    /// still read as settlement's snapshot, with no migration.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn a_snapshot_sealed_without_a_tag_still_reads_as_sealed() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let expected = Handoff::from_json(&snapshot("done"))?;
        seal(
            &attempt,
            AttemptFile::Handoff.name(),
            &serde_json::to_vec(&Ok::<_, HandoffError>(Some(expected.clone())))?,
            HANDOFF_RECORD_BYTES as u64,
        )?;
        assert!(
            !attempt
                .current_path()?
                .join("handoff.settlement-seal")
                .try_exists()?,
            "no separate tag is published"
        );
        assert_eq!(store.handoff(&record)??, Some(expected));
        Ok(())
    }

    /// #3844: a settlement retry against its own snapshot must keep it, not
    /// relabel the host's record as provider-written. This is the crash window
    /// the old two-file tag publish left behind.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn a_second_capture_keeps_an_existing_settlement_snapshot() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let first = Handoff::from_json(&snapshot("done"))?;
        let bytes = serde_json::to_vec(&Ok::<_, HandoffError>(Some(first.clone())))?;
        seal(
            &attempt,
            AttemptFile::Handoff.name(),
            &bytes,
            HANDOFF_RECORD_BYTES as u64,
        )?;
        let captured = store
            .capture_handoff(&attempt, &record, &decoded())?
            .handoff?;
        assert_eq!(captured, Some(first), "the sealed snapshot wins on retry");
        assert_eq!(
            read_settled(&attempt, AttemptFile::Handoff, HANDOFF_RECORD_BYTES as u64)?,
            Some(bytes),
            "the host snapshot is not rewritten"
        );
        assert!(
            !attempt
                .current_path()?
                .join(AttemptFile::HandoffProviderWritten.name())
                .try_exists()?,
            "the host snapshot was not moved aside"
        );
        Ok(())
    }

    /// #3844: only the `HandoffResult` envelope is settlement's snapshot. A
    /// pre-#3078 bare `Handoff` has no compatibility path: it is provider
    /// evidence, moved aside at capture and never read as a sealed handoff.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn a_bare_handoff_slot_is_moved_aside_as_provider_written_evidence() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let expected = Handoff::from_json(&snapshot("done"))?;
        let bare = serde_json::to_vec(&expected)?;
        // The pre-#3078 slot held the bare Handoff, not an envelope.
        seal(
            &attempt,
            AttemptFile::Handoff.name(),
            &bare,
            HANDOFF_RECORD_BYTES as u64,
        )?;

        let captured = store.capture_handoff(&attempt, &record, &decoded())?;
        assert_eq!(captured.handoff?, None, "a bare snapshot is never trusted");
        assert_eq!(
            store.handoff(&record)??,
            None,
            "the slot was cleared for settlement"
        );
        assert_eq!(
            read_settled(
                &attempt,
                AttemptFile::HandoffProviderWritten,
                HANDOFF_RECORD_BYTES as u64
            )?,
            Some(bare),
            "the bare snapshot is kept as provider-written evidence"
        );
        Ok(())
    }

    /// #3844: a lane-created directory at the evidence destination must not
    /// strand capture. The foreign slot entry moves to the declared unsafe
    /// evidence name and the complete answer still settles.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn an_occupied_evidence_destination_does_not_strand_capture() -> TestResult {
        use crate::dispatch_tests::{Fake, seal_exit};
        use crate::tests::route;

        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let id = input.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let attempt = store.attempt_dir(id, false)?;
        let root = attempt.current_path()?;
        // A lane scratch directory occupies the normal evidence destination.
        std::fs::create_dir(root.join(AttemptFile::HandoffProviderWritten.name()))?;
        // A regular, non-envelope slot entry.
        let foreign = b"provider-written handoff";
        std::fs::write(root.join(AttemptFile::Handoff.name()), foreign)?;

        let answer = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"sealed from output\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
        let mut observed = exit();
        observed.scope = RunStore::scope(&store.record(id, &call)?);
        seal_exit(&store, &observed)?;
        let fake = Fake::with_observed(Some(route()), Some(Outcome::Succeeded))
            .with_answer(Some(answer.to_owned()), Some(true));
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        assert!(
            matches!(
                &finished.state,
                AttemptState::Terminal {
                    outcome: Outcome::Succeeded,
                    ..
                }
            ),
            "a complete answer must settle: {:?}",
            finished.state
        );
        let handoff = store.handoff(&finished)??.ok_or("sealed snapshot")?;
        assert_eq!(handoff.note, "sealed from output");
        assert!(
            root.join(AttemptFile::HandoffProviderWritten.name())
                .is_dir(),
            "the lane directory at the preferred name is left intact"
        );
        assert_eq!(
            std::fs::read(root.join(AttemptFile::HandoffProviderWrittenUnsafe.name()))?,
            foreign.to_vec(),
            "foreign bytes were kept under the declared unsafe name"
        );
        Ok(())
    }

    /// An unsafe slot entry cannot change once the provider is gone, so capture
    /// must move it aside by name and settle from output rather than retry a
    /// typed read failure forever (#3844). The read accessor still refuses the
    /// entry before capture; only the settlement act clears it.
    fn assert_unsafe_snapshot_slot_settles(
        plant: impl FnOnce(&std::path::Path) -> std::result::Result<(), Box<dyn std::error::Error>>,
        evidence: &str,
    ) -> TestResult {
        use crate::dispatch_tests::{Fake, seal_exit};
        use crate::tests::route;

        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let id = input.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let attempt = store.attempt_dir(id, false)?;
        let slot = attempt.current_path()?.join(AttemptFile::Handoff.name());
        plant(&slot)?;
        store
            .handoff(&store.record(id, &call)?)
            .expect_err("an unsafe slot must refuse a read before capture");

        let answer = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"sealed from output\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
        let mut observed = exit();
        observed.scope = RunStore::scope(&store.record(id, &call)?);
        seal_exit(&store, &observed)?;
        let fake = Fake::with_observed(Some(route()), Some(Outcome::Succeeded))
            .with_answer(Some(answer.to_owned()), Some(true));
        let finished = store.finish(id, &attempt, &observed, &fake, &call)?;
        assert!(
            matches!(
                &finished.state,
                AttemptState::Terminal {
                    outcome: Outcome::Succeeded,
                    ..
                }
            ),
            "a complete answer must still settle: {:?}",
            finished.state
        );
        let handoff = store.handoff(&finished)??.ok_or("sealed snapshot")?;
        assert_eq!(
            handoff.note, "sealed from output",
            "the unsafe entry left the slot and the snapshot was sealed from output"
        );
        assert!(
            std::fs::symlink_metadata(attempt.current_path()?.join(evidence)).is_ok(),
            "the unsafe entry was kept as {evidence}"
        );
        Ok(())
    }

    /// #3844: a hard link in the slot is an unsafe entry, not a retriable read.
    /// Capture keeps it as provider evidence and seals from output.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn a_hard_linked_snapshot_slot_is_moved_aside_and_settles() -> TestResult {
        assert_unsafe_snapshot_slot_settles(
            |slot| {
                let foreign = slot.with_file_name("foreign-handoff.json");
                std::fs::write(&foreign, snapshot("done"))?;
                std::fs::hard_link(&foreign, slot)?;
                Ok(())
            },
            AttemptFile::HandoffProviderWritten.name(),
        )
    }

    /// #3844: an oversize slot entry cannot be read under the bound and the
    /// provider is gone, so capture keeps it as evidence and still settles.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn an_oversize_snapshot_slot_is_moved_aside_and_settles() -> TestResult {
        assert_unsafe_snapshot_slot_settles(
            |slot| {
                std::fs::write(slot, vec![b'x'; HANDOFF_RECORD_BYTES + 1])?;
                Ok(())
            },
            AttemptFile::HandoffProviderWritten.name(),
        )
    }

    /// #3844: a symlink slot must never be followed; capture renames the link
    /// itself aside under a non-regular evidence name and settles.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn a_symlinked_snapshot_slot_is_moved_aside_and_settles() -> TestResult {
        assert_unsafe_snapshot_slot_settles(
            |slot| {
                std::os::unix::fs::symlink("not-read-target", slot)?;
                Ok(())
            },
            AttemptFile::HandoffProviderWrittenUnsafe.name(),
        )
    }

    /// Provider boundary for exercising holder recovery when this host cannot
    /// launch nested confinement. No launch or Git settlement is simulated.
    struct RecoveringAccounts;
    impl Providers for RecoveringAccounts {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<crate::PrepareRefusal>> {
            Err(crate::fixture_refusal(
                "holder-only fixture does not launch",
            ))
        }
        fn decode(
            &self,
            route: &RouteRecord,
            _: &[u8],
            _: &provenant_supervisor::custody::ExitObservation,
            _: &Path,
        ) -> std::result::Result<Decoded, String> {
            let mut result = decoded();
            match route.account_key.as_deref() {
                // The provider died before declaring anything: an absent final
                // declaration, so settlement falls back to granted progress. A
                // truncated fence is malformed evidence instead.
                Some("fixture-one") => result.result = "Working on the renderer".into(),
                Some("fixture-two") => {
                    result.outcome = Outcome::Succeeded;
                    result.complete = true;
                    result.result = "Recovered progress and completed the renderer\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"finished\"}\n```".into();
                    result.observed_route = Some(route.clone());
                }
                _ => return Err("unexpected fixture account".into()),
            }
            Ok(result)
        }
    }

    // Spec: docs/specs/run-settlement.md#retention-and-disclosure
    #[test]
    fn provider_death_retains_raw_progress() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = input();
        let worktree = temp.path().join("retained-worktree");
        std::fs::create_dir(&worktree)?;
        std::fs::write(
            worktree.join("partial.txt"),
            "parser implementation survives",
        )?;
        input.request.task.access.write_roots.push(worktree.clone());
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempt_id;
        let mut old = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let root = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&root, OsStr::new("candidate-1"), true)?;
        let secret_shape = "sk-abcdefghijklmnopqrstuvwx";
        let mut progress: serde_json::Value = serde_json::from_slice(&snapshot("stopped_early"))?;
        progress["note"] = serde_json::json!(format!(
            "Parser committed; renderer remains.\n{secret_shape}"
        ));
        for field in ["remaining", "decisions", "verify"] {
            progress[field]
                .as_array_mut()
                .ok_or("progress array")?
                .extend([
                    serde_json::json!(format!("\n{secret_shape}")),
                    serde_json::json!(format!("\t{secret_shape}")),
                ]);
        }
        progress["future"] = serde_json::json!({"nested":[format!("\n{secret_shape}"), {"token":format!("\t{secret_shape}")} ]});
        seal(
            &candidate,
            "handoff.json",
            &serde_json::to_vec(&progress)?,
            HANDOFF_BYTES as u64,
        )?;
        let candidate_path = store
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{id}/provider-state/candidate-1"));
        grant(&mut old, &candidate_path)?;
        let route = RouteRecord {
            account_label: Some("one".into()),
            account_key: Some("fixture-one".into()),
            ..crate::tests::route()
        };
        old.resolved_route = Some(route.clone());
        old.applied_route = Some(route);
        old.provider_launched = Some(true);
        old.state = AttemptState::Running {
            extra: Default::default(),
            child_wait: None,
        };
        store.db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(&old)?, id.to_string()],
        )?;
        let terminal = store.finish(id, &attempt, &exit(), &RecoveringAccounts, &call)?;
        assert!(matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::QuotaLimited,
                ..
            }
        ));
        let handoff = store.handoff(&terminal)??.ok_or("missing snapshot")?;
        assert_eq!(
            handoff.remaining,
            [
                "Finish renderer",
                &format!("\n{secret_shape}"),
                &format!("\t{secret_shape}")
            ]
        );
        assert_eq!(
            handoff.extra["future"]["nested"][1]["token"],
            format!("\t{secret_shape}")
        );
        assert!(handoff.fenced().contains(secret_shape));
        // Progress survives temporary cleanup and provider history expiry.
        crate::artifacts::remove_entry_tree(&attempt, OsStr::new("provider-state"))?;
        assert_eq!(store.handoff(&old)??, Some(handoff.clone()));
        assert_eq!(
            std::fs::read_to_string(worktree.join("partial.txt"))?,
            "parser implementation survives"
        );
        Ok(())
    }
    // Spec: docs/specs/run-settlement.md#retention-and-disclosure
    #[test]
    fn final_handoff_preserves_decoded_strings_and_validates_bounds() -> TestResult {
        for prefix in ["\n", "\t"] {
            for field in ["note", "remaining", "decisions", "verify", "future"] {
                let temp = tempfile::tempdir()?;
                let clock = clock();
                let cancel = provenant_core::CancelToken::new();
                let call = call(&clock, &cancel);
                let store = RunStore::open(&context(temp.path())?, &call)?;
                let input = input();
                store.create(&input, &call)?;
                let record = store.record(input.attempt_id, &call)?;
                let attempt = store.attempt_dir(record.attempt_id, false)?;
                let secret = format!("{prefix}sk-abcdefghijklmnopqrstuvwx");
                let mut value: serde_json::Value = serde_json::from_slice(&snapshot("done"))?;
                value[field] = match field {
                    "note" => serde_json::json!(secret),
                    "future" => {
                        serde_json::json!({secret.clone():[secret.clone(),{"value":secret}]})
                    }
                    _ => serde_json::json!([secret]),
                };
                let mut result = decoded();
                result.complete = true;
                result.outcome = Outcome::Succeeded;
                result.result = Handoff::from_json(&serde_json::to_vec(&value)?)?.fenced();
                let safe = store
                    .capture_handoff(&attempt, &record, &result)?
                    .handoff?
                    .ok_or("final snapshot")?;
                assert!(safe.fenced().contains("sk-abcdefghijklmnopqrstuvwx"));
                assert_eq!(store.handoff(&record)??, Some(safe));
            }
        }
        let mut handoff = Handoff::from_json(&snapshot("done"))?;
        handoff.note = "x".repeat(provenant_records::handoff::NOTE_BYTES + 1);
        assert!(matches!(
            validate_handoff(&handoff),
            Err(RunError::Handoff(_))
        ));
        Ok(())
    }

    // Spec: docs/specs/run-settlement.md#settlement
    #[test]
    fn final_handoff_records_the_single_trailing_verdict_tolerance() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let mut result = decoded();
        result.complete = true;
        result.outcome = Outcome::Succeeded;
        result.result = format!(
            "{}\nVERDICT: PASS\n",
            Handoff::from_json(&snapshot("done"))?.fenced()
        );

        let captured = store.capture_handoff(&attempt, &record, &result)?;
        assert!(captured.trailing_verdict_tolerated);
        assert!(!captured.trailing_prose_tolerated);
        assert!(captured.handoff.as_ref().is_ok_and(Option::is_some));
        assert_eq!(store.handoff(&record)?, captured.handoff);
        Ok(())
    }

    // Spec: docs/specs/run-settlement.md#settlement
    #[test]
    fn final_handoff_records_the_trailing_prose_tolerance() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let mut result = decoded();
        result.complete = true;
        result.outcome = Outcome::Succeeded;
        result.result = format!(
            "{}\nCommit pushed. Now updating the PR body ...\n",
            Handoff::from_json(&snapshot("done"))?.fenced()
        );

        let captured = store.capture_handoff(&attempt, &record, &result)?;
        assert!(captured.trailing_prose_tolerated);
        assert!(!captured.trailing_verdict_tolerated);
        assert!(captured.handoff.as_ref().is_ok_and(Option::is_some));
        assert_eq!(store.handoff(&record)?, captured.handoff);
        Ok(())
    }

    #[test]
    fn oversized_handoff_brief_does_not_block_settlement() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = input();
        input.prompt = "x".repeat(RECORD_LIMIT as usize - 64);
        input.request.task.prompt_digest =
            provenant_records::CanonicalV1::file_digest(input.prompt.as_bytes());
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempt_id;
        let old = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let mut result = decoded();
        result.result = Handoff::from_json(&snapshot("stopped_early"))?.fenced();
        store.capture_handoff(&attempt, &old, &result)?.handoff?;
        store
            .db
            .execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
        let mut publication = publication();
        publication.outcome = Outcome::QuotaLimited;
        let terminal = store.publish_terminal(id, 1, &publication, &exit(), &call)?;
        assert!(matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::QuotaLimited,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn final_answer_precedes_progress_and_bad_progress_never_guesses_completion() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
        let base = store
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{}/provider-state", record.attempt_id));
        grant(&mut record, &base.join("candidate-0"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .handoff?
                .is_none()
        );
        seal(
            &candidate,
            "handoff.json",
            &snapshot("stopped_early"),
            HANDOFF_BYTES as u64,
        )?;
        // Another candidate's progress is not the applied candidate's evidence.
        grant(&mut record, &base.join("candidate-1"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .handoff?
                .is_none()
        );
        grant(&mut record, &base.join("candidate-0"))?;
        let mut result = decoded();
        result.outcome = Outcome::Succeeded;
        result.complete = true;
        result.result = "Completed prose without a declaration".into();
        assert!(
            store
                .capture_handoff(&attempt, &record, &result)?
                .handoff?
                .is_none()
        );
        let done = Handoff::from_json(&snapshot("done"))?.fenced();
        let example = format!("````markdown\n{done}\n````");
        result.result = example.clone();
        assert!(
            store
                .capture_handoff(&attempt, &record, &result)?
                .handoff?
                .is_none()
        );
        result.result = format!("{example}\n{done}");
        assert!(
            !store
                .capture_handoff(&attempt, &record, &result)?
                .handoff?
                .ok_or("missing final")?
                .unfinished()
        );
        Ok(())
    }

    #[test]
    fn unsafe_or_oversize_progress_is_a_typed_error_without_following_links() -> TestResult {
        use std::os::unix::fs::symlink;
        for link in [true, false] {
            let temp = tempfile::tempdir()?;
            let clock = clock();
            let cancel = provenant_core::CancelToken::new();
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let input = input();
            store.create(&input, &call)?;
            let mut record = store.record(input.attempt_id, &call)?;
            let attempt = store.attempt_dir(record.attempt_id, false)?;
            let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
            let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
            let base = store
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{}/provider-state", record.attempt_id));
            // An ungranted location is absent evidence, even if it has a file.
            grant(&mut record, temp.path())?;
            assert_eq!(
                store
                    .capture_handoff(&attempt, &record, &decoded())?
                    .handoff,
                Ok(None)
            );
            grant(&mut record, &base.join("candidate-0"))?;
            if link {
                let outside = temp.path().join("outside.json");
                std::fs::write(&outside, snapshot("stopped_early"))?;
                symlink(&outside, base.join("candidate-0/handoff.json"))?;
            } else {
                seal(
                    &candidate,
                    "handoff.json",
                    &vec![b'x'; HANDOFF_BYTES + 1],
                    (HANDOFF_BYTES + 1) as u64,
                )?;
            }
            let result = store.capture_handoff(&attempt, &record, &decoded())?;
            assert!(result.handoff.is_err());
            assert_eq!(store.handoff(&record)?, result.handoff);
        }
        Ok(())
    }

    // Spec: docs/specs/run-settlement.md#retention-and-disclosure
    #[test]
    fn malformed_final_retains_fields_and_never_uses_stale_progress() -> TestResult {
        for complete in [true, false] {
            let temp = tempfile::tempdir()?;
            let clock = clock();
            let cancel = provenant_core::CancelToken::new();
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let input = input();
            store.create(&input, &call)?;
            let mut record = store.record(input.attempt_id, &call)?;
            let attempt = store.attempt_dir(record.attempt_id, false)?;
            let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
            let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
            let base = store
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{}/provider-state", record.attempt_id));
            grant(&mut record, &base.join("candidate-0"))?;
            seal(
                &candidate,
                "handoff.json",
                &snapshot("done"),
                HANDOFF_BYTES as u64,
            )?;
            let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
            let value = serde_json::json!({"schema_version":2,"status":"done","note":format!("New note {secret}"),"remaining":["Unfinished work"],"decisions":[],"verify":[false]});
            let mut result = decoded();
            result.complete = complete;
            result.outcome = Outcome::Succeeded;
            result.result = format!("```handoff\n{value}\n```");
            let captured = store.capture_handoff(&attempt, &record, &result)?;
            let error = captured.handoff.as_ref().unwrap_err();
            assert_eq!(error.dropped[0].field, "verify[0]");
            assert_eq!(
                error.retained["remaining"],
                serde_json::json!(["Unfinished work"])
            );
            assert!(serde_json::to_string(error)?.contains(secret));
            assert_eq!(store.handoff(&record)?, captured.handoff);
            result.result = Handoff::from_json(&snapshot("done"))?.fenced();
            assert_eq!(store.capture_handoff(&attempt, &record, &result)?, captured);
        }
        Ok(())
    }
}
