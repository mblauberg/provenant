use crate::RoutingError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelId(String);
impl ModelId {
    pub fn parse(text: &str) -> Result<Self, RoutingError> {
        let Some((adapter, id)) = text.split_once('/') else {
            return Err(RoutingError::InvalidModel);
        };
        if text.len() > 512
            || !identifier(adapter)
            || id.is_empty()
            || id
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '#' | ','))
        {
            return Err(RoutingError::InvalidModel);
        }
        Ok(Self(text.into()))
    }
    pub fn adapter(&self) -> &str {
        self.0.split_once('/').map_or("", |(adapter, _)| adapter)
    }
    pub fn model(&self) -> &str {
        self.0.split_once('/').map_or("", |(_, id)| id)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ModelId {
    type Error = RoutingError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}
impl From<ModelId> for String {
    fn from(id: ModelId) -> String {
        id.0
    }
}

pub(crate) fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}
pub(crate) fn effort_valid(text: &str) -> bool {
    identifier(text)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RouteChoice {
    Model(ModelId),
    NamedRoute(String),
}

/// Closed bound is checked on construction and deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<ModelId>", into = "Vec<ModelId>")]
pub struct BoundedModels<const N: usize>(Vec<ModelId>);
impl<const N: usize> BoundedModels<N> {
    pub fn new(models: Vec<ModelId>) -> Result<Self, RoutingError> {
        if models.is_empty()
            || models.len() > N
            || models
                .iter()
                .enumerate()
                .any(|(i, m)| models.iter().skip(i + 1).any(|v| v == m))
        {
            return Err(RoutingError::Alternatives);
        }
        Ok(Self(models))
    }
    pub fn as_slice(&self) -> &[ModelId] {
        &self.0
    }
}
impl<const N: usize> TryFrom<Vec<ModelId>> for BoundedModels<N> {
    type Error = RoutingError;
    fn try_from(models: Vec<ModelId>) -> Result<Self, Self::Error> {
        Self::new(models)
    }
}
impl<const N: usize> From<BoundedModels<N>> for Vec<ModelId> {
    fn from(models: BoundedModels<N>) -> Self {
        models.0
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "models",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Fallback {
    #[default]
    None,
    Models(BoundedModels<2>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRequest {
    pub choice: RouteChoice,
    pub effort: Option<String>,
    #[serde(default)]
    pub confidential: bool,
    #[serde(default)]
    pub fallback: Fallback,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteListQuery {
    pub adapter: Option<String>,
}
