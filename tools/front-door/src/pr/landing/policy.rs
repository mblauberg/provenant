//! Resolves landing policy without adding configuration-loader layers.
use std::collections::BTreeMap;

use provenant_cli::domains::{LandOverride, LandSettings};
use provenant_config::Origin;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub layer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LandPolicy {
    pub fields: BTreeMap<String, Sourced<Value>>,
    pub budget_rate_micros_per_minute: BTreeMap<String, Sourced<u64>>,
}

fn flatten(prefix: &str, value: Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) if !map.is_empty() && prefix != "budget.rate_micros_per_minute" => {
            for (key, value) in map {
                let path = if prefix.is_empty() {
                    key
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, value, out);
            }
        }
        value => {
            out.insert(prefix.to_owned(), value);
        }
    }
}

impl LandPolicy {
    /// Reconstruct the effective settings for semantic validation and consumption.
    pub fn settings(&self) -> Result<LandSettings, serde_json::Error> {
        fn insert(map: &mut serde_json::Map<String, Value>, parts: &[&str], value: Value) {
            if let Some((head, tail)) = parts.split_first() {
                if tail.is_empty() {
                    map.insert((*head).into(), value);
                } else {
                    let entry = map
                        .entry(*head)
                        .or_insert_with(|| Value::Object(Default::default()));
                    if let Value::Object(child) = entry {
                        insert(child, tail, value);
                    }
                }
            }
        }
        let mut object = serde_json::Map::new();
        for (path, field) in &self.fields {
            insert(
                &mut object,
                &path.split('.').collect::<Vec<_>>(),
                field.value.clone(),
            );
        }
        let rates = self
            .budget_rate_micros_per_minute
            .iter()
            .map(|(pool, rate)| (pool.clone(), Value::from(rate.value)))
            .collect();
        insert(
            &mut object,
            &["budget", "rate_micros_per_minute"],
            Value::Object(rates),
        );
        serde_json::from_value(Value::Object(object))
    }

