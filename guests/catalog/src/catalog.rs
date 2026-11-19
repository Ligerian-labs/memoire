//! The catalog of one team: its entries, the commands that change them and
//! the events that record each change.
//!
//! The decider and the `databases` projection fold the same events with
//! [`Catalog::apply`], so the read model can never disagree with the rules.
//! Times come from the host: an event's time is its acceptance time
//! (`accepted_unix_ms`), and `ExpireDatabase` carries `now` and the data
//! plane's last use in its payload, because guests are deterministic.
use merezha_domain::{Failure, Principal, PrincipalKind, StoredEvent};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Databases a team may hold in state `active`.
pub const MAX_ACTIVE: usize = 50;
/// Expired entries a team keeps listed; older ones are dropped.
pub const MAX_EXPIRED: usize = 50;
/// The default idle TTL of a burstable database, 24 hours.
pub const DEFAULT_IDLE_TTL_SECONDS: u64 = 24 * 60 * 60;
/// The shortest idle TTL the catalog accepts. The gateway enforces its own
/// configured floor (60 seconds by default) above it; one second lets tests
/// and deploy proofs watch an expiry happen.
pub const MIN_IDLE_TTL_SECONDS: u64 = 1;
/// The longest idle TTL, and the absolute lifetime of a burstable database:
/// 7 days.
pub const MAX_LIFETIME_SECONDS: u64 = 7 * 24 * 60 * 60;
/// Every event this guest writes has this schema version.
pub const SCHEMA_VERSION: u16 = 1;
/// The aggregate the host composes the team's stream from:
/// `{team}:Catalog/{team}`.
pub const AGGREGATE: &str = "Catalog";

/// `burstable` or `persistent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Burstable,
    Persistent,
}

/// An entry's lifecycle state. Deleted entries leave the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Active,
    Expired,
}

/// One database of the team, as the projection serves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub state: State,
    /// Acceptance time of `DatabaseCreated`.
    pub created_unix_ms: u64,
    /// Burstable only.
    pub idle_ttl_seconds: Option<u64>,
    /// Acceptance time of the last `DatabaseExtended`.
    pub extended_unix_ms: Option<u64>,
    /// Acceptance time of `DatabaseExpired`.
    pub expired_unix_ms: Option<u64>,
}

impl Entry {
    /// When a burstable entry expires given the data plane's last use:
    /// `min(max(created, extended, last_used) + idle TTL, created + 7 days)`.
    /// `None` for a persistent entry.
    pub fn expires_unix_ms(&self, last_used_unix_ms: Option<u64>) -> Option<u64> {
        let ttl = self.idle_ttl_seconds?;
        let anchor = self
            .created_unix_ms
            .max(self.extended_unix_ms.unwrap_or(0))
            .max(last_used_unix_ms.unwrap_or(0));
        Some(
            anchor
                .saturating_add(ttl.saturating_mul(1000))
                .min(self.expires_max_unix_ms()),
        )
    }

    /// `created + 7 days`, which never moves.
    pub fn expires_max_unix_ms(&self) -> u64 {
        self.created_unix_ms
            .saturating_add(MAX_LIFETIME_SECONDS * 1000)
    }
}

// ----- commands -----

/// `CreateDatabase`. `id` is minted by the gateway; the catalog checks its
/// shape and that the team never used it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDatabase {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Burstable only; canonicalized to the 24-hour default when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ttl_seconds: Option<u64>,
}

/// `ExtendDatabase`: restart the idle clock, optionally with a new TTL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtendDatabase {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ttl_seconds: Option<u64>,
}

/// `DeleteDatabase`, active or expired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteDatabase {
    pub name: String,
}

/// `ExpireDatabase`, system principals only. The host supplies `now` and
/// the data plane's last use; the catalog checks the rule against them.
/// `id` pins the entry the sweeper looked at, so a database created again
/// under the same name is never expired by a stale decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpireDatabase {
    pub name: String,
    pub id: String,
    pub now_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_unix_ms: Option<u64>,
}

/// A validated catalog command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Create(CreateDatabase),
    Extend(ExtendDatabase),
    Delete(DeleteDatabase),
    Expire(ExpireDatabase),
}

/// The command tags, in the order of [`Command`].
pub const COMMANDS: &[&str] = &[
    "CreateDatabase",
    "ExtendDatabase",
    "DeleteDatabase",
    "ExpireDatabase",
];

