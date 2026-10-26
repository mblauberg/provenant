//! Pure usage and API-equivalent cost reduction over terminal attempt facts,
//! and the local demand-counter log (`usage`), written through the
//! supervisor's safe filesystem API.
#![forbid(unsafe_code)]

mod prices;
mod reducer;
mod trace;
pub mod usage;
pub use prices::{
    BillingKind, Categories, ConfiguredModel, ConfiguredPrices, PriceError, PriceIdentity,
    PriceTable, Rates, validate_prices,
};
pub use reducer::*;
pub use trace::trace;
