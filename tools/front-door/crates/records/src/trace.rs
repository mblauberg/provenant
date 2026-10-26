use provenant_core::{ActorId, AttemptId, HostId, OperationId, ProjectId, RunId};
use serde::{Deserialize, Serialize};

/// The identifiers that place one attempt fact or event in its run. Each field
/// reuses an existing ID; a field unknown or inapplicable where the record is
/// written stays `None` and is never guessed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    pub host: Option<HostId>,
    pub project: Option<ProjectId>,
    pub run: Option<RunId>,
    pub attempt: Option<AttemptId>,
    pub operation: Option<OperationId>,
    pub actor: Option<ActorId>,
    /// The attempt this one descends from: the continued attempt, or else the
    /// attempt that submitted this nested run.
    pub parent: Option<AttemptId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_stay_absent_and_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let trace = Trace {
            run: Some(RunId::from_bytes([1; 16])),
            ..Trace::default()
        };
        let value = serde_json::to_value(&trace)?;
        assert_eq!(value["host"], serde_json::Value::Null);
        assert_eq!(value["run"], "01010101010101010101010101010101");
        assert_eq!(serde_json::from_value::<Trace>(value)?, trace);
        assert_eq!(serde_json::from_str::<Trace>("{}")?, Trace::default());
        Ok(())
    }
}
