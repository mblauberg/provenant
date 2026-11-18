//! Provider listings are data, not evidence of confidentiality or capability.
use crate::{ModelId, RoutingError};
use serde::{Deserialize, Serialize};

/// A successful provider listing. A failed probe must not replace a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverySnapshot {
    pub adapter: String,
    pub observed_at: i64,
    pub models: Vec<DiscoveredModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub model: ModelId,
    pub efforts: Vec<String>,
    #[serde(default)]
    pub controls_known: bool,
    #[serde(default)]
    pub effort_suffix: bool,
    pub service_tiers: Vec<String>,
}

/// Decode Codex's model-list JSON, retaining only the advertised controls.
pub fn parse_codex_models(
    bytes: &[u8],
    observed_at: i64,
) -> Result<DiscoverySnapshot, RoutingError> {
    #[derive(Deserialize)]
    struct Listing {
        models: Vec<Row>,
    }
    #[derive(Deserialize)]
    struct Row {
        slug: String,
        #[serde(default)]
        supported_reasoning_levels: Vec<Effort>,
        #[serde(default)]
        service_tiers: Vec<Tier>,
    }
    #[derive(Deserialize)]
    struct Effort {
        effort: String,
    }
    #[derive(Deserialize)]
    struct Tier {
        id: String,
    }
    let listing: Listing = serde_json::from_slice(bytes).map_err(|_| RoutingError::Catalogue)?;
    let mut models = Vec::new();
    for row in listing.models {
        let model = ModelId::parse(&format!(
            "{}/{}",
            provenant_records::Harness::Codex.spec().name,
            row.slug
        ))?;
        let efforts: Vec<_> = row
            .supported_reasoning_levels
            .into_iter()
            .map(|e| e.effort)
            .collect();
        let service_tiers: Vec<_> = row.service_tiers.into_iter().map(|t| t.id).collect();
        if efforts
            .iter()
            .chain(&service_tiers)
            .any(|s| !crate::request::identifier(s))
        {
            return Err(RoutingError::Catalogue);
        }
        models.push(DiscoveredModel {
            model,
            efforts,
            controls_known: true,
            effort_suffix: false,
            service_tiers,
        });
    }
    if models.is_empty() || models.len() > 1024 {
        return Err(RoutingError::Catalogue);
    }
    models.sort_by(|a, b| a.model.cmp(&b.model));
    if models.windows(2).any(|rows| rows[0].model == rows[1].model) {
        return Err(RoutingError::Catalogue);
    }
    Ok(DiscoverySnapshot {
        adapter: provenant_records::Harness::Codex.spec().name.into(),
        observed_at,
        models,
    })
}

