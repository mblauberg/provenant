//! Resolves landing policy without adding configuration-loader layers.
use std::collections::BTreeMap;

use super::model::PublicationGrant;
use provenant_cli::domains::{AdminPublication, LandOverride, LandSettings};
use provenant_config::Origin;
use provenant_records::Layer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetRule {
    pub index: usize,
    pub target: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub layer: Layer,
    pub path: Option<Value>,
    pub key: String,
    pub rule: Option<TargetRule>,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}

impl Source {
    fn new(layer: Layer, key: String, path: Option<Value>) -> Self {
        Self {
            layer,
            path,
            key,
            rule: None,
            extra: Default::default(),
        }
    }

    fn field(
        &self,
        field: &str,
        origins: &BTreeMap<String, Origin>,
    ) -> Result<Self, serde_json::Error> {
        let mut source = self.clone();
        if !field.is_empty() {
            source.key = format!("{}.{field}", source.key);
        }
        if source.path.is_none() {
            source.path = origins
                .get(&source.key)
                .or_else(|| {
                    origins
                        .iter()
                        .find(|(key, origin)| {
                            (source.key.starts_with(&format!("{key}."))
                                || key.starts_with(&format!("{}.", source.key)))
                                && matches!(origin, Origin::User { .. })
                        })
                        .map(|(_, origin)| origin)
                })
                .and_then(|origin| match origin {
                    Origin::User { path } => Some(path),
                    _ => None,
                })
                .map(serde_json::to_value)
                .transpose()?;
        }
        Ok(source)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}

/// Resolve the admin-publication grant. Only the owner's host config may grant
/// it: other layers are reported with [`provenant_records::warning::SETTING_LAYER_IGNORED`]
/// and fall back to `never`. The shared `Precedence` merge reports the ignored
/// candidate with its source so the warning never invents a second vocabulary.
/// Spec: docs/specs/next-batch.md#seam-5-the-worktree-and-landing-loop-a-chair-actually-uses
fn admin_candidate(
    field: &Sourced<Value>,
) -> Result<provenant_records::Sourced<AdminPublication>, serde_json::Error> {
    use provenant_records::{Source as SharedSource, Sourced as SharedSourced};
    let value: AdminPublication = serde_json::from_value(field.value.clone())?;
    let origin = field.source.path.as_ref().map(|path| {
        path.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| path.to_string())
    });
    Ok(SharedSourced {
        value,
        source: SharedSource {
            layer: field.source.layer,
            origin,
        },
    })
}

