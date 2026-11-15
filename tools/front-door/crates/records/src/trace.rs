use provenant_core::{ActorId, AttemptId, HostId, OperationId, ProjectId};
use serde::{Deserialize, Serialize};

/// The identifiers that place one attempt fact or event. Each field
/// reuses an existing ID; a field unknown or inapplicable where the record is
/// written stays `None` and is never guessed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Local observations captured when the attempt was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_context: Option<crate::telemetry_context::TelemetryContext>,
    pub host: Option<HostId>,
    pub project: Option<ProjectId>,
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
    fn typed_context_round_trips_and_rejects_malformed_values()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::telemetry_context::TelemetryContext;
        for context in [None, Some(TelemetryContext::default())] {
            let trace = Trace {
                telemetry_context: context,
                ..Trace::default()
            };
            let wire = serde_json::to_value(&trace)?;
            assert_eq!(serde_json::from_value::<Trace>(wire.clone())?, trace);
            let mut malformed = wire;
            malformed["telemetry_context"] = serde_json::json!("invalid context");
            assert!(serde_json::from_value::<Trace>(malformed).is_err());
        }
        // Derive the nested-field cases from the declared context's serializer.
        let context = serde_json::to_value(TelemetryContext::default())?;
        for field in context
            .as_object()
            .ok_or("context must be an object")?
            .keys()
        {
            let malformed =
                serde_json::json!({"telemetry_context": {(field): "invalid observation"}});
            assert!(
                serde_json::from_value::<Trace>(malformed).is_err(),
                "{field}"
            );
        }
        Ok(())
    }

    #[test]
    fn unknown_fields_stay_absent_and_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let trace = Trace {
            attempt: Some(AttemptId::from_bytes([1; 16])),
            ..Trace::default()
        };
        let value = serde_json::to_value(&trace)?;
        assert_eq!(value["host"], serde_json::Value::Null);
        assert_eq!(value["attempt"], "01010101010101010101010101010101");
        assert_eq!(serde_json::from_value::<Trace>(value)?, trace);
        assert_eq!(serde_json::from_str::<Trace>("{}")?, Trace::default());
        Ok(())
    }
}
