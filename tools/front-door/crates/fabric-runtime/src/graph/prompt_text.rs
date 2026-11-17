//! Normalise graph input text before it participates in planning or digests.
//!
//! Storage only moves these bytes; it must never rewrite digested content.
use super::*;

pub(super) fn text(text: &str) -> Result<String, &'static str> {
    let safe = provenant_confinement::redact::known_shapes(text.as_bytes())
        .map_err(|_| "graph text redaction failed")?;
    String::from_utf8(safe).map_err(|_| "graph text redaction produced invalid UTF-8")
}

/// Normalise prompt-bearing fields, including nested template arguments and
/// findings. Identifiers, paths, routes and digests retain their exact identity.
pub(super) fn normalise<T: Serialize + serde::de::DeserializeOwned>(
    input: &T,
) -> Result<T, &'static str> {
    fn visit(value: &mut serde_json::Value, content: bool) -> Result<(), &'static str> {
        match value {
            serde_json::Value::String(value) if content => *value = text(value)?,
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, content)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
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
                    );
                    if identity {
                        continue;
                    }
                    let content = content
                        || matches!(
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
                                | "findings"
                                | "repair_items"
                                | "spec_concerns"
                                | "failed_checks"
                                | "summary"
                                | "description"
                                | "detail"
                                | "reason"
                                | "question"
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
                        )
                        || value.as_str().is_some_and(|text| text.len() > 4096);
                    visit(value, content)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut value = serde_json::to_value(input).map_err(|_| "cannot encode graph text")?;
    visit(&mut value, false)?;
    serde_json::from_value(value).map_err(|_| "cannot decode normalised graph text")
}

pub(super) fn capture(
    input: &crate::result_capture::CapturedResult,
) -> Result<crate::result_capture::CapturedResult, &'static str> {
    let mut capture = input.clone();
    capture.excerpt = text(&capture.excerpt)?;
    capture.tail = text(&capture.tail)?;
    capture.findings = capture.findings.as_deref().map(text).transpose()?;
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
            "findings": [{"id": identity, "file": identity, "summary": identity}]
        });
        let safe: serde_json::Value = normalise(&value).unwrap();
        assert_eq!(safe["graph_id"], identity);
        assert_eq!(safe["nodes"][0]["id"], identity);
        assert_eq!(safe["nodes"][0]["depends_on"][0], identity);
        assert_eq!(safe["nodes"][0]["route"]["alias"], identity);
        assert_eq!(safe["findings"][0]["id"], identity);
        assert_eq!(safe["findings"][0]["file"], identity);
        assert_ne!(safe["nodes"][0]["prompt"], identity);
        assert_ne!(safe["findings"][0]["summary"], identity);
        assert_eq!(safe, normalise(&safe).unwrap());
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn captured_findings_and_handoff_use_the_same_normal_form() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let mut input = crate::result_capture::ResultCapture::new(None);
        input.push(format!("Result {secret}\n```findings\n{{\"schema_version\":1,\"findings\":[{{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"file.rs\",\"summary\":\"{secret}\",\"status\":\"open\"}}]}}\n```\n").as_bytes());
        let mut input = input.finish();
        input.handoff = Ok(Some(
            serde_json::from_value(serde_json::json!({
                "schema_version": 2, "status": "done", "note": secret,
                "remaining": [], "decisions": [secret], "verify": []
            }))
            .unwrap(),
        ));
        let safe = capture(&input).unwrap();
        assert_eq!(safe.digest, input.digest);
        assert!(
            !serde_json::to_string(safe.handoff.as_ref().unwrap())
                .unwrap()
                .contains(secret)
        );
        assert!(!safe.excerpt.contains(secret));
        assert!(!safe.tail.contains(secret));
        assert!(!safe.findings.as_deref().unwrap().contains(secret));
        let outputs = outputs::make_outputs(Some(&safe), None, None, None, None);
        assert!(!serde_json::to_string(&outputs).unwrap().contains(secret));
        assert_eq!(safe, capture(&safe).unwrap());
    }
}
