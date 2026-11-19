//! The memoire gateway.
//!
//! [`MemoireModule`] is a Merezha module: the host mounts its routes at
//! `/v1/memoire` (`memoire_api::PREFIX`) behind identity's authenticator,
//! registers its error codes and asks it for readiness. The binary starts
//! the host with Merezha's blocks (identity included) and this module.
pub mod config;
pub mod module;
mod routes;

pub use config::Settings;
pub use module::MemoireModule;
