use crate::Profile;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

/// Until domains land, even an empty unknown section fails closed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDomains {}

/// Semantic failures expose a static field identifier and code, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{field}: {code}")]
pub struct SectionError {
    pub field: &'static str,
    pub code: &'static str,
}

/// Registration is immutable data, with no runtime registry or global state.
///
/// `T` is a closed serde struct whose fields are domain section types, all with
/// `deny_unknown_fields`. Domain owners provide one complete TOML default asset
/// per preset and validate their semantics here. User `[domains.<name>]` tables
/// override the selected defaults once, before `T` is constructed and validated.
/// Config owns non-price types; composition supplies the aggregate without a
/// dependency from config to any domain runtime. Secret-bearing fields do not
/// belong in this model: `config_show` serialises its effective values.
///
/// Arrays replace as a whole. Tables merge recursively; all keys, including
/// empty tables, must be accepted by `T`. There are no includes or interpolation.
pub struct DomainSections<T> {
    pub balanced: &'static str,
    pub economy: &'static str,
    pub rigorous: &'static str,
    pub validate: fn(&T) -> Result<(), SectionError>,
}
impl<T: DeserializeOwned + Serialize> DomainSections<T> {
    pub(crate) fn defaults(&self, profile: Profile) -> &'static str {
        match profile {
            Profile::Balanced => self.balanced,
            Profile::Economy => self.economy,
            Profile::Rigorous => self.rigorous,
        }
    }
}
pub(crate) const NO_DOMAINS: DomainSections<NoDomains> = DomainSections {
    balanced: "",
    economy: "",
    rigorous: "",
    validate: |_| Ok(()),
};
