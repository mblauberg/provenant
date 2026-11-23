use super::*;

#[test]
fn go_allowance_floor_handles_limits_unknown_and_zen() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config: RoutingConfig = toml_edit::de::from_str("[opencode_go]\n")?;
    assert_eq!(config.opencode_go.min_monthly_allowance_usd, 60);
    for id in [
        "opencode/opencode-go/deepseek-v4.1-flash",
        "opencode/opencode-go/glm-5.3-flash",
        "opencode/opencode/deepseek-v4.1-flash-free",
        "opencode/opencode/glm-5.3-flash-free",
    ] {
        assert!(
            check_go_allowance(&ModelId::parse(id)?, &config, &catalogue).is_ok(),
            "{id}"
        );
    }
    let model = ModelId::parse("opencode/opencode-go/unlisted")?;
    assert!(check_go_allowance(&model, &config, &catalogue).is_err());
    config.opencode_go.min_monthly_allowance_usd = 0;
    assert!(check_go_allowance(&model, &config, &catalogue).is_ok());
    config.opencode_go.min_monthly_allowance_usd = 61;
    assert!(
        check_go_allowance(
            &ModelId::parse("opencode/opencode-go/glm-5.3-flash")?,
            &config,
            &catalogue,
        )
        .is_err()
    );
    config.opencode_go.min_monthly_allowance_usd = 60;
    Ok(())
}