fn resolve_admin_publication(
    fields: &BTreeMap<String, Sourced<Value>>,
    base: Option<provenant_records::Sourced<AdminPublication>>,
) -> Result<PublicationGrant, serde_json::Error> {
    use provenant_records::warning;
    let final_candidate = fields
        .get("admin_publication")
        .map(admin_candidate)
        .transpose()?;
    let candidates = base.into_iter().chain(final_candidate).collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(PublicationGrant::default());
    }
    let merged = provenant_fabric_runtime::config::ADMIN_PUBLICATION_PRECEDENCE.merge(candidates);
    for ignored in merged.ignored {
        warning::warn(
            warning::Warning::new(
                warning::SETTING_LAYER_IGNORED,
                format!(
                    "domains.pr.land.admin_publication from {} is ignored; only the host layer may grant admin publication",
                    ignored.source.layer.description()
                ),
            )
            .subject("domains.pr.land.admin_publication")
            .source(ignored.source)
            .repair("set admin_publication in the owner's host config [domains.pr.land]")
            .once_per_change(),
        );
    }
    Ok(merged
        .winner
        .map(|winner| PublicationGrant {
            mode: winner.value,
            source: winner.source,
        })
        .unwrap_or_default())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LandPolicy {
    /// Publication declaration sealed in the plan and pending intent.
    pub publication_port: provenant_config::LandPort,
    /// Frozen contract carried by candidate markers and receipts before dispatch.
    pub landing: Option<provenant_records::landing::LandingContext>,
    pub fields: BTreeMap<String, Sourced<Value>>,
    pub budget_rate_micros_per_minute: BTreeMap<String, Sourced<u64>>,
    /// Admin publication is a host-only grant; its source travels with the policy.
    #[serde(default)]
    pub publication: PublicationGrant,
    /// The landing target and where it came from (#3191). Set by the caller
    /// once the target is resolved from members or the repository default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_source: Option<provenant_records::provenance::Sourced<String>>,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
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

    /// The baseline is the loader's bundled/host snapshot. Repository declarations
    /// precede explicit host values; target rules follow project overrides.
    pub fn resolve(
        settings: &LandSettings,
        origins: &BTreeMap<String, Origin>,
        project: Option<&str>,
        tier: Option<&str>,
        invocation: &LandOverride,
    ) -> Result<Self, serde_json::Error> {
        Self::resolve_for(settings, origins, project, tier, invocation, None, None)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one resolver receives the ordered configuration layers"
    )]
    pub fn resolve_for(
        settings: &LandSettings,
        origins: &BTreeMap<String, Origin>,
        project: Option<&str>,
        tier: Option<&str>,
        invocation: &LandOverride,
        repository: Option<(&LandOverride, &std::path::Path)>,
        target: Option<&str>,
    ) -> Result<Self, serde_json::Error> {
        let mut base = serde_json::to_value(settings)?;
        if let Value::Object(ref mut map) = base {
            for key in ["projects", "tiers", "branches"] {
                map.remove(key);
            }
        }
        let mut leaves = BTreeMap::new();
        flatten("", base, &mut leaves);
        leaves.remove("budget.rate_micros_per_minute");
        let mut budget_rate_micros_per_minute = BTreeMap::new();
        for (pool, value) in &settings.budget.rate_micros_per_minute {
            let key = format!("domains.pr.land.budget.rate_micros_per_minute.{pool}");
            let source = match origins.get(&key) {
                Some(Origin::User { path }) => {
                    Source::new(Layer::Host, key.clone(), Some(serde_json::to_value(path)?))
                }
                _ => Source::new(Layer::Default, key.clone(), None),
            };
            budget_rate_micros_per_minute.insert(
                pool.clone(),
                Sourced {
                    value: *value,
                    source,
                    extra: Default::default(),
                },
            );
        }
        let mut fields = BTreeMap::new();
        for (key, value) in leaves {
            let path = format!("domains.pr.land.{key}");
            let origin = origins.get(&path).or_else(|| {
                origins
                    .iter()
                    .find(|(name, origin)| {
                        (name.starts_with(&format!("{path}."))
                            || path.starts_with(&format!("{name}.")))
                            && matches!(origin, Origin::User { .. })
                    })
                    .map(|(_, origin)| origin)
            });
            let source = match origin {
                Some(Origin::User { path: file }) => {
                    Source::new(Layer::Host, path, Some(serde_json::to_value(file)?))
                }
                _ => Source::new(Layer::Default, path, None),
            };
            fields.insert(
                key,
                Sourced {
                    value,
                    source,
                    extra: Default::default(),
                },
            );
        }
        // Keep the host/bundled candidate before any lower-precedence layer
        // overwrites the field: a tier or project value must not suppress a
        // grant the owner made in the host config.
        let base_admin = fields
            .get("admin_publication")
            .map(admin_candidate)
            .transpose()?;
        fn apply(
            fields: &mut BTreeMap<String, Sourced<Value>>,
            override_: &LandOverride,
            source: Source,
            beneath_host: bool,
            rates: &mut BTreeMap<String, Sourced<u64>>,
            origins: &BTreeMap<String, Origin>,
        ) -> Result<(), serde_json::Error> {
            let mut values = BTreeMap::new();
            flatten("", serde_json::to_value(override_)?, &mut values);
            values.remove("branches");
            values.remove("budget.rate_micros_per_minute");
            if let Some(values) = override_
                .budget
                .as_ref()
                .and_then(|budget| budget.rate_micros_per_minute.as_ref())
            {
                for (pool, value) in values {
                    if !beneath_host
                        || !rates
                            .get(pool)
                            .is_some_and(|rate| rate.source.layer == Layer::Host)
                    {
                        rates.insert(
                            pool.clone(),
                            Sourced {
                                value: *value,
                                source: source.field(
                                    &format!("budget.rate_micros_per_minute.{pool}"),
                                    origins,
                                )?,
                                extra: Default::default(),
                            },
                        );
                    }
                }
            }
            values.remove("");
            for (key, value) in values {
                if !beneath_host
                    || !fields
                        .get(&key)
                        .is_some_and(|field| field.source.layer == Layer::Host)
                {
                    let field_source = source.field(&key, origins)?;
                    fields.insert(
                        key,
                        Sourced {
                            value,
                            source: field_source,
                            extra: Default::default(),
                        },
                    );
                }
            }
            Ok(())
        }
        if let Some((declaration, path)) = repository {
            apply(
                &mut fields,
                declaration,
                Source::new(
                    Layer::Project,
                    "domains.pr.land".into(),
                    Some(serde_json::to_value(path)?),
                ),
                true,
                &mut budget_rate_micros_per_minute,
                origins,
            )?;
        }
        if let Some(override_) = project.and_then(|key| settings.projects.get(key)) {
            apply(
                &mut fields,
                override_,
                Source::new(
                    Layer::HostProject,
                    format!("domains.pr.land.projects.{}", project.unwrap_or_default()),
                    None,
                ),
                false,
                &mut budget_rate_micros_per_minute,
                origins,
            )?;
        }
        let rules_from_host = origins.iter().any(|(key, origin)| {
            key.starts_with("domains.pr.land.branches") && matches!(origin, Origin::User { .. })
        });
        let project_rules = project
            .and_then(|key| settings.projects.get(key))
            .and_then(|override_| override_.branches.as_ref());
        let (rules, rules_key, rules_path) = if let Some(rules) = project_rules {
            (
                rules,
                format!(
                    "domains.pr.land.projects.{}.branches",
                    project.unwrap_or_default()
                ),
                None,
            )
        } else if rules_from_host {
            (&settings.branches, "domains.pr.land.branches".into(), None)
        } else if let Some((declaration, path)) =
            repository.filter(|(declaration, _)| declaration.branches.is_some())
        {
            (
                declaration.branches.as_ref().unwrap_or(&settings.branches),
                "domains.pr.land.branches".into(),
                Some(serde_json::to_value(path)?),
            )
        } else {
            (&settings.branches, "domains.pr.land.branches".into(), None)
        };
        let rules_file = Source::new(Layer::Route, rules_key.clone(), rules_path)
            .field("", origins)?
            .path;
        if let Some(target) = target
            && let Some((index, rule)) = rules
                .iter()
                .enumerate()
                .rev()
                .find(|(_, rule)| target_matches(&rule.target, target))
        {
            let override_ = LandOverride {
                strategy: rule.strategy,
                compose: rule.compose.clone(),
                ..LandOverride::default()
            };
            apply(
                &mut fields,
                &override_,
                Source {
                    layer: Layer::Route,
                    path: rules_file.clone(),
                    key: format!("{rules_key}.{index}"),
                    rule: Some(TargetRule {
                        index,
                        target: rule.target.clone(),
                        extra: Default::default(),
                    }),
                    extra: Default::default(),
                },
                false,
                &mut budget_rate_micros_per_minute,
                origins,
            )?;
        }
        for (layer, key, override_) in [
            (
                Layer::Tier,
                tier.map(|key| format!("domains.pr.land.tiers.{key}")),
                tier.and_then(|key| settings.tiers.get(key)),
            ),
            (Layer::Launch, None, Some(invocation)),
        ] {
            if let Some(override_) = override_ {
                if override_.branches.is_some() {
                    return Err(<serde_json::Error as serde::de::Error>::custom(
                        "branch lists belong to pre-target repository, host or project declarations",
                    ));
                }
                apply(
                    &mut fields,
                    override_,
                    Source::new(layer, key.unwrap_or_else(|| "domains.pr.land".into()), None),
                    false,
                    &mut budget_rate_micros_per_minute,
                    origins,
                )?;
            }
        }
        let publication = resolve_admin_publication(&fields, base_admin)?;
        Ok(Self {
            publication_port: provenant_config::PortTable::builtin().land,
            landing: None,
            fields,
            budget_rate_micros_per_minute,
            publication,
            target_source: None,
            extra: Default::default(),
        })
    }

    pub fn from_plan(
        plan: &provenant_records::landing::ResolvedPlan,
    ) -> Result<Self, serde_json::Error> {
        let value = plan
            .provenance
            .get("policy")
            .cloned()
            .ok_or_else(|| <serde_json::Error as serde::de::Error>::missing_field("policy"))?;
        serde_json::from_value(value)
    }

    pub fn review_scope(
        plan: &provenant_records::landing::ResolvedPlan,
    ) -> Result<provenant_records::landing::ReviewScope, serde_json::Error> {
        let value = plan
            .provenance
            .get("review_scope")
            .cloned()
            .ok_or_else(|| {
                <serde_json::Error as serde::de::Error>::missing_field("review_scope")
            })?;
        serde_json::from_value(value)
    }

    pub fn for_request(
        &self,
        request: &provenant_records::landing::LandRequest,
    ) -> Result<Self, serde_json::Error> {
        let mut policy = self.clone();
        let override_ = LandOverride {
            strategy: request.strategy,
            compose: Some(request.compose.clone()),
            ..Default::default()
        };
        let mut values = BTreeMap::new();
        flatten("", serde_json::to_value(override_)?, &mut values);
        values.remove("compose");
        values.remove("");
        for (key, value) in values {
            policy.fields.insert(
                key.clone(),
                Sourced {
                    value,
                    source: Source::new(Layer::Launch, format!("request.{key}"), None),
                    extra: Default::default(),
                },
            );
        }
        Ok(policy)
    }

    pub fn plan(
        &self,
        capabilities: provenant_records::landing::Capabilities,
        review_scope: &provenant_records::landing::ReviewScope,
    ) -> Result<provenant_records::landing::ResolvedPlan, serde_json::Error> {
        let settings = self.settings()?;
        let mut snapshot = self.clone();
        snapshot.landing = None;
        let provenance = BTreeMap::from([
            ("policy".into(), serde_json::to_value(snapshot)?),
            ("review_scope".into(), serde_json::to_value(review_scope)?),
        ]);
        provenant_records::landing::ResolvedPlan::new(
            settings.strategy,
            &settings.compose,
            capabilities,
            provenance,
        )
    }
}

