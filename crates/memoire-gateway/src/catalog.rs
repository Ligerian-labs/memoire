//! The catalog guest, called in process.
//!
//! The host's `application` module owns the application on PostgreSQL: it
//! applies the `merezha_app` migrations, runs the projections in the
//! background and serves `/v1/application/…`. This module opens a second
//! [`Application`] over the same store and configuration, as a second host
//! would, so memoire's own routes can run catalog commands and queries as
//! the caller the edge authenticated without an HTTP hop. Its queries wait
//! for the projection with `min_position`, which polls the shared store.
use merezha_application::{
    AllowAll, Application, Caller,
    config::Config,
    postgres::{BackgroundOptions, CatchUp, PostgresOptions, PostgresStore},
};
use merezha_domain::{Command, Failure, Identity, Metadata, Query, Receipt};
use merezha_platform::AppContext;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// The catalog's aggregate; a team's stream is `{team}:Catalog/{team}`.
pub const AGGREGATE: &str = "Catalog";
/// Attempts of a command whose stream moved between its decision and its
/// append (`concurrency_conflict`): another command of the same team won.
/// Each waits a random 1 to `10 * attempt` milliseconds first, so racing
/// writers of one team spread out.
const ATTEMPTS: u64 = 10;
/// Time the store's connect may take inside the module's start budget.
const CONNECT_BUDGET: Duration = Duration::from_secs(8);

/// What the binary hands the module so it can open the catalog: the runtime
/// manifest and the application document `[runtime]` and `[application]`
/// name.
#[derive(Clone, Debug)]
pub struct Wiring {
    pub manifest: PathBuf,
    pub application: Config,
}

/// The catalog application, cheap to clone.
#[derive(Clone)]
pub struct Catalog {
    application: Application,
}

impl Catalog {
    /// Loads the guests and connects to the store the `application` module
    /// migrated. Background runs stay with that module: this instance only
    /// commands and queries.
    ///
    /// # Errors
    ///
    /// `invalid_manifest` when the runtime does not load, and the store's
    /// own codes (`storage_schema`, `storage_configuration`, …).
    pub async fn open(ctx: &AppContext, wiring: &Wiring) -> Result<Self, Failure> {
        let manifest = wiring.manifest.clone();
        let meters = ctx.telemetry().clone();
        let runtime = merezha_telemetry::spawn_blocking_in_span(move || {
            merezha_runtime::Runtime::load_with_meters(manifest, &meters)
        })
        .await
        .map_err(|_| Failure::transient("internal"))?
        .map_err(|_| Failure::permanent("invalid_manifest"))?;
        let options = PostgresOptions {
            background: BackgroundOptions {
                catch_up: CatchUp::Manual,
                warm_readiness: false,
                max_concurrent: 1,
                instance_id: format!("{}-memoire", ctx.instance_id()),
                ..BackgroundOptions::default()
            },
            ..PostgresOptions::default()
        };
        // After the guests compiled, which can take seconds.
        let deadline = Instant::now() + CONNECT_BUDGET;
        let store =
            PostgresStore::connect(ctx.database(), &wiring.application, options, deadline).await?;
        let config = wiring.application.clone();
        let application = merezha_telemetry::spawn_blocking_in_span(move || {
            // Route permissions are the policy (checked through the caller
            // before any guest call); the payload hook adds nothing.
            Application::with_postgres(
                config,
                Arc::new(runtime),
                Arc::new(AllowAll),
                Arc::new(store),
            )
        })
        .await
        .map_err(|_| Failure::transient("internal"))??;
        Ok(Self { application })
    }

