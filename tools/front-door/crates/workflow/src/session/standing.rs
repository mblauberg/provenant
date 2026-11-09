//! The chair's standing record: goal, standing grants, open obligations and
//! what waits on the owner. It is a typed `provenant_standing` key in the
//! checkpoint frontmatter, beside `provenant_summary`, so it is saved, revised
//! and restored with the checkpoint itself.
//!
//! The record is self-attested by the chair and is not verified. A grant is a
//! pointer: `owner:<ref>` names where the owner gave it and `rule:<ref>` a
//! durable rule. Nothing resolves a source, and only its shape is checked, so
//! a grant lets a chair skip asking for what it could already do, no more.
//! A grant without a well-formed source is shown as unverified and is not
//! acted on; an expired one is not restored. Landing, publishing and
//! chair-switching gates check held authority themselves, and an irreversible
//! action should be checked against the grant's source first.
use super::summary::{HEADER_LIMIT, empty_header, header_end, parse_header, strip_root_key};
use provenant_core::UtcMicros;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const STANDING_KEY: &str = "provenant_standing";
/// The most a restored standing section contributes to hook output. It shares
/// the intent section's budget, so the hook output bound is unchanged.
pub const STANDING_LIMIT: usize = 1536;
const HEADING: &str = "## Standing";
/// Longest grant scope, ask or obligation shown; a longer grant scope is not
/// shown in part, since a cut scope could read as a wider grant.
const ITEM_LIMIT: usize = 240;
const SOURCE_LIMIT: usize = 512;
/// Most entries shown per list before the rest are counted.
const LIST_LIMIT: usize = 8;

/// The typed section. Every field is required, so an omitted
/// `waiting_on_owner` never reads as "nothing waits".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub schema_version: u64,
    pub goal: String,
    pub grants: Vec<Grant>,
    pub obligations: Vec<String>,
    /// The specific asks waiting on the owner; empty means nothing waits.
    pub waiting_on_owner: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub scope: String,
    /// `owner:<ref>` or `rule:<ref>`, one token with no spaces. Anything
    /// else, including prose, leaves the grant unverified.
    #[serde(default)]
    pub source: Option<String>,
    /// An RFC 3339 instant, or absent or `none` for a grant without expiry.
    #[serde(default)]
    pub expires: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantState {
    Active,
    Unverified,
    Expired,
}

impl Grant {
    pub fn state(&self, now: UtcMicros) -> GrantState {
        let expiry = match self.expires.as_deref().map(str::trim) {
            None | Some("none") => None,
            Some(text) => match UtcMicros::parse_rfc3339(text) {
                Ok(at) => Some(at),
                Err(_) => return GrantState::Unverified,
            },
        };
        if expiry.is_some_and(|at| at <= now) {
            return GrantState::Expired;
        }
        let sourced = self.source.as_deref().is_some_and(|source| {
            source.len() <= SOURCE_LIMIT
                && !source.chars().any(|c| c.is_whitespace() || c.is_control())
                && source
                    .split_once(':')
                    .is_some_and(|(kind, at)| matches!(kind, "owner" | "rule") && !at.is_empty())
        });
        let shown = !self.scope.trim().is_empty() && self.scope.len() <= ITEM_LIMIT;
        if sourced && shown {
            GrantState::Active
        } else {
            GrantState::Unverified
        }
    }
}

#[derive(Debug, Error)]
#[error("invalid or unsupported checkpoint standing section")]
pub struct StandingError;

/// Read the standing section from YAML frontmatter. A checkpoint without one
/// is distinct from an invalid one.
pub fn parse_standing(markdown: &str) -> Result<Option<Standing>, StandingError> {
    let Some(end) = header_end(markdown).map_err(|_| StandingError)? else {
        return Ok(None);
    };
    if end > HEADER_LIMIT {
        return Err(StandingError);
    }
    if empty_header(markdown, end) {
        return Ok(None);
    }
    let mut root = parse_header(markdown, end).map_err(|_| StandingError)?;
    let Some(node) = root.remove(STANDING_KEY) else {
        return Ok(None);
    };
    let standing: Standing = serde_json::from_value(node).map_err(|_| StandingError)?;
    if standing.schema_version != 1 {
        return Err(StandingError);
    }
    Ok(Some(standing))
}

/// The checkpoint without its standing section, for the prose excerpt: the
/// section is restored on its own and is never shown raw beside it.
pub fn without_standing(markdown: &str) -> String {
    strip_root_key(markdown, STANDING_KEY).unwrap_or_else(|_| markdown.to_owned())
}

fn item(text: &str) -> String {
    super::clean(text.trim(), ITEM_LIMIT)
}

fn list(lines: &mut Vec<String>, items: impl ExactSizeIterator<Item = String>) {
    let total = items.len();
    lines.extend(items.take(LIST_LIMIT).map(|text| format!("- {text}")));
    if total > LIST_LIMIT {
        lines.push(format!("- and {} more", total - LIST_LIMIT));
    }
}

