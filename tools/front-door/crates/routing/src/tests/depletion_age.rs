use super::*;

const NOW: i64 = 10_000;
const LUNA: &str = "codex/gpt-6-luna";

fn depletion_facts_at_age(
    age_seconds: i64,
) -> Result<Vec<ReadinessFact>, Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [{"model": LUNA, "weight": 1, "efforts": ["high"]}]}
    }))?;
    let registry = [provenant_providers::Registration {
        quota_source: None,
        ..provenant_providers::codex::REGISTRATION
    }];
    let record = AvailabilityRecord {
        account_key: Some("second".into()),
        observed_unix_seconds: NOW - age_seconds,
        expires_unix_seconds: NOW + 60,
        ..availability()
    };
    let records = [record];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let quota = [QuotaObservation {
        model: ModelId::parse(LUNA)?,
        snapshot: provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account: provenant_quota::AccountScope {
                adapter: "codex".into(),
                account_key: "second".into(),
                label: "second".into(),
            },
            revision: 1,
            status: provenant_quota::QuotaStatus::Exhausted,
            observed_unix_seconds: NOW - 10,
            source_ref: "test".into(),
            authority_ref: None,
            windows: vec![provenant_quota::QuotaWindow {
                pool: "primary".into(),
                window_seconds: Some(18_000),
                used_basis_points: 10_000,
                resets_unix_seconds: Some(NOW + 600),
            }],
            details: None,
        },
        expires_unix_seconds: NOW + 60,
    }];

    let mut context = context(
        &catalogue,
        &config,
        provenant_core::WallTime::from_unix_seconds(NOW)?,
    );
    context.registry = &registry;
    context.availability = &records;
    context.access = &access;
    context.quota = &quota;
    Ok(depletion_facts(&context, RouteIntent::default(), &[])?)
}

#[test]
fn depletion_uses_availability_through_the_one_hour_boundary_only()
-> Result<(), Box<dyn std::error::Error>> {
    let account_state = |facts: &[ReadinessFact]| {
        facts
            .iter()
            .find(|fact| {
                fact.model == ModelId::parse(LUNA).unwrap()
                    && fact.account.as_deref() == Some("second")
            })
            .map(|fact| fact.state)
    };

    // Fresh observations, including the exact one-hour boundary, remain usable.
    for age_seconds in [0, 1, 3_599, 3_600] {
        let facts = depletion_facts_at_age(age_seconds)?;
        assert!(
            matches!(account_state(&facts), Some(Readiness::Depleted { .. })),
            "age {age_seconds} seconds should retain the exhaustion fact"
        );
    }

    // Once older than the documented bound, the old exhaustion cannot create
    // a readiness fact and therefore cannot steer the next route.
    for age_seconds in [3_601, 3_602, 7_200] {
        let facts = depletion_facts_at_age(age_seconds)?;
        assert_eq!(
            account_state(&facts),
            None,
            "age {age_seconds} seconds should leave this account unknown"
        );
    }
    Ok(())
}
