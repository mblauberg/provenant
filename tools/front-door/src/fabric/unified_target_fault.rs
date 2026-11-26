//! Target resolution faults and their command-facing representation.
use super::{FacadeError, JournalError, ResolveError};

/// A target that names nothing, or names it ambiguously, as its envelope
/// reports it. It carries its own outcome so the caller does not re-derive it.
#[derive(Debug)]
pub(super) struct TargetFault {
    pub outcome: provenant_records::Outcome,
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for TargetFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TargetFault {}

fn close(matches: &[String]) -> String {
    if matches.is_empty() {
        String::new()
    } else {
        format!("; close matches: {}", matches.join(", "))
    }
}

pub(super) fn target_fault(error: ResolveError<JournalError>) -> TargetFault {
    let (outcome, code) = match &error {
        ResolveError::NotFound { .. } => (provenant_records::Outcome::Invalid, "NOT_FOUND"),
        ResolveError::Ambiguous { .. } => (provenant_records::Outcome::Invalid, "AMBIGUOUS_TARGET"),
        ResolveError::Read(_) => (provenant_records::Outcome::Error, "GRAPH_READ"),
    };
    let message = match &error {
        ResolveError::NotFound { target, candidates } => {
            let mut message = target.clone();
            message.push_str(&close(candidates));
            message
        }
        ResolveError::Ambiguous { target, candidates } => format!(
            "{target} has several nodes; use one of {}",
            candidates.join(", ")
        ),
        ResolveError::Read(_) => error.to_string(),
    };
    TargetFault {
        outcome,
        code,
        message,
    }
}

pub(super) fn outside_project_fault(target: &provenant_cli::Target) -> TargetFault {
    TargetFault {
        outcome: provenant_records::Outcome::Invalid,
        code: "NOT_FOUND",
        message: format!(
            "no project found for {target}; run from a project checkout or pass --project PATH"
        ),
    }
}

pub(super) fn is_target_not_found(error: &(dyn std::error::Error + 'static)) -> bool {
    if matches!(
        error.downcast_ref::<FacadeError>(),
        Some(FacadeError::NotFound)
    ) {
        return true;
    }
    if matches!(
        error.downcast_ref::<FacadeError>(),
        Some(FacadeError::GraphRelayRefused { refusal_code, .. })
            if refusal_code == "NOT_FOUND"
    ) {
        return true;
    }
    if let Some(service) = error.downcast_ref::<provenant_fabric_runtime::relay::ServiceError>() {
        return match service {
            provenant_fabric_runtime::relay::ServiceError::Facade(error) => {
                matches!(error, FacadeError::NotFound)
                    || matches!(
                        error,
                        FacadeError::GraphRelayRefused { refusal_code, .. }
                            if refusal_code == "NOT_FOUND"
                    )
            }
            provenant_fabric_runtime::relay::ServiceError::Mutation { source, .. } => {
                is_target_not_found(source.as_ref())
            }
            provenant_fabric_runtime::relay::ServiceError::Cli { code, .. } => code == "NOT_FOUND",
            _ => false,
        };
    }
    false
}

pub(super) fn command_target_fault(
    error: &(dyn std::error::Error + 'static),
    target: Option<&str>,
) -> Option<(provenant_records::Outcome, &'static str, String)> {
    if let Some(fault) = error.downcast_ref::<TargetFault>() {
        return Some((fault.outcome, fault.code, fault.message.clone()));
    }
    is_target_not_found(error).then(|| {
        let message = target.map_or_else(
            || {
                error
                    .to_string()
                    .trim_start_matches("NOT_FOUND: ")
                    .to_owned()
            },
            |target| format!("{target} was not found in this project"),
        );
        (provenant_records::Outcome::Invalid, "NOT_FOUND", message)
    })
}
