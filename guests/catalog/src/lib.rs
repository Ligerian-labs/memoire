//! `memoire-catalog`: decides which databases a team has.
//!
//! One stream per team, aggregate `Catalog/{team}`, holds every entry, so
//! name uniqueness and the 50-database quota are one transaction. The guest
//! speaks application protocol 2 (the host supplies the principal and
//! tenant) and imports nothing: no file, network or database handle.
//!
//! | Command | Emits |
//! | --- | --- |
//! | `CreateDatabase` | `DatabaseCreated` |
//! | `ExtendDatabase` | `DatabaseExtended` |
//! | `DeleteDatabase` | `DatabaseDeleted` |
//! | `ExpireDatabase` (system only; the host supplies `now` and `last_used_at`) | `DatabaseExpired` |
//!
//! This is the skeleton: it names the commands and answers `describe`, and
//! refuses every command with `not_implemented` until the rules land.
use merezha_domain::{Failure, MessageKind, Operation, Reply};

/// The commands the catalog will decide.
pub const COMMANDS: &[&str] = &[
    "CreateDatabase",
    "ExtendDatabase",
    "DeleteDatabase",
    "ExpireDatabase",
];

/// Answers one protocol operation.
///
/// # Errors
///
/// `unknown_command` for a tag outside [`COMMANDS`], `not_implemented` for
/// one inside it, and `unsupported_operation` for anything but `validate`
/// and `decide`.
pub fn dispatch(operation: Operation) -> Result<Reply, Failure> {
    match operation {
        Operation::Validate {
            kind: MessageKind::Command,
            tag,
            ..
        }
        | Operation::Decide { tag, .. } => {
            if COMMANDS.contains(&tag.as_str()) {
                Err(Failure::permanent("not_implemented"))
            } else {
                Err(Failure::permanent("unknown_command"))
            }
        }
        Operation::Validate { .. } => Err(Failure::permanent("unknown_command")),
        // `describe` never reaches a dispatcher: the SDK answers it.
        Operation::Project { .. }
        | Operation::Query { .. }
        | Operation::Describe {}
        | Operation::React { .. } => Err(Failure::permanent("unsupported_operation")),
    }
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
