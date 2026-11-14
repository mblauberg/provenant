//! The one command target form, `NAME[/NODE[#N]]`, as `docs/specs/cli.md#Targets`
//! defines it. Parsing is syntactic: whether the graph or node exists is the
//! resolver's question, answered later as `NOT_FOUND`.

use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use provenant_records::work_key::{NODE_ID_BYTES, valid_node};

/// Longest graph name in bytes.
pub const NAME_BYTES: usize = 48;

/// One parsed target: a graph, optionally narrowed to a node and an attempt.
/// An attempt only has meaning with a node, so `Display` omits it otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub node: Option<String>,
    pub attempt: Option<NonZeroU32>,
}

/// One variant per distinct fault. Each renders as one line that shows the
/// accepted form.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    #[error("empty target; use NAME[/NODE[#N]]")]
    Empty,
    #[error("a name starts with a lowercase letter; use NAME[/NODE[#N]]")]
    NameStart,
    #[error("a name has lowercase letters, digits and hyphens only; use NAME[/NODE[#N]]")]
    NameCharacter,
    #[error("a name has single hyphens and does not end in one; use NAME[/NODE[#N]]")]
    NameHyphen,
    #[error("a name is at most {NAME_BYTES} bytes; use NAME[/NODE[#N]]")]
    NameTooLong,
    #[error("a node is missing after '/'; use NAME[/NODE[#N]]")]
    NodeEmpty,
    #[error("a node has letters, digits, '.', '_' and '-' only; use NAME[/NODE[#N]]")]
    NodeCharacter,
    #[error("a node is at most {NODE_ID_BYTES} bytes; use NAME[/NODE[#N]]")]
    NodeTooLong,
    #[error("a target has one '/', between name and node; use NAME[/NODE[#N]]")]
    TooManySegments,
    #[error("an attempt needs a node before '#'; use NAME[/NODE[#N]]")]
    AttemptWithoutNode,
    #[error("an attempt is a positive decimal without a leading zero; use NAME[/NODE[#N]]")]
    AttemptNotNumber,
    #[error("attempts count from 1, so #0 names nothing; use NAME[/NODE[#N]]")]
    AttemptZero,
}

impl Target {
    pub fn parse(spelling: &str) -> Result<Target, TargetError> {
        if spelling.is_empty() {
            return Err(TargetError::Empty);
        }
        let mut segments = spelling.split('/');
        let head = segments.next().unwrap_or_default();
        let node_part = segments.next();
        if segments.next().is_some() {
            return Err(TargetError::TooManySegments);
        }
        let Some(node_part) = node_part else {
            if head.contains('#') {
                return Err(TargetError::AttemptWithoutNode);
            }
            check_name(head)?;
            return Ok(Target {
                name: head.to_owned(),
                node: None,
                attempt: None,
            });
        };
        check_name(head)?;
        let (node, attempt) = match node_part.split_once('#') {
            Some((node, attempt)) => (node, Some(parse_attempt(attempt)?)),
            None => (node_part, None),
        };
        check_node(node)?;
        Ok(Target {
            name: head.to_owned(),
            node: Some(node.to_owned()),
            attempt,
        })
    }
}

impl FromStr for Target {
    type Err = TargetError;

    fn from_str(spelling: &str) -> Result<Self, Self::Err> {
        Self::parse(spelling)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if let Some(node) = &self.node {
            write!(f, "/{node}")?;
            if let Some(attempt) = self.attempt {
                write!(f, "#{attempt}")?;
            }
        }
        Ok(())
    }
}

fn check_name(name: &str) -> Result<(), TargetError> {
    let Some(first) = name.bytes().next() else {
        return Err(TargetError::Empty);
    };
    if !first.is_ascii_lowercase() {
        return Err(TargetError::NameStart);
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(TargetError::NameCharacter);
    }
    if name.len() > NAME_BYTES {
        return Err(TargetError::NameTooLong);
    }
    if name.ends_with('-') || name.contains("--") {
        return Err(TargetError::NameHyphen);
    }
    Ok(())
}

fn check_node(node: &str) -> Result<(), TargetError> {
    if node.is_empty() {
        return Err(TargetError::NodeEmpty);
    }
    if !valid_node(node) {
        // `valid_node` rejects both faults; tell them apart for the message.
        return Err(
            if node.len() > NODE_ID_BYTES
                && node
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                TargetError::NodeTooLong
            } else {
                TargetError::NodeCharacter
            },
        );
    }
    Ok(())
}

fn parse_attempt(digits: &str) -> Result<NonZeroU32, TargetError> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TargetError::AttemptNotNumber);
    }
    if digits == "0" {
        return Err(TargetError::AttemptZero);
    }
    if digits.starts_with('0') {
        return Err(TargetError::AttemptNotNumber);
    }
    digits
        .parse::<NonZeroU32>()
        .map_err(|_| TargetError::AttemptNotNumber)
}

