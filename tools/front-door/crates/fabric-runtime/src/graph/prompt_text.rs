//! Normalise graph input text before it participates in planning or digests.
//!
//! Storage only moves these bytes; it must never rewrite digested content.
use super::*;

/// Mask typed and ambient credential values before graph text is persisted.
pub fn normalise_prompt_text(text: &str) -> Result<String, &'static str> {
    #[cfg(test)]
    NORMALISATION_TRACE.with_borrow_mut(|trace| {
        if let Some(trace) = trace {
            trace.push(text.to_owned());
        }
    });
    let safe = provenant_confinement::redact::publication_text(text.as_bytes(), [])
        .map_err(|_| "graph text redaction failed")?;
    String::from_utf8(safe).map_err(|_| "graph text redaction produced invalid UTF-8")
}

#[cfg(test)]
thread_local! {
    static NORMALISATION_TRACE: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Observe calls at ingress, so regressions cannot hide a second pass behind
/// the redactor's idempotence. The guard also clears the trace on test panic.
#[cfg(test)]
pub(super) fn trace<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            NORMALISATION_TRACE.with_borrow_mut(|trace| *trace = None);
        }
    }
    NORMALISATION_TRACE.with_borrow_mut(|trace| {
        assert!(trace.is_none());
        *trace = Some(Vec::new());
    });
    let _guard = Guard;
    let result = run();
    let trace = NORMALISATION_TRACE.with_borrow_mut(|trace| trace.take().unwrap());
    (result, trace)
}

#[cfg(test)]
pub(super) fn assert_once(trace: &[String], original: &str) {
    assert_eq!(trace.iter().filter(|text| *text == original).count(), 1);
    assert_eq!(normalise_prompt_text(original).unwrap(), original);
}

