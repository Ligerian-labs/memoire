//! `GET /v1/memoire/whoami`.
use serde::{Deserialize, Serialize};

/// What kind of credential authenticated the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// A member's session (cookie or bearer) or a member's own API key.
    User,
    /// An API key, including a machine's (service account) key.
    ApiKey,
}

/// The caller as the gateway sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Whoami {
    /// `usr_…` for a member, `key_…` for an API key.
    pub subject: String,
    pub kind: PrincipalKind,
    /// The active team (`org_…`), `null` when a session has none selected.
    pub team_id: Option<String>,
}