/// What a verb needs its target to name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// A graph, a node or an attempt: a bare `NAME` is the whole graph.
    Work,
    /// One attempt. A bare `NAME` is its only node, and a node without `#N`
    /// is its newest attempt, or with `settled` its newest settled one.
    Attempt { settled: bool },
}

/// A target checked against its graph's one checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub graph: String,
    pub node: Option<String>,
    /// The attempt number, set when the target selects one attempt.
    pub number: Option<NonZeroU32>,
    /// `HOST/ATTEMPT` of the selected attempt, once it has launched.
    pub attempt_ref: Option<String>,
}

impl Resolved {
    /// The canonical spelling, `NAME[/NODE[#N]]`.
    pub fn target(&self) -> Target {
        Target {
            name: self.graph.clone(),
            node: self.node.clone(),
            attempt: self.number,
        }
    }

    /// The run store key of the selected attempt.
    pub fn work_key(
        &self,
        project: provenant_core::ProjectId,
    ) -> Option<provenant_records::WorkKey> {
        provenant_records::WorkKey::new(
            project,
            self.graph.clone(),
            self.node.clone()?,
            self.number?.get(),
        )
        .ok()
    }
}

/// Resolution faults. Each names the target as the caller spelled it.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError<E> {
    #[error("NOT_FOUND: {0}")]
    NotFound(String),
    #[error("AMBIGUOUS_TARGET: {target} has several nodes; use one of {}", .candidates.join(", "))]
    Ambiguous {
        target: String,
        candidates: Vec<String>,
    },
    #[error(transparent)]
    Read(E),
}

/// Most candidates an ambiguity lists.
const CANDIDATES: usize = 20;

/// Resolve `target` by reading only its own graph's checkpoint: `read` is
/// called once, with the target's name, so no other graph is ever opened.
pub fn resolve<E>(
    target: &Target,
    want: Want,
    read: impl FnOnce(&str) -> Result<Option<provenant_fabric_runtime::graph::GraphCheckpoint>, E>,
) -> Result<Resolved, ResolveError<E>> {
    use provenant_fabric_runtime::graph::AttemptState;
    let checkpoint = read(&target.name)
        .map_err(ResolveError::Read)?
        .ok_or_else(|| ResolveError::NotFound(target.name.clone()))?;
    let node = match (&target.node, want) {
        (Some(node), _) => Some(node.clone()),
        (None, Want::Work) => None,
        (None, Want::Attempt { .. }) => match checkpoint.nodes.as_slice() {
            [only] => Some(only.id.clone()),
            nodes => {
                return Err(ResolveError::Ambiguous {
                    target: target.to_string(),
                    candidates: nodes
                        .iter()
                        .take(CANDIDATES)
                        .map(|node| format!("{}/{}", target.name, node.id))
                        .collect(),
                });
            }
        },
    };
    let Some(node) = node else {
        return Ok(Resolved {
            graph: target.name.clone(),
            node: None,
            number: None,
            attempt_ref: None,
        });
    };
    let progress = checkpoint
        .nodes
        .iter()
        .find(|candidate| candidate.id == node)
        .ok_or_else(|| ResolveError::NotFound(format!("{}/{node}", target.name)))?;
    let numbered: Vec<_> = progress
        .recorded_attempts()
        .filter_map(|attempt| Some((attempt, NonZeroU32::new(attempt.key.attempt)?)))
        .collect();
    let selected = match (target.attempt, want) {
        (Some(number), _) => Some(
            numbered
                .iter()
                .find(|(_, n)| *n == number)
                .copied()
                .ok_or_else(|| ResolveError::NotFound(target.to_string()))?,
        ),
        (None, Want::Work) => None,
        (None, Want::Attempt { settled }) => {
            let newest = numbered.last().copied();
            let newest_settled = numbered
                .iter()
                .rfind(|(attempt, _)| attempt.state == AttemptState::Terminal)
                .copied();
            // A node that has never settled still has a newest attempt to read.
            if settled {
                newest_settled.or(newest)
            } else {
                newest
            }
        }
    };
    if target.attempt.is_none() && want != Want::Work && selected.is_none() {
        return Err(ResolveError::NotFound(format!(
            "{}/{node} has no attempt",
            target.name
        )));
    }
    Ok(Resolved {
        graph: target.name.clone(),
        node: Some(node),
        number: selected.map(|(_, number)| number),
        attempt_ref: selected.and_then(|(attempt, _)| attempt.attempt_ref.clone()),
    })
}