    /// Runs command `tag` on the caller's team catalog, deciding again when
    /// another command of the team committed in between.
    ///
    /// # Errors
    ///
    /// `tenant_required` without a team, the route's `unauthorized`, the
    /// guest's refusals and the store's failures.
    pub async fn command<C>(&self, caller: C, tag: &str, payload: Value) -> Result<Receipt, Failure>
    where
        C: Caller + Send + 'static,
    {
        let team = caller
            .tenant()
            .ok_or_else(|| Failure::permanent("tenant_required"))?;
        let application = self.application.clone();
        let tag = Identity::new(tag)?;
        blocking(move || {
            let mut attempt = 0;
            loop {
                attempt += 1;
                let command = Command {
                    tag: tag.clone(),
                    stream: team.clone(),
                    payload: payload.clone(),
                    metadata: metadata()?,
                    idempotency_key: None,
                    expected_version: None,
                };
                match application.command_as(&caller, command) {
                    Err(failure)
                        if failure.code == "concurrency_conflict" && attempt < ATTEMPTS =>
                    {
                        std::thread::sleep(Duration::from_millis(1 + jitter() % (10 * attempt)));
                    }
                    result => return result,
                }
            }
        })
        .await
    }

    /// Runs query `tag` on the caller's team partition; with `after`, once
    /// the projection reached that receipt.
    ///
    /// # Errors
    ///
    /// `tenant_required`, `unauthorized`, the guest's refusals,
    /// `projection_behind` when the projection did not catch up in time.
    pub async fn query<C>(
        &self,
        caller: C,
        tag: &str,
        payload: Value,
        after: Option<&Receipt>,
    ) -> Result<Value, Failure>
    where
        C: Caller + Send + 'static,
    {
        let application = self.application.clone();
        let query = Query {
            tag: Identity::new(tag)?,
            payload,
            metadata: metadata()?,
            min_position: after.map(|receipt| receipt.position),
        };
        blocking(move || {
            application
                .query_as(&caller, query)
                .map(|answer| answer.value)
        })
        .await
    }
}

/// The gateway itself acting in one team: the sweeper, the snapshots and
/// the reconciler. A `system` principal (`sys_memoire`) on the team, which
/// may read the catalog and run the routes that need only a principal
/// (`ExpireDatabase`, whose guest accepts system principals only).
pub struct System(merezha_domain::Principal);

impl System {
    pub fn new(team: &str) -> Result<Self, Failure> {
        Ok(Self(merezha_domain::Principal::new(
            Identity::new("sys_memoire")?,
            merezha_domain::PrincipalKind::System,
            Some(Identity::new(team)?),
            Vec::new(),
        )?))
    }
}

impl Caller for System {
    fn principal(&self, _: &merezha_application::Scope) -> Option<merezha_domain::Principal> {
        Some(self.0.clone())
    }
    fn tenant(&self) -> Option<Identity> {
        self.0.tenant.clone()
    }
    fn authenticated(&self) -> bool {
        true
    }
    /// Only reading the catalog: every change it makes goes through a
    /// route that checks no permission and a guest that wants a system
    /// principal.
    fn check(&self, permission: &Identity, _: &merezha_application::Scope) -> Result<(), Failure> {
        if permission.as_str() == crate::routes::permission::READ {
            Ok(())
        } else {
            Err(Failure::permanent("unauthorized"))
        }
    }
}

/// The application blocks on the runtime handle, so every call runs on a
/// blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    merezha_telemetry::spawn_blocking_in_span(work)
        .await
        .map_err(|_| Failure::transient("internal"))?
}

/// Fresh message and correlation ids; the trace id correlates when one is
/// recorded.
fn metadata() -> Result<Metadata, Failure> {
    let correlation = match merezha_telemetry::propagation::StoredContext::current() {
        Some(context) => context.span_context().trace_id().to_string(),
        None => random_hex()?,
    };
    Ok(Metadata {
        message_id: Identity::new(random_hex()?)?,
        correlation_id: Identity::new(correlation)?,
        causation_id: None,
        actor: None,
        principal: None,
    })
}

/// A random number for retry jitter.
fn jitter() -> u64 {
    use merezha_platform::Random as _;
    let mut bytes = [0u8; 8];
    let _ = merezha_platform::OsRandom.fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

/// 128 bits from the operating system's CSPRNG as 32 lowercase hex digits.
pub fn random_hex() -> Result<String, Failure> {
    use merezha_platform::Random as _;
    let mut bytes = [0u8; 16];
    merezha_platform::OsRandom.fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