impl Command {
    /// Decodes and checks the payload of command `tag`, canonicalizing it:
    /// a burstable create without a TTL gets the default.
    ///
    /// # Errors
    ///
    /// `unknown_command` for a tag outside [`COMMANDS`], and
    /// `validation_failed` for a payload of the wrong shape (unknown fields
    /// included), an invalid name or id, a TTL out of range, or a TTL on a
    /// persistent database.
    pub fn decode(tag: &str, payload: Value) -> Result<Self, Failure> {
        let command = match tag {
            "CreateDatabase" => {
                let mut create: CreateDatabase = typed(payload)?;
                check_name(&create.name)?;
                check_id(&create.id)?;
                create.idle_ttl_seconds = match (create.kind, create.idle_ttl_seconds) {
                    (Kind::Persistent, None) => None,
                    (Kind::Persistent, Some(_)) => return Err(invalid()),
                    (Kind::Burstable, ttl) => {
                        Some(check_ttl(ttl.unwrap_or(DEFAULT_IDLE_TTL_SECONDS))?)
                    }
                };
                Self::Create(create)
            }
            "ExtendDatabase" => {
                let extend: ExtendDatabase = typed(payload)?;
                check_name(&extend.name)?;
                if let Some(ttl) = extend.idle_ttl_seconds {
                    check_ttl(ttl)?;
                }
                Self::Extend(extend)
            }
            "DeleteDatabase" => {
                let delete: DeleteDatabase = typed(payload)?;
                check_name(&delete.name)?;
                Self::Delete(delete)
            }
            "ExpireDatabase" => {
                let expire: ExpireDatabase = typed(payload)?;
                check_name(&expire.name)?;
                check_id(&expire.id)?;
                Self::Expire(expire)
            }
            _ => return Err(Failure::permanent("unknown_command")),
        };
        Ok(command)
    }

    /// The canonical payload.
    pub fn payload(&self) -> Result<Value, Failure> {
        match self {
            Self::Create(c) => serde_json::to_value(c),
            Self::Extend(c) => serde_json::to_value(c),
            Self::Delete(c) => serde_json::to_value(c),
            Self::Expire(c) => serde_json::to_value(c),
        }
        .map_err(|_| Failure::permanent("serialization_failed"))
    }
}

// ----- events -----

/// `DatabaseCreated`: a new entry, active. Created at its acceptance time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseCreated {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub idle_ttl_seconds: Option<u64>,
}

/// `DatabaseExtended`: the idle clock restarted at the acceptance time,
/// with this TTL from then on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseExtended {
    pub id: String,
    pub name: String,
    pub idle_ttl_seconds: u64,
}

/// `DatabaseDeleted`: the entry left the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseDeleted {
    pub id: String,
    pub name: String,
}

/// Why a burstable database expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryReason {
    /// Its idle TTL passed.
    Idle,
    /// Its 7-day lifetime passed.
    Lifetime,
}

/// `DatabaseExpired`: the entry stays listed as expired; its file goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseExpired {
    pub id: String,
    pub name: String,
    pub reason: ExpiryReason,
}

/// A catalog event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Created(DatabaseCreated),
    Extended(DatabaseExtended),
    Deleted(DatabaseDeleted),
    Expired(DatabaseExpired),
}

impl Event {
    /// The stored tag.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Created(_) => "DatabaseCreated",
            Self::Extended(_) => "DatabaseExtended",
            Self::Deleted(_) => "DatabaseDeleted",
            Self::Expired(_) => "DatabaseExpired",
        }
    }

    /// The event the host stores.
    pub fn encode(&self) -> Result<merezha_domain::Event, Failure> {
        let payload = match self {
            Self::Created(e) => serde_json::to_value(e),
            Self::Extended(e) => serde_json::to_value(e),
            Self::Deleted(e) => serde_json::to_value(e),
            Self::Expired(e) => serde_json::to_value(e),
        }
        .map_err(|_| Failure::permanent("serialization_failed"))?;
        Ok(merezha_domain::Event {
            tag: merezha_domain::Identity::new(self.tag())?,
            schema_version: SCHEMA_VERSION,
            payload,
        })
    }

    /// A stored event back into its typed form.
    ///
    /// # Errors
    ///
    /// `unknown_event` for a tag or schema version this guest never wrote,
    /// `invalid_event` for a payload that does not decode.
    pub fn decode(stored: &merezha_domain::Event) -> Result<Self, Failure> {
        if stored.schema_version != SCHEMA_VERSION {
            return Err(Failure::permanent("unknown_event"));
        }
        let payload = stored.payload.clone();
        let event =
            |r: Result<Self, serde_json::Error>| r.map_err(|_| Failure::permanent("invalid_event"));
        match stored.tag.as_str() {
            "DatabaseCreated" => event(serde_json::from_value(payload).map(Self::Created)),
            "DatabaseExtended" => event(serde_json::from_value(payload).map(Self::Extended)),
            "DatabaseDeleted" => event(serde_json::from_value(payload).map(Self::Deleted)),
            "DatabaseExpired" => event(serde_json::from_value(payload).map(Self::Expired)),
            _ => Err(Failure::permanent("unknown_event")),
        }
    }
}

