//! Observe-only unattended PR service. Intake owns authoritative forge reads;
//! this owner produces durable decisions without issuing effects.
pub mod observer;
pub mod settings;
pub mod supervisor;
