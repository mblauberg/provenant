//! The one way library code reports a warning: a typed value with a stable
//! code, routed to a capture, the command's sink or stderr.
use crate::provenance::Source;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, RwLock};

/// A typed warning. The code is stable (like a refusal code, ADR 0034); the
/// message is one sentence for a person and never carries a secret value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    /// Stable UPPER_SNAKE code from the declared catalogue in this module.
    pub code: Cow<'static, str>,
    /// One sentence for a person; never a secret value (redaction as for errors).
    pub message: String,
    /// What it is about: a path, key, branch, attempt or route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The setting that caused it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// One command or edit that clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    /// How repeats collapse. Never part of the wire.
    #[serde(skip)]
    pub dedup: Dedup,
}

/// How a repeated warning collapses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dedup {
    /// The same key collapses within one command (count kept).
    #[default]
    PerInvocation,
    /// Shown once until its key changes; remembered in the state root.
    PerChange,
}

/// A declared warning code: one const per code with a one-line meaning.
/// Adding a code is additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WarningCode(pub &'static str);

impl WarningCode {
    /// The stable UPPER_SNAKE spelling.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// A configured key was ignored: it is not part of the declared schema.
pub const CONFIG_KEY_IGNORED: WarningCode = WarningCode("CONFIG_KEY_IGNORED");
/// A configured value was malformed, so the default applied.
pub const CONFIG_MALFORMED: WarningCode = WarningCode("CONFIG_MALFORMED");
/// A candidate value came from a layer its family does not list.
pub const SETTING_LAYER_IGNORED: WarningCode = WarningCode("SETTING_LAYER_IGNORED");
/// A name differed only by case and was folded to its canonical spelling.
pub const NAME_CASE_FOLDED: WarningCode = WarningCode("NAME_CASE_FOLDED");
/// A name matched a declared alias rather than its canonical spelling.
pub const NAME_ALIAS_USED: WarningCode = WarningCode("NAME_ALIAS_USED");
/// The lane's branch is behind its base and will be updated.
pub const BRANCH_BEHIND_BASE: WarningCode = WarningCode("BRANCH_BEHIND_BASE");
/// An existing worktree was adopted although it had uncommitted changes.
pub const WORKTREE_ADOPTED_DIRTY: WarningCode = WarningCode("WORKTREE_ADOPTED_DIRTY");
/// This release reads and writes a different live-record format.
pub const LIVE_RECORD_FORMAT_CHANGED: WarningCode = WarningCode("LIVE_RECORD_FORMAT_CHANGED");
/// One install-time migration step failed; the install continues.
pub const MIGRATION_STEP_FAILED: WarningCode = WarningCode("MIGRATION_STEP_FAILED");
/// The state root overlaps a location the product treats as advisory.
pub const STATE_ROOT_OVERLAP: WarningCode = WarningCode("STATE_ROOT_OVERLAP");

impl Warning {
    /// A warning with no subject, source or repair.
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Self {
            code: Cow::Borrowed(code.0),
            message: message.into(),
            subject: None,
            source: None,
            repair: None,
            dedup: Dedup::default(),
        }
    }

    /// Name what the warning is about.
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Name the setting that caused it.
    pub fn source(mut self, source: Source) -> Self {
        self.source = Some(source);
        self
    }

    /// State one command or edit that clears it.
    pub fn repair(mut self, repair: impl Into<String>) -> Self {
        self.repair = Some(repair.into());
        self
    }

    /// Show it once until its key changes instead of once per invocation.
    pub fn once_per_change(mut self) -> Self {
        self.dedup = Dedup::PerChange;
        self
    }

    /// sha256 over code, subject and message: the dedup key.
    pub fn key(&self) -> String {
        use sha2::{Digest as _, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"provenant-warning\0");
        digest.update(self.code.as_bytes());
        digest.update(b"\0");
        digest.update(self.subject.as_deref().unwrap_or_default().as_bytes());
        digest.update(b"\0");
        digest.update(self.message.as_bytes());
        format!("sha256:{}", provenant_core::hex(&digest.finalize()))
    }
}

/// A command's warning consumer. The binary edge installs it.
pub trait Sink: Send + Sync {
    /// Accept one warning.
    fn accept(&self, w: Warning);
}

/// Installs the process sink for the current command; dropping the guard
/// restores the previous one.
pub fn install(sink: Arc<dyn Sink>) -> SinkGuard {
    let previous = SINK
        .write()
        .map(|mut slot| slot.replace(sink))
        .unwrap_or(None);
    SinkGuard { previous }
}

