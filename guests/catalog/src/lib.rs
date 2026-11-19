//! `memoire-catalog`: decides which databases a team has.
//!
//! One stream per team, `{team}:Catalog/{team}`, holds every entry, so name
//! uniqueness and the 50-database quota are one transaction. The guest
//! speaks application protocol 2 (the host supplies the principal and
//! tenant) and imports nothing: no file, network or database handle.
//!
//! | Command | Emits | Refusals |
//! | --- | --- | --- |
//! | `CreateDatabase` `{id, name, kind, idle_ttl_seconds?}` | `DatabaseCreated` | `database_exists`, `database_limit` |
//! | `ExtendDatabase` `{name, idle_ttl_seconds?}` | `DatabaseExtended` | `database_not_found`, `not_burstable`, `database_expired` |
//! | `DeleteDatabase` `{name, id?}` | `DatabaseDeleted` | `database_not_found` |
//! | `ExpireDatabase` `{name, id, now_unix_ms, last_used_unix_ms?}`, system only | `DatabaseExpired`, or nothing when already expired | `unauthorized`, `database_not_found`, `not_burstable`, `not_expired` |
//!
//! The `databases` projection, partitioned by team, folds the same events
//! with the same code ([`catalog::Catalog::apply`]) and answers the queries
//! `ListDatabases` `{}` (every entry, by name) and `GetDatabase` `{name}`
//! (`database_not_found` otherwise).
pub mod catalog;

use catalog::{Catalog, Command, History, check_stream, typed, valid_name};
use merezha_domain::{Failure, MessageKind, Operation, Reply};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use catalog::COMMANDS;

/// The projection this guest maintains.
pub const PROJECTION: &str = "databases";
/// The queries it answers.
pub const QUERIES: &[&str] = &["ListDatabases", "GetDatabase"];

/// `ListDatabases`: the empty object.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDatabases {}

/// `GetDatabase`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetDatabase {
    name: String,
}

/// Answers one protocol operation.
///
/// # Errors
///
/// The command refusals in the table above, `validation_failed` for a bad
/// payload, `unknown_command`, `unknown_query` and `unknown_projection` for
/// names this guest does not own, and `unsupported_operation` for `react`.
pub fn dispatch(operation: Operation) -> Result<Reply, Failure> {
    match operation {
        Operation::Validate {
            kind: MessageKind::Command,
            tag,
            payload,
        } => Ok(Reply::Validated {
            payload: Command::decode(tag.as_str(), payload)?.payload()?,
        }),
        Operation::Validate {
            kind: MessageKind::Query,
            tag,
            payload,
        } => Ok(Reply::Validated {
            payload: decode_query(tag.as_str(), payload)?,
        }),
        Operation::Decide {
            tag,
            stream,
            payload,
            history,
            metadata,
        } => {
            let principal = metadata.principal();
            check_stream(stream.as_str(), principal)?;
            let command = Command::decode(tag.as_str(), payload)?;
            let events = History::rehydrate(&history)?.decide(principal, command)?;
            Ok(Reply::Decided {
                events: events
                    .iter()
                    .map(catalog::Event::encode)
                    .collect::<Result<_, _>>()?,
            })
        }
        Operation::Project {
            projection,
            state,
            events,
            ..
        } => {
            if projection.as_str() != PROJECTION {
                return Err(Failure::permanent("unknown_projection"));
            }
            let catalog: Catalog = typed(state).map_err(|_| invalid_state())?;
            let catalog = catalog.fold(&events)?;
            Ok(Reply::Projected {
                state: serde_json::to_value(catalog).map_err(|_| invalid_state())?,
            })
        }
        Operation::Query {
            tag,
            payload,
            state,
            ..
        } => {
            let catalog: Catalog = typed(state).map_err(|_| invalid_state())?;
            let value = match tag.as_str() {
                "ListDatabases" => {
                    let _: ListDatabases = typed(payload)?;
                    json!({"databases": catalog.databases.values().collect::<Vec<_>>()})
                }
                "GetDatabase" => {
                    let get: GetDatabase = typed(payload)?;
                    let entry = catalog
                        .databases
                        .get(&get.name)
                        .ok_or_else(|| Failure::permanent("database_not_found"))?;
                    serde_json::to_value(entry).map_err(|_| invalid_state())?
                }
                _ => return Err(Failure::permanent("unknown_query")),
            };
            Ok(Reply::Queried { value })
        }
        // `describe` never reaches a dispatcher: the SDK answers it.
        Operation::Describe {} | Operation::React { .. } => {
            Err(Failure::permanent("unsupported_operation"))
        }
    }
}

fn decode_query(tag: &str, payload: Value) -> Result<Value, Failure> {
    let encode = |v: Result<Value, serde_json::Error>| {
        v.map_err(|_| Failure::permanent("serialization_failed"))
    };
    match tag {
        "ListDatabases" => encode(serde_json::to_value(typed::<ListDatabases>(payload)?)),
        "GetDatabase" => {
            let get: GetDatabase = typed(payload)?;
            if !valid_name(&get.name) {
                return Err(Failure::permanent("validation_failed"));
            }
            encode(serde_json::to_value(get))
        }
        _ => Err(Failure::permanent("unknown_query")),
    }
}

fn invalid_state() -> Failure {
    Failure::permanent("invalid_state")
}

/// The `handle` export: one version 1 or 2 request in, one response out in
/// the request's version. See [`merezha_domain::handle`].
///
/// # Errors
///
/// Only when the response cannot be encoded within the protocol's limit.
pub fn handle(input: &str) -> Result<String, String> {
    merezha_domain::handle(input, dispatch)
}

#[cfg(target_arch = "wasm32")]
mod component {
    // Merezha's canonical service world, which the componentizer checks.
    wit_bindgen::generate!({
        inline: "
            package merezha:service@0.1.0;

            interface handler {
                handle: func(input: string) -> result<string, string>;
            }

            world service {
                export handler;
            }
        ",
        world: "service",
    });

    struct Component;

    impl exports::merezha::service::handler::Guest for Component {
        fn handle(input: String) -> Result<String, String> {
            super::handle(&input)
        }
    }

    export!(Component);
}
