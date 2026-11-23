use super::*;

// Spec: docs/specs/routing.md#exact-selection-no-inference
#[test]
fn an_unknown_effort_refuses_over_the_models_declared_variants()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("claude/claude-haiku-5-5")?;
    assert!(catalogue.variant_refusal(&model, "high").is_none());
    let refusal = catalogue
        .variant_refusal(&model, "hig")
        .ok_or("a variant the model does not declare must refuse")?;
    assert_eq!(refusal.kind, provenant_core::NameKind("variant"));
    assert_eq!(refusal.query, "hig");
    assert!(refusal.candidates.contains(&"high".to_owned()));
    let message = refusal.to_string();
    assert!(message.contains("unknown variant `hig`"), "{message}");
    assert!(message.contains("close matches: high"), "{message}");
    Ok(())
}
