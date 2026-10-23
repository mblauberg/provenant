//! Shared identity, root and path policy. Environment and host observations are values.
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr
)]
#![forbid(unsafe_code)]

pub mod bytes;
pub use bytes::hex;
pub mod excludes;
pub mod host_identity;
pub mod js;
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