/// The section a restored chair sees, in whole lines within `limit` bytes.
/// `None` when the checkpoint has no standing section.
pub fn restore_standing(markdown: &str, now: UtcMicros, limit: usize) -> Option<String> {
    let mut lines = vec![HEADING.to_owned()];
    match parse_standing(markdown) {
        Ok(None) => return None,
        Err(_) => lines.push(
            "The checkpoint's standing section is invalid, so no goal, grant or obligation is restored from it. Act on no grant from it until it is saved again with its source.".into(),
        ),
        Ok(Some(standing)) => {
            lines.push("Restored from the typed record. Grants are the chair's own record, each with a source to check; gates still check held authority. Check an irreversible action against its source first.".into());
            lines.push(format!(
                "Goal: {}",
                if standing.goal.trim().is_empty() {
                    "not recorded".into()
                } else {
                    item(&standing.goal)
                }
            ));
            if standing.waiting_on_owner.is_empty() {
                lines.push("Waiting on owner: nothing".into());
            } else {
                lines.push("Waiting on owner:".into());
                list(&mut lines, standing.waiting_on_owner.iter().map(|ask| item(ask)));
            }
            let by = |wanted| {
                standing
                    .grants
                    .iter()
                    .filter(move |grant| grant.state(now) == wanted)
            };
            let active: Vec<String> = by(GrantState::Active)
                .map(|grant| {
                    format!(
                        "{} (source {}; expires {})",
                        item(&grant.scope),
                        super::clean(grant.source.as_deref().unwrap_or_default(), SOURCE_LIMIT),
                        super::clean(grant.expires.as_deref().unwrap_or("none"), 64)
                    )
                })
                .collect();
            lines.push(if active.is_empty() {
                "Standing grants: none".into()
            } else {
                "Standing grants (act within these without asking; sources are pointers to check):".into()
            });
            list(&mut lines, active.into_iter());
            let unverified: Vec<String> = by(GrantState::Unverified)
                .map(|grant| {
                    if grant.scope.len() > ITEM_LIMIT {
                        "a grant whose scope is too long to show".into()
                    } else {
                        item(&grant.scope)
                    }
                })
                .collect();
            if !unverified.is_empty() {
                lines.push("Unverified grants (no owner or rule source, or unreadable; do not act on these):".into());
                list(&mut lines, unverified.into_iter());
            }
            let expired = by(GrantState::Expired).count();
            if expired > 0 {
                lines.push(format!("Expired grants not restored: {expired}"));
            }
            if standing.obligations.is_empty() {
                lines.push("Open obligations: none".into());
            } else {
                lines.push("Open obligations:".into());
                list(&mut lines, standing.obligations.iter().map(|text| item(text)));
            }
        }
    }
    let cut = "[standing cut; read the checkpoint for the rest]";
    let mut text = String::new();
    for (index, line) in lines.iter().enumerate() {
        let room = if index + 1 == lines.len() {
            0
        } else {
            cut.len() + 1
        };
        if text.len() + line.len() + 1 + room > limit {
            text.push_str(cut);
            return Some(text);
        }
        text.push_str(line);
        text.push('\n');
    }
    Some(text.trim_end().to_owned())
}

/// What `provenant status` reports as waiting on the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerWait {
    /// No chair checkpoint, or one without a standing section.
    NotRecorded,
    Invalid,
    Nothing,
    Asks(Vec<String>),
    /// The checkpoint is older than the freshness rule, so its asks are not current.
    Stale,
    /// The checkpoint could not be read.
    Unknown(String),
}

/// An [`OwnerWait`] with the chair session it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerStatus {
    pub wait: OwnerWait,
    pub session: Option<String>,
    pub saved_at: Option<provenant_core::UtcMicros>,
    /// Other sessions of the same actor saved within the freshness window.
    pub other_live: Vec<String>,
}

impl OwnerStatus {
    pub fn new(
        wait: OwnerWait,
        session: Option<String>,
        saved_at: Option<provenant_core::UtcMicros>,
        other_live: Vec<String>,
    ) -> Self {
        Self {
            wait,
            session,
            saved_at,
            other_live,
        }
    }