// ----- state -----

/// A team's catalog: its entries by name, active and expired. Name
/// uniqueness holds by construction: one entry per name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub databases: BTreeMap<String, Entry>,
}

impl Catalog {
    /// Active entries, the ones the quota counts.
    pub fn active(&self) -> usize {
        self.databases
            .values()
            .filter(|e| e.state == State::Active)
            .count()
    }

    /// Folds one event accepted at `at` (Unix milliseconds).
    ///
    /// # Errors
    ///
    /// `invalid_history` when the event names an entry the catalog does not
    /// hold, which a history this guest wrote never does.
    pub fn apply(&mut self, event: Event, at: u64) -> Result<(), Failure> {
        let corrupt = || Failure::permanent("invalid_history");
        match event {
            Event::Created(e) => {
                // An expired entry of the same name gives way.
                self.databases.insert(
                    e.name.clone(),
                    Entry {
                        id: e.id,
                        name: e.name,
                        kind: e.kind,
                        state: State::Active,
                        created_unix_ms: at,
                        idle_ttl_seconds: e.idle_ttl_seconds,
                        extended_unix_ms: None,
                        expired_unix_ms: None,
                    },
                );
            }
            Event::Extended(e) => {
                let entry = self.entry_mut(&e.name, &e.id).ok_or_else(corrupt)?;
                entry.idle_ttl_seconds = Some(e.idle_ttl_seconds);
                entry.extended_unix_ms = Some(at);
            }
            Event::Deleted(e) => {
                self.entry_mut(&e.name, &e.id).ok_or_else(corrupt)?;
                self.databases.remove(&e.name);
            }
            Event::Expired(e) => {
                let entry = self.entry_mut(&e.name, &e.id).ok_or_else(corrupt)?;
                entry.state = State::Expired;
                entry.expired_unix_ms = Some(at);
                self.drop_oldest_expired();
            }
        }
        Ok(())
    }

    /// Folds stored events in order.
    ///
    /// # Errors
    ///
    /// Those of [`Event::decode`] and [`Catalog::apply`].
    pub fn fold<'a>(
        mut self,
        events: impl IntoIterator<Item = &'a StoredEvent>,
    ) -> Result<Self, Failure> {
        for stored in events {
            self.apply(Event::decode(&stored.event)?, stored.accepted_unix_ms)?;
        }
        Ok(self)
    }

    fn entry_mut(&mut self, name: &str, id: &str) -> Option<&mut Entry> {
        self.databases.get_mut(name).filter(|e| e.id == id)
    }

    /// Keeps at most [`MAX_EXPIRED`] expired entries: the oldest expiries
    /// go first, ties by name.
    fn drop_oldest_expired(&mut self) {
        let mut expired: Vec<(u64, String)> = self
            .databases
            .values()
            .filter(|e| e.state == State::Expired)
            .map(|e| (e.expired_unix_ms.unwrap_or(0), e.name.clone()))
            .collect();
        if expired.len() <= MAX_EXPIRED {
            return;
        }
        expired.sort();
        for (_, name) in expired.drain(..expired.len() - MAX_EXPIRED) {
            self.databases.remove(&name);
        }
    }
}

/// The decider's view of a stream: the catalog and every id the team ever
/// used, so an id is never reused, even after a delete.
#[derive(Clone, Debug, Default)]
pub struct History {
    pub catalog: Catalog,
    pub ids: BTreeSet<String>,
}

impl History {
    /// Rehydrates a stream.
    ///
    /// # Errors
    ///
    /// Those of [`Catalog::fold`].
    pub fn rehydrate(events: &[StoredEvent]) -> Result<Self, Failure> {
        let mut history = Self::default();
        for stored in events {
            let event = Event::decode(&stored.event)?;
            if let Event::Created(created) = &event {
                history.ids.insert(created.id.clone());
            }
            history.catalog.apply(event, stored.accepted_unix_ms)?;
        }
        Ok(history)
    }