/// Decode one model ID per line, optionally followed by a tab-separated label.
/// Diagnostics on stderr must not be supplied as listing data.
pub fn parse_text_models(
    adapter: &str,
    text: &str,
    observed_at: i64,
) -> Result<DiscoverySnapshot, RoutingError> {
    let (cursor_format, effort_suffixes) =
        match provenant_records::AdapterSpec::get(adapter).and_then(|spec| spec.model_listing) {
            Some(provenant_records::ModelListingFormat::CursorText) => (true, true),
            Some(provenant_records::ModelListingFormat::AgyText) => (false, true),
            Some(provenant_records::ModelListingFormat::OpencodeText) => (false, false),
            _ => return Err(RoutingError::UnknownAdapter),
        };
    let mut models = std::collections::BTreeMap::<ModelId, DiscoveredModel>::new();
    let mut raw_ids = std::collections::BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if cursor_format && line.trim() == "Available models" {
            continue;
        }
        let id = if cursor_format {
            line.split(" - ").next()
        } else {
            line.split('\t').next()
        }
        .unwrap_or_default()
        .trim();
        let model = ModelId::parse(&format!("{adapter}/{id}"))?;
        if !raw_ids.insert(model.clone()) {
            return Err(RoutingError::Catalogue);
        }
        models
            .entry(model.clone())
            .or_insert_with(|| DiscoveredModel {
                model,
                efforts: Vec::new(),
                controls_known: false,
                effort_suffix: false,
                service_tiers: Vec::new(),
            });
        if effort_suffixes
            && let Some((base, effort)) = id.rsplit_once('-')
            && matches!(effort, "low" | "medium" | "high" | "xhigh" | "max")
        {
            let canonical = ModelId::parse(&format!("{adapter}/{base}"))?;
            let entry = models
                .entry(canonical.clone())
                .or_insert_with(|| DiscoveredModel {
                    model: canonical,
                    efforts: Vec::new(),
                    controls_known: true,
                    effort_suffix: cursor_format,
                    service_tiers: Vec::new(),
                });
            entry.controls_known = true;
            entry.effort_suffix = cursor_format;
            if !entry.efforts.iter().any(|value| value == effort) {
                entry.efforts.push(effort.into());
            }
        }
    }
    if models.is_empty() || models.len() > 1024 {
        return Err(RoutingError::Catalogue);
    }
    let models = models.into_values().collect();
    Ok(DiscoverySnapshot {
        adapter: adapter.into(),
        observed_at,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/routing.md#provider-model-discovery
    #[test]
    fn discovery_updates_family_selection_without_inheriting_security_claims() {
        let input = br#"{"models":[{"slug":"gpt-6.2-sol","supported_reasoning_levels":[{"effort":"medium"}],"service_tiers":[]}]}"#;
        let snapshot = parse_codex_models(input, 123).unwrap();
        let catalogue = crate::Catalogue::bundled()
            .unwrap()
            .with_discovery(&[snapshot])
            .unwrap();
        let id = ModelId::parse("codex/gpt-6.2-sol").unwrap();
        let model = catalogue.model(&id).unwrap();
        assert_eq!(model.source, "discovered");
        assert_eq!(model.trains_on_prompts, None);
        assert!(
            catalogue.pools["senior"]
                .iter()
                .any(|member| member.model == id)
        );
        assert!(
            catalogue.pools["senior"]
                .iter()
                .find(|member| member.model == id)
                .unwrap()
                .efforts
                .iter()
                .all(|effort| effort == "medium")
        );
    }

    // Spec: docs/specs/routing.md#provider-model-discovery
    #[test]
    fn text_listings_preserve_effort_variants_and_canonical_models() {
        let agy = parse_text_models(
            "agy",
            "gemini-9-flash-high\tHigh\ngemini-9-flash-low\tLow\n",
            1,
        )
        .unwrap();
        let model = agy
            .models
            .iter()
            .find(|row| row.model.as_str() == "agy/gemini-9-flash")
            .unwrap();
        assert!(model.controls_known && !model.effort_suffix);
        let catalogue = crate::Catalogue::bundled()
            .unwrap()
            .with_discovery(std::slice::from_ref(&agy))
            .unwrap();
        assert_eq!(catalogue.quota_pool(&model.model), Some("gemini"));
        assert!(model.efforts.contains(&"high".into()) && model.efforts.contains(&"low".into()));
        let cursor = parse_text_models(
            "cursor",
            "Available models\n\ngrok-9-high - High\ngrok-9-low - Low\n",
            1,
        )
        .unwrap();
        let model = cursor
            .models
            .iter()
            .find(|row| row.model.as_str() == "cursor/grok-9")
            .unwrap();
        assert!(model.effort_suffix && model.efforts.contains(&"high".into()));
        assert!(
            cursor
                .models
                .iter()
                .any(|row| row.model.as_str() == "cursor/grok-9-high")
        );
    }

    // Spec: docs/specs/routing.md#provider-model-discovery
    #[test]
    fn family_fallback_tracks_the_effective_catalogue() {
        let input = br#"{"models":[{"slug":"gpt-6.2-luna","supported_reasoning_levels":[{"effort":"high"}],"service_tiers":[]}]}"#;
        let catalogue = crate::Catalogue::bundled()
            .unwrap()
            .with_discovery(&[parse_codex_models(input, 123).unwrap()])
            .unwrap();
        let config = crate::RoutingConfig::default();
        let model = ModelId::parse("agy/gemini-3.8-flash").unwrap();
        let now = provenant_core::WallTime::from_unix_seconds(1).unwrap();
        let policy = config.model_policy.effective(&catalogue, &model, None, now);
        assert_eq!(
            policy.policy.result.fallback.unwrap().as_str(),
            "codex/gpt-6.2-luna"
        );
    }

    // Spec: docs/specs/routing.md#provider-model-discovery
    #[test]
    fn codex_listing_preserves_advertised_controls_and_rejects_bad_snapshots() {
        let input = br#"{"models":[{"slug":"future-sol","supported_reasoning_levels":[{"effort":"high"}],"service_tiers":[{"id":"priority"}]}]}"#;
        let snapshot = parse_codex_models(input, 123).unwrap();
        assert_eq!(snapshot.observed_at, 123);
        assert_eq!(snapshot.models[0].model.as_str(), "codex/future-sol");
        assert_eq!(snapshot.models[0].efforts, ["high"]);
        assert_eq!(snapshot.models[0].service_tiers, ["priority"]);
        assert!(parse_codex_models(br#"{"models":[]}"#, 123).is_err());
        assert!(parse_codex_models(br#"{"models":[{"slug":"bad id"}]}"#, 123).is_err());
    }
}
