//! The memoire gateway.
//!
//! [`MemoireModule`] is a Merezha module: the host mounts its routes at
//! `/v1/memoire` (`memoire_api::PREFIX`) behind identity's authenticator,
//! registers its error codes and asks it for readiness. The binary starts
//! the host with Merezha's blocks (identity included) and this module.
pub mod catalog;
pub mod config;
pub mod data;
mod http;
mod last_used;
mod lifecycle;
pub mod module;
mod routes;
mod store;

pub use catalog::Wiring;
pub use config::Settings;
pub use module::MemoireModule;
