//! Shared identity, root and path policy. Environment and host observations are values.
#![forbid(unsafe_code)]

pub mod excludes;
pub mod host_identity;
mod json_support;
pub mod jsonc;
pub mod paths;
mod pending;
pub mod product_root_resolver;
pub mod py;
pub mod roots;
#[cfg(test)]
mod test_support;
pub mod workspace_paths;
