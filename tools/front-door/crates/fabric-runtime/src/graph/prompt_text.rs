//! Normalise graph input text before it participates in planning or digests.
//!
//! Storage only moves these bytes; it must never rewrite digested content.
use super::*;

/// Redact known credential shapes before graph text is planned or persisted.
pub fn normalise_prompt_text(text: &str) -> Result<String, &'static str> {
    #[cfg(test)]
    NORMALISATION_TRACE.with_borrow_mut(|trace| {
        if let Some(trace) = trace {
            trace.push(text.to_owned());
        }
    });
    let safe = provenant_confinement::redact::known_shapes(text.as_bytes())
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
    let normal = normalise_prompt_text(original).unwrap();
    assert_ne!(normal, original, "fixture must exercise normalisation");
    assert!(
        !trace.contains(&normal),
        "already normalised text re-entered ingress"
    );
}

/// A schema position, never inferred from user-defined keys or object shape.
#[derive(Clone, Copy)]
pub(super) enum Context {
    Content,
    Identity,
    Document,
    Patch,
    Node,
    Route,
    RequestedRoute,
    Source,
    TemplateMap,
    Template,
    Plan,
    Composite,
    Loop,
    Input,
    Land,
    Condition,
    CiTarget,
    Outputs,
    Result,
    Findings,
    Finding,
    RepairItem,
    CiObservation,
}

impl Context {
    fn child(self, key: &str) -> Self {
        use Context::*;
        match (self, key) {
            (
                Document,
                "graph_id" | "review_base" | "integration_branch" | "replay_digest"
                | "decision_session",
            ) => Identity,
            (Document, "nodes")
            | (Patch, "nodes" | "replace")
            | (Template, "node" | "nodes")
            | (Composite, "nodes")
            | (Loop, "body") => Node,
            (Document, "submission_patch") => Patch,
            (Document, "template_library") | (Patch, "templates") => TemplateMap,
            (TemplateMap, _) => Template,
            (Template, "name" | "tier") => Identity,
            (Template, "exports")
            | (Node, "inputs")
            | (Composite, "exports")
            | (Plan, "fallback_inputs") => Input,
            (Patch, "idempotency_key" | "skip") => Identity,
            (
                Node,
                "id" | "template" | "use" | "depends_on" | "after" | "after_settled"
                | "after_expansion" | "continues" | "dispositions" | "parent_node" | "worktree"
                | "review_base" | "review_lens" | "repair_lens",
            ) => Identity,
            (Node, "route" | "routes") => Route,
            (Node, "source") => Source,
            (Node, "composite") | (Plan, "fallback") => Composite,
            (Node, "loop" | "repeat") => Loop,
            (Node, "plan") => Plan,
            (Node, "land") => Land,
            (Node, "condition") => Condition,
            (Plan, "patterns") => Template,
            (Plan, "fallback_depends_on") | (Loop, "until" | "decision_session") => Identity,
            (
                Route,
                "adapter" | "name" | "model" | "alias" | "effort" | "account" | "tier"
                | "required_family" | "peer_source_head",
            ) => Identity,
            (Route, "peer_requested_route") => RequestedRoute,
            (
                RequestedRoute,
                "kind" | "required_family" | "provider" | "name" | "model" | "effort"
                | "excluded_families" | "preferred_families",
            ) => Identity,
            (Source, "type" | "commit" | "node") => Identity,
            (Input, "node") | (Land, "node" | "session") | (CiTarget, "node") => Identity,
            (Condition, "target") => CiTarget,
            (Outputs, "findings") => Findings,
            (Findings, _) => Finding,
            (Outputs, "repair_items") => RepairItem,
            (Outputs, "result") => Result,
            (Outputs, "branch" | "commit") | (Result, "path" | "digest") => Identity,
            (Finding, "id" | "file" | "path" | "lens" | "propagates")
            | (RepairItem, "id")
            | (CiObservation, "head_sha") => Identity,
            // Everything else, including flattened extension values, is content.
            // A content subtree can never re-enter a schema context.
            _ => Content,
        }
    }
}