#[cfg(test)]
#[path = "target_continuation_invariants.rs"]
mod continuation_invariants;

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str, node: Option<&str>, attempt: Option<u32>) -> Target {
        Target {
            name: name.into(),
            node: node.map(Into::into),
            attempt: attempt.and_then(NonZeroU32::new),
        }
    }

    #[test]
    fn accepts_each_depth() {
        let cases = [
            ("fix-login", target("fix-login", None, None)),
            ("a", target("a", None, None)),
            ("build2", target("build2", None, None)),
            ("fix/review", target("fix", Some("review"), None)),
            (
                "fix/Review_1.a-b",
                target("fix", Some("Review_1.a-b"), None),
            ),
            ("fix/review#1", target("fix", Some("review"), Some(1))),
            ("fix/review#12", target("fix", Some("review"), Some(12))),
            (
                "fix/review#4294967295",
                target("fix", Some("review"), Some(u32::MAX)),
            ),
            ("fix/7", target("fix", Some("7"), None)),
        ];
        for (spelling, expected) in cases {
            assert_eq!(Target::parse(spelling), Ok(expected), "{spelling}");
        }
    }

    #[test]
    fn round_trips_through_display() {
        for spelling in ["fix-login", "fix/review", "fix/review#3", "a1-b2/x.y_z-0#9"] {
            let parsed = Target::parse(spelling).unwrap();
            assert_eq!(parsed.to_string(), spelling);
            assert_eq!(Target::parse(&parsed.to_string()), Ok(parsed));
            assert_eq!(spelling.parse::<Target>(), Target::parse(spelling));
        }
    }

    #[test]
    fn name_length_boundary() {
        let longest = format!("a{}", "b".repeat(NAME_BYTES - 1));
        assert_eq!(Target::parse(&longest).unwrap().name, longest);
        let over = format!("{longest}b");
        assert_eq!(Target::parse(&over), Err(TargetError::NameTooLong));
    }

    #[test]
    fn node_length_boundary() {
        let longest = "n".repeat(NODE_ID_BYTES);
        assert_eq!(
            Target::parse(&format!("g/{longest}")).unwrap().node,
            Some(longest.clone())
        );
        assert_eq!(
            Target::parse(&format!("g/{longest}n")),
            Err(TargetError::NodeTooLong)
        );
    }

    #[test]
    fn refuses_each_fault() {
        let cases = [
            ("", TargetError::Empty),
            ("Fix", TargetError::NameStart),
            ("1fix", TargetError::NameStart),
            ("-fix", TargetError::NameStart),
            ("42", TargetError::NameStart),
            ("fix_login", TargetError::NameCharacter),
            ("fixLogin", TargetError::NameCharacter),
            ("fix login", TargetError::NameCharacter),
            ("fix.login", TargetError::NameCharacter),
            ("fix-", TargetError::NameHyphen),
            ("fix--login", TargetError::NameHyphen),
            ("/node", TargetError::Empty),
            ("fix/", TargetError::NodeEmpty),
            ("fix/#2", TargetError::NodeEmpty),
            ("fix/no de", TargetError::NodeCharacter),
            ("fix/a:b", TargetError::NodeCharacter),
            ("fix/a#b", TargetError::AttemptNotNumber),
            ("fix/a/b", TargetError::TooManySegments),
            ("fix/a/b/c", TargetError::TooManySegments),
            ("fix#2", TargetError::AttemptWithoutNode),
            ("fix/a#", TargetError::AttemptNotNumber),
            ("fix/a#0", TargetError::AttemptZero),
            ("fix/a#00", TargetError::AttemptNotNumber),
            ("fix/a#01", TargetError::AttemptNotNumber),
            ("fix/a#-1", TargetError::AttemptNotNumber),
            ("fix/a#+1", TargetError::AttemptNotNumber),
            ("fix/a#1x", TargetError::AttemptNotNumber),
            ("fix/a#1#2", TargetError::AttemptNotNumber),
            ("fix/a#4294967296", TargetError::AttemptNotNumber),
        ];
        for (spelling, expected) in cases {
            assert_eq!(Target::parse(spelling), Err(expected), "{spelling:?}");
        }
    }

    #[test]
    fn letter_led_names_that_look_like_hex_stay_names() {
        for spelling in [
            "deadbeef",
            "abc",
            "face-lift",
            "abcdef0123456789abcdef0123456789",
        ] {
            assert!(
                provenant_fabric_runtime::graph::template::graph_name_fault(spelling).is_none()
            );
            for suffix in ["", "/writer", "/writer#2"] {
                let target = format!("{spelling}{suffix}");
                let parsed = Target::parse(&target).unwrap();
                assert_eq!(parsed.name, spelling);
                assert_eq!(parsed.to_string(), target);
            }
        }
    }

    #[test]
    fn display_leaves_out_an_attempt_without_a_node() {
        assert_eq!(target("fix", None, Some(2)).to_string(), "fix");
    }

    fn checkpoint(
        name: &str,
        nodes: &[(&str, &[&str])],
    ) -> provenant_fabric_runtime::graph::GraphCheckpoint {
        let nodes: Vec<_> = nodes
            .iter()
            .map(|(id, attempts)| {
                let attempts: Vec<_> = attempts
                    .iter()
                    .enumerate()
                    .map(|(index, state)| serde_json::json!({"launched":true,"key":{"graph_id":name,"node_id":id,"route_index":0, "attempt": index + 1},"route":{"alias":"senior"},"state":state,"attempt_ref":format!("host/{id}-{}", index + 1),"launch_actor":"actor"}))
                    .collect();
                serde_json::json!({"id":id,"state":"running","worktree":null,"attempts":attempts})
            })
            .collect();
        serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":name,"plan_digest":"digest","nodes":nodes})).unwrap()
    }

    #[test]
    fn resolution_reads_only_the_named_graph() {
        let graph = checkpoint("fix", &[("review", &["terminal", "running"])]);
        // Every other graph is unreadable; the resolver must never ask for one.
        let read = |name: &str| {
            if name == "fix" {
                Ok(Some(graph.clone()))
            } else {
                Err(format!("unreadable {name}"))
            }
        };
        for (spelling, want) in [
            ("fix", Want::Work),
            ("fix/review", Want::Work),
            ("fix/review#1", Want::Attempt { settled: true }),
            ("fix", Want::Attempt { settled: false }),
        ] {
            let target = Target::parse(spelling).unwrap();
            let resolved = resolve(&target, want, read).unwrap();
            assert_eq!(resolved.graph, "fix", "{spelling}");
        }
    }

    #[test]
    fn steer_and_output_reach_the_same_numbered_attempt() {
        let graph = checkpoint(
            "fix",
            &[
                ("review", &["terminal", "running", "terminal"]),
                ("plan", &["terminal"]),
            ],
        );
        for number in 1..=3u32 {
            let target = Target::parse(&format!("fix/review#{number}")).unwrap();
            let steer = resolve(&target, Want::Attempt { settled: false }, |_| {
                Ok::<_, ()>(Some(graph.clone()))
            })
            .unwrap();
            let output = resolve(&target, Want::Attempt { settled: true }, |_| {
                Ok::<_, ()>(Some(graph.clone()))
            })
            .unwrap();
            assert_eq!(steer, output);
            assert_eq!(
                steer.attempt_ref.as_deref(),
                Some(format!("host/review-{number}").as_str())
            );
            assert_eq!(steer.target(), target);
        }
    }

    #[test]
    fn a_node_reads_its_newest_attempt_and_output_its_newest_settled_one() {
        let graph = checkpoint("fix", &[("review", &["terminal", "running"])]);
        let node = Target::parse("fix/review").unwrap();
        let newest = resolve(&node, Want::Attempt { settled: false }, |_| {
            Ok::<_, ()>(Some(graph.clone()))
        })
        .unwrap();
        assert_eq!(newest.number, NonZeroU32::new(2));
        let settled = resolve(&node, Want::Attempt { settled: true }, |_| {
            Ok::<_, ()>(Some(graph.clone()))
        })
        .unwrap();
        assert_eq!(settled.number, NonZeroU32::new(1));
        let work = resolve(&node, Want::Work, |_| Ok::<_, ()>(Some(graph.clone()))).unwrap();
        assert_eq!(work.number, None);
    }

    #[test]
    fn a_bare_name_is_one_attempt_only_in_a_one_node_graph() {
        let one = checkpoint("fix", &[("task", &["terminal"])]);
        let bare = Target::parse("fix").unwrap();
        let resolved = resolve(&bare, Want::Attempt { settled: true }, |_| {
            Ok::<_, ()>(Some(one))
        })
        .unwrap();
        assert_eq!(resolved.node.as_deref(), Some("task"));
        let two = checkpoint("fix", &[("plan", &["terminal"]), ("build", &[])]);
        match resolve(&bare, Want::Attempt { settled: true }, |_| {
            Ok::<_, ()>(Some(two.clone()))
        }) {
            Err(ResolveError::Ambiguous { candidates, .. }) => {
                assert_eq!(candidates, ["fix/plan", "fix/build"]);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
        assert_eq!(
            resolve(&bare, Want::Work, |_| Ok::<_, ()>(Some(two)))
                .unwrap()
                .node,
            None
        );
    }

    #[test]
    fn missing_graphs_nodes_and_attempts_are_not_found() {
        let graph = checkpoint("fix", &[("review", &["terminal"])]);
        for spelling in ["gone", "fix/absent", "fix/review#2"] {
            let target = Target::parse(spelling).unwrap();
            let result = resolve(&target, Want::Work, |name| {
                Ok::<_, ()>((name == "fix").then(|| graph.clone()))
            });
            assert!(
                matches!(result, Err(ResolveError::NotFound(_))),
                "{spelling}"
            );
        }
    }
}