/// Small deterministic glob matcher for target rules (`*` and `?`).
fn target_matches(pattern: &str, target: &str) -> bool {
    let (pattern, target): (Vec<_>, Vec<_>) = (pattern.chars().collect(), target.chars().collect());
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < target.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == target[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(index) = star {
            retry += 1;
            t = retry;
            p = index + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use provenant_cli::domains::{BudgetOverride, GateOverride, Strategy};
    use provenant_core::RelativePath;
    use provenant_records::PathWire;

    #[test]
    fn resolution_precedence_and_last_target_rule_are_persisted()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_cli::domains::{LandBranch, Strategy};
        let mut settings = LandSettings::default();
        settings.projects.insert(
            "p".into(),
            LandOverride {
                strategy: Some(Strategy::Direct),
                ..Default::default()
            },
        );
        settings.tiers.insert(
            "risk".into(),
            LandOverride {
                strategy: Some(Strategy::Async),
                ..Default::default()
            },
        );
        let declaration = LandOverride {
            strategy: Some(Strategy::Stack),
            batch_size: Some(3),
            branches: Some(vec![
                LandBranch {
                    target: "release/*".into(),
                    strategy: Some(Strategy::Batch),
                    ..Default::default()
                },
                LandBranch {
                    target: "release/?".into(),
                    strategy: Some(Strategy::Queue),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let origins = BTreeMap::from([(
            "domains.pr.land.strategy".into(),
            Origin::User {
                path: PathWire::from_relative(&RelativePath::parse("host.toml")?)?,
            },
        )]);
        let cases = [
            (None, None, None, None, Strategy::Batch, Layer::Host),
            (
                Some("p"),
                None,
                None,
                None,
                Strategy::Direct,
                Layer::HostProject,
            ),
            (
                Some("p"),
                Some("release/x"),
                None,
                None,
                Strategy::Queue,
                Layer::Route,
            ),
            (
                Some("p"),
                Some("release/x"),
                Some("risk"),
                None,
                Strategy::Async,
                Layer::Tier,
            ),
            (
                Some("p"),
                Some("release/x"),
                Some("risk"),
                Some(Strategy::Direct),
                Strategy::Direct,
                Layer::Launch,
            ),
        ];
        for (project, target, tier, strategy, expected, source) in cases {
            let policy = LandPolicy::resolve_for(
                &settings,
                &origins,
                project,
                tier,
                &LandOverride {
                    strategy,
                    ..Default::default()
                },
                Some((&declaration, std::path::Path::new(".provenant/config.toml"))),
                target,
            )?;
            assert_eq!(policy.settings()?.strategy, expected);
            assert_eq!(policy.fields["strategy"].source.layer, source);
            assert_eq!(policy.settings()?.batch_size, 3);
            let snapshot = policy.plan(Default::default(), &Default::default())?;
            let decoded: provenant_records::landing::ResolvedPlan =
                serde_json::from_slice(&serde_json::to_vec(&snapshot)?)?;
            assert_eq!(snapshot, decoded);
        }
        settings
            .projects
            .get_mut("p")
            .ok_or("missing project")?
            .branches = Some(vec![LandBranch {
            target: "release/*".into(),
            strategy: Some(Strategy::Stack),
            ..Default::default()
        }]);
        let project_rules = LandPolicy::resolve_for(
            &settings,
            &origins,
            Some("p"),
            None,
            &LandOverride::default(),
            Some((&declaration, std::path::Path::new(".provenant/config.toml"))),
            Some("release/x"),
        )?;
        assert_eq!(project_rules.settings()?.strategy, Strategy::Stack);
        assert_eq!(
            project_rules.fields["strategy"].source.key,
            "domains.pr.land.projects.p.branches.0.strategy"
        );
        assert!(
            serde_json::from_value::<LandSettings>(serde_json::json!({"integration":"single"}))
                .is_err()
        );
        Ok(())
    }

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
        assert_eq!(field.source.layer, Layer::Default);
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
        assert_eq!(
            policy.fields["approval_sources"].source.layer,
            Layer::HostProject
        );
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
            Layer::Host
        );
        assert_eq!(
            policy.budget_rate_micros_per_minute["project.with.dot"]
                .source
                .layer,
            Layer::HostProject
        );
        assert_eq!(
            policy.budget_rate_micros_per_minute["shared"].source.layer,
            Layer::Tier
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
            strategy: Some(Strategy::Direct),
            ..LandOverride::default()
        };
        let base = LandPolicy::resolve(&settings, &origins, None, None, &LandOverride::default())?;
        assert_eq!(base.fields["publish_settle_seconds"].value, 30);
        assert_eq!(base.fields["batch_size"].value, 7);
        assert_eq!(base.fields["batch_size"].source.layer, Layer::Host);
        assert!(base.fields["batch_size"].source.path.is_some());
        assert_eq!(base.fields["gate.kind"].source.layer, Layer::Default);
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
            Layer::HostProject
        );
        assert_eq!(project.fields["batch_size"].value, 4);
        assert_eq!(
            project.fields["batch_size"].source.layer,
            Layer::HostProject
        );
        assert_eq!(project.fields["gate.timeout_seconds"].value, 50);
        let tier = LandPolicy::resolve(
            &settings,
            &origins,
            Some("p"),
            Some("high"),
            &LandOverride::default(),
        )?;
        assert_eq!(tier.fields["publish_settle_seconds"].value, 7);
        assert_eq!(
            tier.fields["publish_settle_seconds"].source.layer,
            Layer::Tier
        );
        assert_eq!(tier.fields["batch_size"].value, 2);
        assert_eq!(tier.fields["batch_size"].source.layer, Layer::Tier);
        let effective =
            LandPolicy::resolve(&settings, &origins, Some("p"), Some("high"), &invocation)?;
        assert_eq!(effective.fields["batch_size"].value, 1);
        assert_eq!(effective.fields["batch_size"].source.layer, Layer::Launch);
        assert_eq!(
            effective.fields["gate.timeout_seconds"].source.layer,
            Layer::HostProject
        );
        let resolved = effective.settings()?;
        assert_eq!(resolved.strategy, Strategy::Direct);
        assert_eq!(resolved.batch_size, 1);
        assert_eq!(resolved.gate.timeout_seconds, 50);
        Ok(())
    }

    // Spec: docs/specs/next-batch.md#seam-5-the-worktree-and-landing-loop-a-chair-actually-uses
    #[test]
    fn admin_publication_is_honoured_only_from_the_host_layer()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_records::warning;

        let mut settings = LandSettings::default();
        settings.admin_publication = AdminPublication::WhenReviewUnavailable;
        let path = PathWire::from_relative(&RelativePath::parse("config.toml")?)?;
        let origins = BTreeMap::from([(
            "domains.pr.land.admin_publication".into(),
            Origin::User { path },
        )]);
        let (host, warnings) = warning::capture(|| {
            LandPolicy::resolve(&settings, &origins, None, None, &LandOverride::default())
        });
        let host = host?;
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            host.publication.mode,
            AdminPublication::WhenReviewUnavailable
        );
        assert_eq!(host.publication.source.layer, Layer::Host);
        assert!(host.publication.allows_review_unavailable());

        let mut project = LandSettings::default();
        project.projects.insert(
            "p".into(),
            LandOverride {
                admin_publication: Some(AdminPublication::WhenReviewUnavailable),
                ..Default::default()
            },
        );
        let (ignored, warnings) = warning::capture(|| {
            LandPolicy::resolve(
                &project,
                &BTreeMap::new(),
                Some("p"),
                None,
                &LandOverride::default(),
            )
        });
        let ignored = ignored?;
        assert_eq!(ignored.publication.mode, AdminPublication::Never);
        assert!(!ignored.publication.allows_review_unavailable());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code.as_str(), "SETTING_LAYER_IGNORED");
        assert_eq!(
            warnings[0].source.as_ref().map(|source| source.layer),
            Some(Layer::HostProject)
        );
        Ok(())
    }
}