/// Restores the process sink that was installed before its [`install`].
pub struct SinkGuard {
    previous: Option<Arc<dyn Sink>>,
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = SINK.write() {
            *slot = self.previous.take();
        }
    }
}

/// The only way library code reports a warning. Routes to, in order: the
/// innermost capture on this thread; the process sink the binary installed
/// for this command (one command per process, so worker threads reach it);
/// else stderr as `warning: <message>`.
#[allow(clippy::print_stderr, clippy::disallowed_methods)]
pub fn warn(w: Warning) {
    let captured = CAPTURES.with(|captures| {
        let mut stack = captures.borrow_mut();
        match stack.last_mut() {
            Some(frame) => {
                frame.push(w.clone());
                true
            }
            None => false,
        }
    });
    if captured {
        return;
    }
    let sink = SINK.read().map(|slot| slot.clone()).unwrap_or(None);
    if let Some(sink) = sink {
        sink.accept(w);
        return;
    }
    eprintln!("warning: {}", w.message);
}

/// Thread-local: collects warnings raised on this thread inside `f` (tests, or
/// an owner that must return them as data). Parallel tests do not see each
/// other's warnings.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Warning>) {
    struct PopOnDrop;
    impl Drop for PopOnDrop {
        fn drop(&mut self) {
            CAPTURES.with(|captures| {
                captures.borrow_mut().pop();
            });
        }
    }

    CAPTURES.with(|captures| captures.borrow_mut().push(Vec::new()));
    let frame = PopOnDrop;
    let result = f();
    let warnings = CAPTURES.with(|captures| {
        captures
            .borrow_mut()
            .last_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    });
    drop(frame);
    (result, warnings)
}

thread_local! {
    static CAPTURES: RefCell<Vec<Vec<Warning>>> = const { RefCell::new(Vec::new()) };
}

static SINK: RwLock<Option<Arc<dyn Sink>>> = RwLock::new(None);

#[cfg(test)]
mod tests {
    use super::{Dedup, NAME_CASE_FOLDED, Sink, Warning, capture, install, warn};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder {
        messages: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn messages(&self) -> Vec<String> {
            self.messages.lock().expect("recorder lock").clone()
        }
    }

    impl Sink for Recorder {
        fn accept(&self, warning: Warning) {
            self.messages
                .lock()
                .expect("recorder lock")
                .push(warning.message);
        }
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn capture_collects_warnings_and_prints_nothing() {
        let ((), warnings) = capture(|| {
            warn(Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
            ));
            warn(Warning::new(NAME_CASE_FOLDED, "folded `Reader` to `reader`").subject("reader"));
        });
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].code.as_ref(), NAME_CASE_FOLDED.0);
        assert_eq!(warnings[1].subject.as_deref(), Some("reader"));
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn nested_sinks_restore_the_previous_one() {
        let outer = Arc::new(Recorder::default());
        let inner = Arc::new(Recorder::default());
        let outer_guard = install(outer.clone());
        {
            let _inner_guard = install(inner.clone());
            warn(Warning::new(NAME_CASE_FOLDED, "inner"));
        }
        warn(Warning::new(NAME_CASE_FOLDED, "outer"));
        assert_eq!(inner.messages(), ["inner"]);
        assert_eq!(outer.messages(), ["outer"]);
        drop(outer_guard);
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn a_per_change_key_is_stable_for_equal_content_and_follows_the_subject() {
        let first = Warning::new(NAME_CASE_FOLDED, "folded")
            .subject("writer")
            .once_per_change();
        let same = Warning::new(NAME_CASE_FOLDED, "folded")
            .subject("writer")
            .once_per_change();
        let other = Warning::new(NAME_CASE_FOLDED, "folded")
            .subject("reader")
            .once_per_change();
        assert_eq!(first.dedup, Dedup::PerChange);
        assert_eq!(first.key(), same.key());
        assert_ne!(first.key(), other.key());
        assert!(first.key().starts_with("sha256:"));
    }

    #[test]
    fn the_catalogue_codes_are_upper_snake() {
        for code in [
            super::CONFIG_KEY_IGNORED,
            super::CONFIG_MALFORMED,
            super::SETTING_LAYER_IGNORED,
            super::NAME_CASE_FOLDED,
            super::NAME_ALIAS_USED,
            super::BRANCH_BEHIND_BASE,
            super::WORKTREE_ADOPTED_DIRTY,
            super::LIVE_RECORD_FORMAT_CHANGED,
            super::MIGRATION_STEP_FAILED,
            super::STATE_ROOT_OVERLAP,
        ] {
            let name = code.as_str();
            assert!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
                "{name}"
            );
        }
    }
}
