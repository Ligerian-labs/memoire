//! The `memoire` command line: arguments and their validation.
//!
//! The commands talk to a gateway over the HTTP API in `docs/api.md`.
//! Secrets never travel in arguments: `login` reads the API key from
//! standard input or `MEMOIRE_API_KEY`.
pub mod cli;
pub mod duration;