    /// Resolve host values and apply project, tier and invocation overrides in order.
    pub fn resolve(
        settings: &LandSettings,
        origins: &BTreeMap<String, Origin>,
        project: Option<&str>,
        tier: Option<&str>,
        invocation: &LandOverride,
    ) -> Result<Self, serde_json::Error> {
        let mut base = serde_json::to_value(settings)?;
        if let Value::Object(ref mut map) = base {
            map.remove("projects");
            map.remove("tiers");
        }
        let mut leaves = BTreeMap::new();
        flatten("", base, &mut leaves);
        leaves.remove("budget.rate_micros_per_minute");
        let mut budget_rate_micros_per_minute = BTreeMap::new();
        for (pool, rate) in &settings.budget.rate_micros_per_minute {
            let path = format!("domains.pr.land.budget.rate_micros_per_minute.{pool}");
            let source = match origins.get(&path) {
                Some(Origin::User { path }) => Source {
                    layer: "host".into(),
                    path: Some(serde_json::to_value(path)?),
                },
                _ => Source {
                    layer: "bundled".into(),
                    path: None,
                },
            };
            budget_rate_micros_per_minute.insert(
                pool.clone(),
                Sourced {
                    value: *rate,
                    source,
                },
            );
        }
        let mut fields = BTreeMap::new();
        for (key, value) in leaves {
            let path = format!("domains.pr.land.{key}");
            let origin = origins.get(&path).or_else(|| {
                let prefix = format!("{path}.");
                origins
                    .range(prefix.clone()..)
                    .take_while(|(name, _)| name.starts_with(&prefix))
                    .map(|(_, origin)| origin)
                    .find(|origin| matches!(origin, Origin::User { .. }))
            });
            let source = match origin {
                Some(Origin::User { path }) => Source {
                    layer: "host".into(),
                    path: Some(serde_json::to_value(path)?),
                },
                _ => Source {
                    layer: "bundled".into(),
                    path: None,
                },
            };
            fields.insert(key, Sourced { value, source });
        }
        for (layer, override_) in [
            (
                "project",
                project.and_then(|key| settings.projects.get(key)),
            ),
            ("tier", tier.and_then(|key| settings.tiers.get(key))),
            ("invocation", Some(invocation)),
        ] {
            if let Some(override_) = override_ {
                if let Some(rates) = override_
                    .budget
                    .as_ref()
                    .and_then(|budget| budget.rate_micros_per_minute.as_ref())
                {
                    for (pool, rate) in rates {
                        budget_rate_micros_per_minute.insert(
                            pool.clone(),
                            Sourced {
                                value: *rate,
                                source: Source {
                                    layer: layer.into(),
                                    path: None,
                                },
                            },
                        );
                    }
                }
                let mut values = BTreeMap::new();
                flatten("", serde_json::to_value(override_)?, &mut values);
                for (key, value) in values {
                    if let Some(field) = fields.get_mut(&key) {
                        *field = Sourced {
                            value,
                            source: Source {
                                layer: layer.into(),
                                path: None,
                            },
                        };
                    }
                }
            }
        }
        Ok(Self {
            fields,
            budget_rate_micros_per_minute,
        })
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use provenant_cli::domains::{BudgetOverride, GateOverride, Integration};
    use provenant_core::RelativePath;
    use provenant_records::PathWire;

    #[test]
    fn approval_sources_are_typed_overridable_and_sourced() -> Result<(), Box<dyn std::error::Error>>
    {
        use provenant_cli::domains::ApprovalSource;
        let mut settings = LandSettings::default();
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        assert_eq!(
            settings.approval_sources,
            vec![ApprovalSource::ProvenantReview, ApprovalSource::Forge]
        );
        let field = &policy.fields["approval_sources"];
        assert_eq!(
            field.value,
            serde_json::to_value(&settings.approval_sources)?
        );
        assert_eq!(field.source.layer, "bundled");
        settings.projects.insert(
            "p".into(),
            LandOverride {
                approval_sources: Some(vec![ApprovalSource::Forge]),
                ..LandOverride::default()
            },
        );
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            Some("p"),
            None,
            &LandOverride::default(),
        )?;
        assert_eq!(
            policy.fields["approval_sources"].value,
            serde_json::json!(["forge"])
        );
        assert_eq!(policy.fields["approval_sources"].source.layer, "project");
        assert!(
            serde_json::from_value::<LandSettings>(
                serde_json::json!({"approval_sources":["receipt"]})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<LandSettings>(serde_json::json!({"approval":"forge"}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn budget_rate_micros_per_minute_merge_per_key_with_independent_sources()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut settings = LandSettings::default();
        settings.budget.rate_micros_per_minute =
            BTreeMap::from([("host".into(), 10), ("shared".into(), 20)]);
        settings.projects.insert(
            "p".into(),
            LandOverride {
                budget: Some(BudgetOverride {
                    rate_micros_per_minute: Some(BTreeMap::from([
                        ("project.with.dot".into(), 30),
                        ("shared".into(), 40),
                    ])),
                    ..BudgetOverride::default()
                }),
                ..LandOverride::default()
            },
        );
        settings.tiers.insert(
            "high".into(),
            LandOverride {
                budget: Some(BudgetOverride {
                    rate_micros_per_minute: Some(BTreeMap::from([("shared".into(), 50)])),
                    ..BudgetOverride::default()
                }),
                ..LandOverride::default()
            },
        );
        let path = PathWire::from_relative(&RelativePath::parse("config.toml")?)?;
        let origins = BTreeMap::from([(
            "domains.pr.land.budget.rate_micros_per_minute.host".into(),
            Origin::User { path },
        )]);
        let policy = LandPolicy::resolve(
            &settings,
            &origins,
            Some("p"),
            Some("high"),
            &LandOverride::default(),
        )?;
        assert_eq!(
            policy.budget_rate_micros_per_minute["host"].source.layer,
            "host"
        );
        assert_eq!(
            policy.budget_rate_micros_per_minute["project.with.dot"]
                .source
                .layer,
            "project"
        );
        assert_eq!(
            policy.budget_rate_micros_per_minute["shared"].source.layer,
            "tier"
        );
        assert_eq!(
            policy.settings()?.budget.rate_micros_per_minute,
            BTreeMap::from([
                ("host".into(), 10),
                ("project.with.dot".into(), 30),
                ("shared".into(), 50)
            ])
        );
        Ok(())
    }

    #[test]
    fn resolves_host_project_tier_and_invocation_by_leaf() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut settings = LandSettings {
            batch_size: 7,
            ..LandSettings::default()
        };
        settings.projects.insert(
            "p".into(),
            LandOverride {
                batch_size: Some(4),
                publish_settle_seconds: Some(12),
                gate: Some(GateOverride {
                    timeout_seconds: Some(50),
                    ..GateOverride::default()
                }),
                ..LandOverride::default()
            },
        );
        settings.tiers.insert(
            "high".into(),
            LandOverride {
                batch_size: Some(2),
                publish_settle_seconds: Some(7),
                ..LandOverride::default()
            },
        );
        let path = PathWire::from_relative(&RelativePath::parse("config.toml")?)?;
        let origins =
            BTreeMap::from([("domains.pr.land.batch_size".into(), Origin::User { path })]);
        let invocation = LandOverride {
            batch_size: Some(1),
            integration: Some(Integration::Single),
            ..LandOverride::default()
        };
        let base = LandPolicy::resolve(&settings, &origins, None, None, &LandOverride::default())?;
        assert_eq!(base.fields["publish_settle_seconds"].value, 30);
        assert_eq!(base.fields["batch_size"].value, 7);
        assert_eq!(base.fields["batch_size"].source.layer, "host");
        assert!(base.fields["batch_size"].source.path.is_some());
        assert_eq!(base.fields["gate.kind"].source.layer, "bundled");
        let project = LandPolicy::resolve(
            &settings,
            &origins,
            Some("p"),
            None,
            &LandOverride::default(),
        )?;
        assert_eq!(project.fields["publish_settle_seconds"].value, 12);
        assert_eq!(
            project.fields["publish_settle_seconds"].source.layer,
            "project"
        );
        assert_eq!(project.fields["batch_size"].value, 4);
        assert_eq!(project.fields["batch_size"].source.layer, "project");
        assert_eq!(project.fields["gate.timeout_seconds"].value, 50);
        let tier = LandPolicy::resolve(
            &settings,
            &origins,
            Some("p"),
            Some("high"),
            &LandOverride::default(),
        )?;
        assert_eq!(tier.fields["publish_settle_seconds"].value, 7);
        assert_eq!(tier.fields["publish_settle_seconds"].source.layer, "tier");
        assert_eq!(tier.fields["batch_size"].value, 2);
        assert_eq!(tier.fields["batch_size"].source.layer, "tier");
        let effective =
            LandPolicy::resolve(&settings, &origins, Some("p"), Some("high"), &invocation)?;
        assert_eq!(effective.fields["batch_size"].value, 1);
        assert_eq!(effective.fields["batch_size"].source.layer, "invocation");
        assert_eq!(
            effective.fields["gate.timeout_seconds"].source.layer,
            "project"
        );
        let resolved = effective.settings()?;
        assert_eq!(resolved.integration, Integration::Single);
        assert_eq!(resolved.batch_size, 1);
        assert_eq!(resolved.gate.timeout_seconds, 50);
        Ok(())
    }
}
