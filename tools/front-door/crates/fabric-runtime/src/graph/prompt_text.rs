//! Normalise graph input text before it participates in planning or digests.
//!
//! Storage only moves these bytes; it must never rewrite digested content.
use super::*;

/// Redact known credential shapes before graph text is planned or persisted.
pub fn normalise_prompt_text(text: &str) -> Result<String, &'static str> {
    let safe = provenant_confinement::redact::known_shapes(text.as_bytes())
        .map_err(|_| "graph text redaction failed")?;
    String::from_utf8(safe).map_err(|_| "graph text redaction produced invalid UTF-8")
}

/// Normalise prompt-bearing fields, including nested template arguments and
/// findings. Identifiers, paths, routes and digests retain their exact identity.
pub(super) fn normalise<T: Serialize + serde::de::DeserializeOwned>(
    input: &T,
) -> Result<T, &'static str> {
    #[derive(Clone, Copy)]
    enum Context {
        Schema,
        Content,
        Finding,
    }

    fn visit(value: &mut serde_json::Value, context: Context) -> Result<(), &'static str> {
        match value {
            serde_json::Value::String(value) if !matches!(context, Context::Schema) => {
                *value = normalise_prompt_text(value)?
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, context)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    // Once inside prose or a user-defined parameter map, every
                    // descendant is content, regardless of its key or shape.
                    if matches!(context, Context::Content) {
                        visit(value, Context::Content)?;
                        continue;
                    }
                    // Finding records carry schema identities alongside prose.
                    // This context is entered only at the schema findings field,
                    // never at a similarly named key inside parameter content.
                    if matches!(context, Context::Finding) {
                        if !matches!(key.as_str(), "id" | "file" | "path" | "lens" | "propagates") {
                            visit(value, Context::Content)?;
                        }
                        continue;
                    }
                    let identity = matches!(
                        key.as_str(),
                        "id" | "graph_id"
                            | "node"
                            | "source"
                            | "reference"
                            | "file"
                            | "path"
                            | "branch"
                            | "commit"
                            | "digest"
                            | "route"
                            | "routes"
                            | "worktree"
                            | "depends_on"
                            | "after"
                            | "after_settled"
                            | "after_expansion"
                            | "lens"
                            | "template"
                            | "use"
                            | "propagates"
                    );
                    if identity {
                        continue;
                    }
                    if key == "findings" {
                        visit(value, Context::Finding)?;
                        continue;
                    }
                    let content = matches!(
                        key.as_str(),
                        "prompt"
                            | "brief"
                            | "spec"
                            | "fragment"
                            | "excerpt"
                            | "_artifact_findings"
                            | "_compact_disposition_findings"
                            | "acceptance_checks"
                            | "parameters"
                            | "with"
                            | "spec_concerns"
                            | "failed_checks"
                            | "summary"
                            | "description"
                            | "detail"
                            | "reason"
                            | "question"
                            | "expected"
                            | "required_verdict"
                            | "verdict_error"
                            | "note"
                            | "remaining"
                            | "decisions"
                            | "verify"
                            | "warnings"
                            | "criterion"
                            | "claim"
                            | "evidence"
                            | "proposed_change"
                            | "alternative"
                    ) || value.as_str().is_some_and(|text| text.len() > 4096);
                    visit(
                        value,
                        if content {
                            Context::Content
                        } else {
                            Context::Schema
                        },
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut value = serde_json::to_value(input).map_err(|_| "cannot encode graph text")?;
    visit(&mut value, Context::Schema)?;
    serde_json::from_value(value).map_err(|_| "cannot decode normalised graph text")
}

pub(super) fn capture(
    input: &crate::result_capture::CapturedResult,
) -> Result<crate::result_capture::CapturedResult, &'static str> {
    let mut capture = input.clone();
    capture.excerpt = normalise_prompt_text(&capture.excerpt)?;
    capture.tail = normalise_prompt_text(&capture.tail)?;
    capture.verdict = capture
        .verdict
        .as_deref()
        .map(normalise_prompt_text)
        .transpose()?
        .map(String::into_boxed_str);
    capture.verdict_cause = capture
        .verdict_cause
        .as_deref()
        .map(normalise_prompt_text)
        .transpose()?;
    capture.handoff = match &capture.handoff {
        Ok(handoff) => Ok(normalise(handoff)?),
        Err(error) => {
            let mut error = error.clone();
            error.retained = normalise(&error.retained)?;
            Err(error)
        }
    };
    // The digest identifies the immutable result artifact, not its prompt projection.
    Ok(capture)
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
                "depends_on": [identity], "route": {"alias": identity}}],
            "findings": [{"id": identity, "file": identity, "propagates": identity, "summary": identity}],
            "provenance": {"parameters": {"source": identity,
                "nested": {"file": identity, "findings": [{"id": identity}]}}},
            "with": {"source": identity, "path": [identity], "template": identity},
            "repair_items": [{"id": identity, "reason": identity}]
        });
        let safe: serde_json::Value = normalise(&value).unwrap();
        assert_eq!(safe["graph_id"], identity);
        assert_eq!(safe["nodes"][0]["id"], identity);
        assert_eq!(safe["nodes"][0]["depends_on"][0], identity);
        assert_eq!(safe["nodes"][0]["route"]["alias"], identity);
        assert_eq!(safe["findings"][0]["id"], identity);
        assert_eq!(safe["findings"][0]["file"], identity);
        assert_eq!(safe["findings"][0]["propagates"], identity);
        assert_ne!(safe["nodes"][0]["prompt"], identity);
        assert_ne!(safe["findings"][0]["summary"], identity);
        assert_eq!(safe["repair_items"][0]["id"], identity);
        assert_ne!(safe["repair_items"][0]["reason"], identity);
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
        assert_eq!(safe, normalise(&safe).unwrap());
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn captured_findings_and_handoff_use_the_same_normal_form() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let mut input = crate::result_capture::ResultCapture::new(None);
        input.push(format!("VERDICT: {secret}\nResult {secret}\n```findings\n{{\"schema_version\":1,\"findings\":[{{\"id\":\"{secret}\",\"severity\":\"high\",\"file\":\"{secret}\",\"propagates\":\"{secret}\",\"summary\":\"{secret}\",\"status\":\"open\"}}]}}\n```\n").as_bytes());
        let mut input = input.finish();
        assert_eq!(
            input.verdict.as_deref(),
            Some(format!("VERDICT: {secret}").as_str())
        );
        input.verdict_cause = Some(format!("untrusted verdict diagnostic {secret}"));
        input.handoff = Ok(Some(
            serde_json::from_value(serde_json::json!({
                "schema_version": 2, "status": "done", "note": secret,
                "remaining": [], "decisions": [secret], "verify": []
            }))
            .unwrap(),
        ));
        let safe = capture(&input).unwrap();
        assert_eq!(safe.digest, input.digest);
        assert!(!safe.verdict.as_deref().unwrap().contains(secret));
        assert!(!safe.verdict_cause.as_deref().unwrap().contains(secret));
        assert!(
            !serde_json::to_string(safe.handoff.as_ref().unwrap())
                .unwrap()
                .contains(secret)
        );
        assert!(!safe.excerpt.contains(secret));
        assert!(!safe.tail.contains(secret));
        // Transient findings JSON is parsed first, not prematurely normalised as prose.
        let outputs = outputs::make_outputs(Some(&safe), None, None, None, None);
        let outputs = normalise(&outputs).unwrap();
        assert_eq!(outputs.findings[0].id, secret);
        assert_eq!(outputs.findings[0].file, secret);
        assert_eq!(outputs.findings[0].propagates.as_deref(), Some(secret));
        assert!(!outputs.findings[0].summary.contains(secret));
        assert_eq!(safe, capture(&safe).unwrap());
    }
}