    /// Decides `command` for `principal`.
    ///
    /// # Errors
    ///
    /// | Code | When |
    /// | --- | --- |
    /// | `database_exists` | create: an active database holds the name, or the id was used before |
    /// | `database_limit` | create: the team has [`MAX_ACTIVE`] active databases |
    /// | `database_not_found` | extend, delete, expire: no entry of that name (and id) |
    /// | `not_burstable` | extend, expire: a persistent database |
    /// | `database_expired` | extend: the entry already expired |
    /// | `unauthorized` | expire: the principal is not a system principal |
    /// | `not_expired` | expire: neither the idle TTL nor the lifetime passed at `now` |
    pub fn decide(
        &self,
        principal: Option<&Principal>,
        command: Command,
    ) -> Result<Vec<Event>, Failure> {
        let not_found = || Failure::permanent("database_not_found");
        let databases = &self.catalog.databases;
        match command {
            Command::Create(c) => {
                if databases
                    .get(&c.name)
                    .is_some_and(|e| e.state == State::Active)
                    || self.ids.contains(&c.id)
                {
                    return Err(Failure::conflict("database_exists"));
                }
                if self.catalog.active() >= MAX_ACTIVE {
                    return Err(Failure::permanent("database_limit"));
                }
                Ok(vec![Event::Created(DatabaseCreated {
                    id: c.id,
                    name: c.name,
                    kind: c.kind,
                    idle_ttl_seconds: c.idle_ttl_seconds,
                })])
            }
            Command::Extend(c) => {
                let entry = databases.get(&c.name).ok_or_else(not_found)?;
                let current = match (entry.kind, entry.state) {
                    (Kind::Persistent, _) => return Err(Failure::permanent("not_burstable")),
                    (_, State::Expired) => return Err(Failure::permanent("database_expired")),
                    (Kind::Burstable, State::Active) => entry.idle_ttl_seconds,
                };
                let ttl = c
                    .idle_ttl_seconds
                    .or(current)
                    .unwrap_or(DEFAULT_IDLE_TTL_SECONDS);
                Ok(vec![Event::Extended(DatabaseExtended {
                    id: entry.id.clone(),
                    name: c.name,
                    idle_ttl_seconds: ttl,
                })])
            }
            Command::Delete(c) => {
                let entry = databases.get(&c.name).ok_or_else(not_found)?;
                Ok(vec![Event::Deleted(DatabaseDeleted {
                    id: entry.id.clone(),
                    name: c.name,
                })])
            }
            Command::Expire(c) => {
                if !principal.is_some_and(|p| p.kind == PrincipalKind::System) {
                    return Err(Failure::permanent("unauthorized"));
                }
                let entry = databases
                    .get(&c.name)
                    .filter(|e| e.id == c.id)
                    .ok_or_else(not_found)?;
                if entry.kind == Kind::Persistent {
                    return Err(Failure::permanent("not_burstable"));
                }
                if entry.state == State::Expired {
                    // Already decided: a retried sweep changes nothing.
                    return Ok(Vec::new());
                }
                let lifetime = entry.expires_max_unix_ms();
                let deadline = entry
                    .expires_unix_ms(c.last_used_unix_ms)
                    .unwrap_or(lifetime);
                if c.now_unix_ms < deadline {
                    return Err(Failure::conflict("not_expired"));
                }
                let reason = if c.now_unix_ms >= lifetime && deadline == lifetime {
                    ExpiryReason::Lifetime
                } else {
                    ExpiryReason::Idle
                };
                Ok(vec![Event::Expired(DatabaseExpired {
                    id: c.id,
                    name: c.name,
                    reason,
                })])
            }
        }
    }
}

// ----- validation -----

/// `^[a-z][a-z0-9-]{1,62}$`.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (2..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `db_` and 32 lowercase hex digits.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("db_").is_some_and(|hex| {
        hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn invalid() -> Failure {
    Failure::permanent("validation_failed")
}

fn check_name(name: &str) -> Result<(), Failure> {
    valid_name(name).then_some(()).ok_or_else(invalid)
}

fn check_id(id: &str) -> Result<(), Failure> {
    valid_id(id).then_some(()).ok_or_else(invalid)
}

fn check_ttl(ttl: u64) -> Result<u64, Failure> {
    (MIN_IDLE_TTL_SECONDS..=MAX_LIFETIME_SECONDS)
        .contains(&ttl)
        .then_some(ttl)
        .ok_or_else(invalid)
}

pub(crate) fn typed<T: DeserializeOwned>(payload: Value) -> Result<T, Failure> {
    serde_json::from_value(payload).map_err(|_| invalid())
}

/// `unauthorized` unless the stream is the principal's team catalog,
/// `{team}:Catalog/{team}`: one stream per team, so the quota and name
/// uniqueness hold in one transaction and no caller can open a second
/// catalog for its team.
pub fn check_stream(stream: &str, principal: Option<&Principal>) -> Result<(), Failure> {
    let team = principal
        .and_then(|p| p.tenant.as_ref())
        .ok_or_else(|| Failure::permanent("tenant_required"))?;
    let team = team.as_str();
    if stream == format!("{team}:{AGGREGATE}/{team}") {
        Ok(())
    } else {
        Err(Failure::permanent("unauthorized"))
    }
}
