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
            (Outputs, "findings") => Finding,
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
                for value in values {
                    visit(value, context)?;
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
    visit(&mut value, context)?;
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
        let identity = "AKIAIOSFODNN7EXAMPLE";
        let value = serde_json::json!({
            "graph_id": identity,
            "nodes": [{"id": identity, "prompt": identity,
                "depends_on": [identity], "route": {"alias": identity, "comment": {"source": identity}},
                "source": {"type": "node", "node": identity, "comment": identity},
                "comment": {"nodes": [{"id": identity}], "route": {"alias": identity}}}],
            "findings": [{"id": identity, "file": identity, "propagates": identity, "summary": identity}],
            "provenance": {"parameters": {"source": identity,
                "nested": {"file": identity, "findings": [{"id": identity}]}}},
            "with": {"source": identity, "path": [identity], "template": identity},
            "repair_items": [{"id": identity, "reason": identity, "comment": {"id": identity}}],
            "template_library": {"test": {"doc": identity, "parameters": {"value": {"type": "string", "doc": identity}}}}
        });
        let mut safe: serde_json::Value = normalise(&value, Context::Document).unwrap();
        for (key, context) in [
            ("findings", Context::Finding),
            ("repair_items", Context::RepairItem),
        ] {
            safe[key] = normalise(&value[key], context).unwrap();
        }
        assert_eq!(safe["graph_id"], identity);
        assert_eq!(safe["nodes"][0]["id"], identity);
        assert_eq!(safe["nodes"][0]["depends_on"][0], identity);
        assert_eq!(safe["nodes"][0]["route"]["alias"], identity);
        assert_eq!(safe["nodes"][0]["source"]["node"], identity);
        for pointer in [
            "/nodes/0/route/comment/source",
            "/nodes/0/source/comment",
            "/nodes/0/comment/nodes/0/id",
            "/nodes/0/comment/route/alias",
        ] {
            assert_ne!(safe.pointer(pointer).unwrap(), identity);
        }
        assert_eq!(safe["findings"][0]["id"], identity);
        assert_eq!(safe["findings"][0]["file"], identity);
        assert_eq!(safe["findings"][0]["propagates"], identity);
        assert_ne!(safe["nodes"][0]["prompt"], identity);
        assert_ne!(safe["findings"][0]["summary"], identity);
        assert_eq!(safe["repair_items"][0]["id"], identity);
        assert_ne!(safe["repair_items"][0]["reason"], identity);
        assert_ne!(safe["repair_items"][0]["comment"]["id"], identity);
        assert_ne!(safe["template_library"]["test"]["doc"], identity);
        assert_ne!(
            safe["template_library"]["test"]["parameters"]["value"]["doc"],
            identity
        );
        for pointer in [
            "/provenance/parameters/source",
            "/provenance/parameters/nested/file",
            "/provenance/parameters/nested/findings/0/id",
            "/with/source",
            "/with/path/0",
            "/with/template",
        ] {
            assert_ne!(safe.pointer(pointer).unwrap(), identity);
        }
        assert_eq!(safe, {
            let mut again = normalise(&safe, Context::Document).unwrap();
            for (key, context) in [
                ("findings", Context::Finding),
                ("repair_items", Context::RepairItem),
            ] {
                again[key] = normalise(&safe[key], context).unwrap();
            }
            again
        });
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn captured_findings_and_handoff_use_the_same_normal_form() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let summary = format!("finding summary {secret}");
        let comment = format!("repair extension {secret}");
        let mut input = crate::result_capture::ResultCapture::new(None);
        input.push(format!("VERDICT: {secret}\nResult {secret}\n```findings\n{{\"schema_version\":1,\"findings\":[{{\"id\":\"{secret}\",\"severity\":\"high\",\"file\":\"{secret}\",\"propagates\":\"{secret}\",\"summary\":\"{summary}\",\"status\":\"open\"}}],\"repair_items\":[{{\"id\":\"{secret}\",\"status\":\"fixed\",\"reason\":\"fixed it\",\"comment\":\"{comment}\"}}]}}\n```\n").as_bytes());
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
        assert_eq!(safe.repair_items.as_ref().unwrap()[0].id, secret);
        assert!(
            !safe.repair_items.as_ref().unwrap()[0].extra["comment"]
                .as_str()
                .unwrap()
                .contains(secret)
        );
        assert_eq!(safe.result.as_ref().unwrap().digest, input.digest);
        assert!(!safe.verdict.as_deref().unwrap().contains(secret));
        assert!(!safe.result.as_ref().unwrap().excerpt.contains(secret));
        assert_eq!(safe.findings[0].id, secret);
        assert_eq!(safe.findings[0].file, secret);
        assert_eq!(safe.findings[0].propagates.as_deref(), Some(secret));
        assert!(!safe.findings[0].summary.contains(secret));
    }
}
