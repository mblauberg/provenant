//! Pure usage and API-equivalent cost reduction over terminal attempt facts,
//! and the local demand-counter log (`usage`), written through the
//! OS crate's held filesystem handles.
#![forbid(unsafe_code)]

mod native;
mod prices;
mod reducer;
mod trace;
pub mod usage;
pub use native::{NativeModel, NativeReport, claude_subagents};
pub use prices::{
    BillingKind, Categories, ConfiguredModel, ConfiguredPrices, PriceError, PriceIdentity,
    PriceTable, Rates, validate_prices,
};
pub use reducer::*;
pub use trace::{Hosts, trace};