pub(super) fn normalise<T: Serialize + serde::de::DeserializeOwned>(
    input: &T,
) -> Result<T, &'static str> {
    fn visit(value: &mut serde_json::Value) -> Result<(), &'static str> {
        match value {
            serde_json::Value::String(value) => *value = normalise_prompt_text(value)?,
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            serde_json::Value::Object(values) => {
                let entries = std::mem::take(values);
                for (key, mut value) in entries {
                    visit(&mut value)?;
                    let key = normalise_prompt_text(&key)?;
                    if values.insert(key, value).is_some() {
                        return Err("credential masking collapsed object keys");
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut value = serde_json::to_value(input).map_err(|_| "cannot encode graph text")?;
    visit(&mut value)?;
    serde_json::from_value(value).map_err(|_| "cannot decode normalised graph text")
}

/// The typed handoff is a separate persisted projection from output summaries.
/// Normalise it while the capture is fresh, before any consumer retains it.
pub(super) fn captured_handoff(
    capture: &mut crate::result_capture::CapturedResult,
) -> Result<(), &'static str> {
    match &mut capture.handoff {
        Ok(handoff) => *handoff = normalise(handoff)?,
        Err(error) => error.retained = normalise(&error.retained)?,
    }
    Ok(())
}

/// Findings have already been normalised while parsing, before merging stored
/// upstream evidence. Only the fresh output projection enters the walker here.
pub(super) fn captured_outputs(mut outputs: NodeOutputs) -> Result<NodeOutputs, &'static str> {
    let findings = std::mem::take(&mut outputs.findings);
    let mut outputs = normalise(&outputs)?;
    outputs.findings = findings;
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn prompt_normalisation_preserves_secret_shaped_examples_and_references() {
        let identity = "control-id";
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let value = serde_json::json!({
            "graph_id": identity,
            "nodes": [{"id": identity, "prompt": secret,
                "depends_on": [identity], "route": {"alias": identity, "comment": {"source": secret}},
                "source": {"type": "node", "node": identity, "comment": secret},
                "comment": {"nodes": [{"id": secret}], "route": {"alias": secret}}}],
            "provenance": {"parameters": {"source": secret,
                "nested": {"file": secret, "findings": [{"id": secret}]}}},
            "with": {"source": secret, "path": [secret], "template": secret},
            "template_library": {"test": {"doc": secret, "parameters": {"value": {"type": "string", "doc": secret}}}}
        });
        let safe: serde_json::Value = normalise(&value).unwrap();
        for pointer in [
            "/graph_id",
            "/nodes/0/id",
            "/nodes/0/depends_on/0",
            "/nodes/0/route/alias",
            "/nodes/0/source/node",
        ] {
            assert_eq!(safe.pointer(pointer).unwrap(), identity);
        }
        assert_eq!(safe.pointer("/nodes/0/prompt").unwrap(), secret);
        assert_eq!(safe.pointer("/nodes/0/comment/nodes/0/id").unwrap(), secret);
        assert_eq!(
            safe.pointer("/provenance/parameters/source").unwrap(),
            secret
        );
        assert!(serde_json::to_string(&safe).unwrap().contains(secret));
        assert_eq!(safe, normalise(&safe).unwrap());
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn captured_secret_shaped_examples_are_kept_in_findings_and_handoff() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let summary = format!("finding summary {secret}");
        let comment = format!("repair extension {secret}");
        let mut input = crate::result_capture::ResultCapture::new(None);
        input.push(format!("VERDICT: {secret}\nResult {secret}\n```findings\n{{\"schema_version\":1,\"findings\":[{{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"src/lib.rs\",\"propagates\":\"upstream:F1\",\"summary\":\"{summary}\",\"status\":\"open\"}}],\"repair_items\":[{{\"id\":\"F1\",\"status\":\"fixed\",\"reason\":\"fixed it\",\"comment\":\"{comment}\"}}]}}\n```\n").as_bytes());
        let mut input = input.finish();
        assert_eq!(
            input.verdict.as_deref(),
            Some(format!("VERDICT: {secret}").as_str())
        );
        input.verdict_cause = Some(format!("untrusted verdict diagnostic {secret}"));
        let note = format!("captured handoff note {secret}");
        input.handoff = Ok(Some(
            serde_json::from_value(serde_json::json!({
                "schema_version": 2, "status": "done", "note": note,
                "remaining": [], "decisions": [secret], "verify": []
            }))
            .unwrap(),
        ));
        let (safe, calls) = trace(|| {
            let outputs = outputs::make_outputs(Some(&input), None, None, None, None);
            let outputs = captured_outputs(outputs).unwrap();
            captured_handoff(&mut input).unwrap();
            outputs
        });
        assert_once(&calls, &note);
        assert!(
            input
                .handoff
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .note
                .contains(secret)
        );
        assert_once(&calls, &summary);
        assert_once(&calls, &comment);
        assert_eq!(safe.repair_items.as_ref().unwrap()[0].id, "F1");
        assert!(
            safe.repair_items.as_ref().unwrap()[0].extra["comment"]
                .as_str()
                .unwrap()
                .contains(secret)
        );
        assert_eq!(safe.result.as_ref().unwrap().digest, input.digest);
        assert!(safe.verdict.as_deref().unwrap().contains(secret));
        assert!(safe.result.as_ref().unwrap().excerpt.contains(secret));
        assert_eq!(safe.findings[0].id, "F1");
        assert_eq!(safe.findings[0].file, "src/lib.rs");
        assert_eq!(safe.findings[0].propagates.as_deref(), Some("upstream:F1"));
        assert!(safe.findings[0].summary.contains(secret));
    }
    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn secret_shaped_identity_and_object_keys_are_preserved_consistently() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let input = serde_json::json!({
            "graph_id": secret,
            "nodes": [{"id":secret, "depends_on":[secret], "prompt":"example " .to_owned() + secret}],
            "metadata": {secret: "secret shaped example"}
        });
        let safe: serde_json::Value = normalise(&input).unwrap();
        assert_eq!(safe["graph_id"], secret);
        assert_eq!(safe["nodes"][0]["id"], secret);
        assert_eq!(safe["nodes"][0]["depends_on"][0], secret);
        assert!(
            safe["nodes"][0]["prompt"]
                .as_str()
                .unwrap()
                .contains(secret)
        );
        assert_eq!(safe["metadata"][secret], "secret shaped example");
    }
}