    pub fn line(&self) -> String {
        let mut line = self.wait.line();
        if let Some(session) = &self.session {
            line.push_str(&format!(" [session {}", super::clean(session, 128)));
            if let Some(at) = self.saved_at {
                line.push_str(&format!(", saved {}", at.as_timestamp()));
            }
            line.push(']');
        }
        if !self.other_live.is_empty() {
            line.push_str(&format!(
                "\nwaiting on owner: {} other live chair session(s) for this actor ({}); showing the newest",
                self.other_live.len(),
                self.other_live
                    .iter()
                    .take(LIST_LIMIT)
                    .map(|id| super::clean(id, 128))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        line
    }

    pub fn value(&self) -> serde_json::Value {
        let mut value = self.wait.value();
        value["session"] = serde_json::json!(self.session);
        value["saved_at"] =
            serde_json::json!(self.saved_at.map(|at| at.as_timestamp().to_string()));
        value["other_live_sessions"] = serde_json::json!(self.other_live);
        value
    }
}

impl OwnerWait {
    pub fn from_checkpoint(markdown: Option<&str>) -> Self {
        match markdown.map(parse_standing) {
            None | Some(Ok(None)) => Self::NotRecorded,
            Some(Err(_)) => Self::Invalid,
            Some(Ok(Some(standing))) if standing.waiting_on_owner.is_empty() => Self::Nothing,
            Some(Ok(Some(standing))) => Self::Asks(
                standing
                    .waiting_on_owner
                    .iter()
                    .map(|ask| item(ask))
                    .collect(),
            ),
        }
    }

    pub fn line(&self) -> String {
        match self {
            Self::NotRecorded => {
                "waiting on owner: not recorded (no standing section in the chair checkpoint)"
                    .into()
            }
            Self::Invalid => {
                "waiting on owner: unknown (the chair checkpoint's standing section is invalid)"
                    .into()
            }
            Self::Stale => "waiting on owner: unknown (the chair checkpoint is stale, so its asks are not current)".into(),
            Self::Unknown(cause) => format!(
                "waiting on owner: unknown (the chair checkpoint could not be read: {})",
                super::clean(cause, 160)
            ),
            Self::Nothing => "waiting on owner: nothing".into(),
            Self::Asks(asks) => {
                let mut shown: Vec<_> = asks.iter().take(LIST_LIMIT).cloned().collect();
                if asks.len() > LIST_LIMIT {
                    shown.push(format!("and {} more", asks.len() - LIST_LIMIT));
                }
                format!("waiting on owner: {}", shown.join("; "))
            }
        }
    }

    pub fn value(&self) -> serde_json::Value {
        let (state, asks): (&str, &[String]) = match self {
            Self::Stale => ("stale", &[]),
            Self::Unknown(_) => ("unknown", &[]),
            Self::NotRecorded => ("not_recorded", &[]),
            Self::Invalid => ("invalid", &[]),
            Self::Nothing => ("nothing", &[]),
            Self::Asks(asks) => ("asks", asks),
        };
        serde_json::json!({"state": state, "asks": asks})
    }
}

#[cfg(test)]
mod tests {
    use super::OwnerWait;

    #[test]
    fn status_reports_nothing_or_the_specific_ask() {
        let record = |asks: &str| {
            format!(
                "---\nprovenant_standing:\n  schema_version: 1\n  goal: \"G\"\n  grants: []\n  obligations: []\n  waiting_on_owner: {asks}\n---\nBody\n"
            )
        };
        let nothing = OwnerWait::from_checkpoint(Some(&record("[]")));
        assert_eq!(nothing.line(), "waiting on owner: nothing");
        assert_eq!(nothing.value()["state"], "nothing");
        let ask = OwnerWait::from_checkpoint(Some(&record("[\"Approve the release window\"]")));
        assert_eq!(ask.line(), "waiting on owner: Approve the release window");
        assert_eq!(ask.value()["asks"][0], "Approve the release window");
        // An omitted field is invalid, never "nothing"; no section is not recorded.
        let omitted = record("[]").replace("  waiting_on_owner: []\n", "");
        assert_eq!(
            OwnerWait::from_checkpoint(Some(&omitted)),
            OwnerWait::Invalid
        );
        assert_eq!(
            OwnerWait::from_checkpoint(Some("# Notes\n")),
            OwnerWait::NotRecorded
        );
        assert_eq!(OwnerWait::from_checkpoint(None), OwnerWait::NotRecorded);
    }

    #[test]
    fn stale_and_unreadable_are_unknown_and_name_the_session() {
        use super::OwnerStatus;
        let at = provenant_core::UtcMicros::from_micros(0).unwrap();
        let stale = OwnerStatus::new(
            OwnerWait::Stale,
            Some("s1".into()),
            Some(at),
            vec!["s2".into()],
        );
        assert!(stale.line().contains("stale"));
        assert!(
            stale
                .line()
                .contains("session s1, saved 1970-01-01T00:00:00Z")
        );
        assert!(stale.line().contains("1 other live chair session"));
        assert_eq!(stale.value()["state"], "stale");
        assert_eq!(stale.value()["session"], "s1");
        let bad = OwnerStatus::new(OwnerWait::Unknown("io".into()), None, None, vec![]);
        assert_eq!(bad.value()["state"], "unknown");
        assert!(bad.line().starts_with("waiting on owner: unknown"));
    }
}
