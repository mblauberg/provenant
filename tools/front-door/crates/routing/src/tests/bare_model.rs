use super::*;

#[test]
fn bare_name_resolves_to_the_one_provider_that_offers_it() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    assert_eq!(
        catalogue.resolve_bare_model("claude-sonnet-5-5")?.as_str(),
        "claude/claude-sonnet-5-5"
    );
    assert_eq!(
        catalogue.resolve_bare_model("gpt-6.1-terra")?.as_str(),
        "codex/gpt-6.1-terra"
    );
    Ok(())
}

#[test]
fn bare_name_offered_by_several_providers_names_each_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let mut row = catalogue
        .models
        .iter()
        .find(|row| row.model.as_str() == "codex/gpt-6.1-terra")
        .cloned()
        .ok_or("missing codex row")?;
    row.model = ModelId::parse("cursor/gpt-6.1-terra")?;
    catalogue.models.push(row);
    assert_eq!(
        catalogue.resolve_bare_model("gpt-6.1-terra"),
        Err(BareModelError::Ambiguous {
            candidates: vec!["codex/gpt-6.1-terra".into(), "cursor/gpt-6.1-terra".into()],
        })
    );
    Ok(())
}

#[test]
fn bare_name_no_provider_offers_is_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    assert_eq!(
        catalogue.resolve_bare_model("no-such-model-9"),
        Err(BareModelError::Unknown)
    );
    // A provider-qualified name never resolves as a bare name.
    assert_eq!(
        catalogue.resolve_bare_model("claude/claude-sonnet-5-5"),
        Err(BareModelError::Unknown)
    );
    Ok(())
}

#[test]
fn bundled_senior_pool_keeps_terra_below_high_effort() -> Result<(), Box<dyn std::error::Error>> {
    // Terra at high effort or above stopped writers early too often, so the
    // pool that assigns writer work offers it only at low or medium effort.
    let catalogue = Catalogue::bundled()?;
    let senior = &catalogue.pools["senior"];
    for member in senior
        .iter()
        .filter(|member| member.model.as_str() == "codex/gpt-6.1-terra")
    {
        assert_eq!(member.efforts, ["low", "medium"]);
        assert_eq!(member.effort.as_deref(), Some("medium"));
    }
    Ok(())
}