/// Identity bytes and object keys are immutable, so refuse credentials rather
/// than silently changing references. Check the entire fresh structured block,
/// including fields that a later typed projection may discard.
pub(super) fn validate_identities(
    value: &serde_json::Value,
    context: Context,
) -> Result<(), &'static str> {
    let scan = |text: &str| {
        provenant_confinement::disclosure::scan_snapshot(text.as_bytes(), &[])
            .map_err(|_| "credential in structured output identity or object key")
    };
    match value {
        serde_json::Value::String(text)
            if matches!(context, Context::Identity | Context::CiTarget) =>
        {
            scan(text)?
        }
        serde_json::Value::Array(values) => {
            let child = if matches!(context, Context::Findings) {
                Context::Finding
            } else {
                context
            };
            for value in values {
                validate_identities(value, child)?;
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                scan(key)?;
                validate_identities(value, context.child(key))?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn normalise<T: Serialize + serde::de::DeserializeOwned>(
    input: &T,
    context: Context,
) -> Result<T, &'static str> {
    fn visit(value: &mut serde_json::Value, context: Context) -> Result<(), &'static str> {
        match value {
            serde_json::Value::String(value)
                if !matches!(context, Context::Identity | Context::CiTarget) =>
            {
                *value = normalise_prompt_text(value)?;
            }
            serde_json::Value::Array(values) => {
                let child = if matches!(context, Context::Findings) {
                    Context::Finding
                } else {
                    context
                };
                for value in values {
                    visit(value, child)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    visit(value, context.child(key))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut value = serde_json::to_value(input).map_err(|_| "cannot encode graph text")?;
    validate_identities(&value, context)?;
    visit(&mut value, context)?;
    let text = serde_json::to_string(&value).map_err(|_| "cannot encode graph text")?;
    provenant_confinement::disclosure::scan_snapshot(text.as_bytes(), &[])
        .map_err(|_| "graph text matches a secret pattern")?;
    serde_json::from_value(value).map_err(|_| "cannot decode normalised graph text")
}

/// The typed handoff is a separate persisted projection from output summaries.
/// Normalise it while the capture is fresh, before any consumer retains it.
pub(super) fn captured_handoff(
    capture: &mut crate::result_capture::CapturedResult,
) -> Result<(), &'static str> {
    match &mut capture.handoff {
        Ok(handoff) => *handoff = normalise(handoff, Context::Content)?,
        Err(error) => error.retained = normalise(&error.retained, Context::Content)?,
    }
    Ok(())
}

/// Findings have already been normalised while parsing, before merging stored
/// upstream evidence. Only the fresh output projection enters the walker here.
pub(super) fn captured_outputs(mut outputs: NodeOutputs) -> Result<NodeOutputs, &'static str> {
    let findings = std::mem::take(&mut outputs.findings);
    let mut outputs = normalise(&outputs, Context::Outputs)?;
    outputs.findings = findings;
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn prompt_normalisation_preserves_control_identities() {
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
        let safe: serde_json::Value = normalise(&value, Context::Document).unwrap();
        for pointer in [
            "/graph_id",
            "/nodes/0/id",
            "/nodes/0/depends_on/0",
            "/nodes/0/route/alias",
            "/nodes/0/source/node",
        ] {
            assert_eq!(safe.pointer(pointer).unwrap(), identity);
        }
        assert!(!serde_json::to_string(&safe).unwrap().contains(secret));
        assert_eq!(safe, normalise(&safe, Context::Document).unwrap());
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn captured_findings_and_handoff_use_the_same_normal_form() {
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
            !input
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
            !safe.repair_items.as_ref().unwrap()[0].extra["comment"]
                .as_str()
                .unwrap()
                .contains(secret)
        );
        assert_eq!(safe.result.as_ref().unwrap().digest, input.digest);
        assert!(!safe.verdict.as_deref().unwrap().contains(secret));
        assert!(!safe.result.as_ref().unwrap().excerpt.contains(secret));
        assert_eq!(safe.findings[0].id, "F1");
        assert_eq!(safe.findings[0].file, "src/lib.rs");
        assert_eq!(safe.findings[0].propagates.as_deref(), Some("upstream:F1"));
        assert!(!safe.findings[0].summary.contains(secret));
    }
    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn captured_credentials_in_identities_or_keys_refuse_the_whole_block() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let base = serde_json::json!({
            "schema_version": 1,
            "findings": [{"id":"F1", "file":"src/lib.rs", "severity":"high",
                "summary":"valid finding", "status":"open", "propagates":"upstream:F1"}],
            "repair_items": [{"id":"F1", "status":"fixed", "reason":"fixed it"}]
        });
        let mut cases = Vec::new();
        for pointer in [
            "/findings/0/id",
            "/findings/0/file",
            "/findings/0/propagates",
            "/repair_items/0/id",
        ] {
            let mut value = base.clone();
            *value.pointer_mut(pointer).unwrap() = secret.into();
            cases.push(value);
        }
        let mut keyed = base.clone();
        keyed["findings"] = serde_json::json!({secret: base["findings"][0].clone()});
        cases.push(keyed);
        let mut extension = base.clone();
        extension["unknown"] = serde_json::json!({"nested": {secret: "extension key"}});
        cases.push(extension);
        let mut keyed_identity = base.clone();
        keyed_identity["findings"] = serde_json::json!({"F1": base["findings"][0].clone()});
        keyed_identity["findings"]["F1"]["file"] = secret.into();
        cases.push(keyed_identity);
        for value in cases {
            let original = value.clone();
            let mut input = crate::result_capture::ResultCapture::new(None);
            input.push(format!("VERDICT: PASS\n```findings\n{value}\n```\n").as_bytes());
            let input = input.finish();
            let outputs =
                captured_outputs(outputs::make_outputs(Some(&input), None, None, None, None))
                    .unwrap();
            assert_eq!(outputs.findings_available, Some(false));
            assert!(outputs.findings.is_empty());
            assert!(outputs.repair_items.is_none());
            assert!(
                outputs
                    .warnings
                    .iter()
                    .any(|warning| warning.starts_with(outputs::FINDINGS_INVALID))
            );
            assert!(!serde_json::to_string(&outputs).unwrap().contains(secret));
            assert_eq!(value, original, "refusal must not rewrite identities");
        }
    }
}
