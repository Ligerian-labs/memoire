//! The `memoire` Merezha module.
use crate::{
    catalog::{Catalog, Wiring},
    config::Settings,
    data::DataPlane,
    last_used::{self, LastUse},
    routes,
};
use axum::Router;
use merezha_domain::Failure;
use merezha_platform::{
    AppContext, BoxFuture, ConfigIssue, ErrorCode, Migration, Module, ModuleSection,
};
use std::{
    sync::{Arc, LazyLock, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// The module name: the configuration section `[modules.memoire]` and the
/// route prefix `/v1/memoire`. Never changes.
pub const NAME: &str = "memoire";

/// Codes no client meets through memoire's routes in normal operation:
/// `not_expired` refuses a sweep that came too early, and
/// `database_unavailable` a database whose file is not on disk.
const INTERNAL: &[ErrorCode] = &[
    ErrorCode::new("not_expired", 409),
    ErrorCode::new("database_unavailable", 503),
];

/// The statement lists the module runs.
const STATEMENTS: &[&[merezha_postgres::Statement]] = &[last_used::STATEMENTS];

/// memoire's error codes (docs/api.md, "Errors") in Merezha's registry
/// form, and the internal ones.
static ERRORS: LazyLock<Vec<ErrorCode>> = LazyLock::new(|| {
    memoire_api::error::CODES
        .iter()
        .map(|c| ErrorCode::new(c.code, c.status))
        .chain(INTERNAL.iter().cloned())
        .collect()
});

/// Its start loads the guests and connects the catalog to the store.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// The module. It owns the data directory; the catalog guest owns which
/// databases exist.
#[derive(Default)]
pub struct MemoireModule {
    settings: Option<Arc<Settings>>,
    wiring: Option<Wiring>,
    catalog: OnceLock<Catalog>,
    data: OnceLock<Arc<DataPlane>>,
    last_used: Arc<LastUse>,
    database: OnceLock<merezha_platform::Database>,
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl MemoireModule {
    /// A module without the catalog: its start fails
    /// `memoire_application_missing`. For configuration checks.
    pub fn new() -> Self {
        Self::default()
    }

    /// The module with the runtime manifest and application document the
    /// project names.
    pub fn with_catalog(wiring: Wiring) -> Self {
        Self {
            wiring: Some(wiring),
            ..Self::default()
        }
    }

    fn settings(&self) -> Result<&Arc<Settings>, Failure> {
        self.settings
            .as_ref()
            .ok_or_else(|| Failure::permanent("memoire_not_configured"))
    }
}

impl Module for MemoireModule {
    fn name(&self) -> &'static str {
        NAME
    }

    fn configure(&mut self, section: &ModuleSection) -> Result<(), Vec<ConfigIssue>> {
        self.settings = Some(Arc::new(Settings::parse(section)?));
        Ok(())
    }

    fn errors(&self) -> &[ErrorCode] {
        &ERRORS
    }

    fn migrations(&self) -> &[Migration] {
        last_used::MIGRATIONS
    }

    fn statements(&self) -> &[&'static [merezha_postgres::Statement]] {
        STATEMENTS
    }

    fn start_timeout(&self) -> Option<Duration> {
        Some(START_TIMEOUT)
    }

    fn start(&self, ctx: AppContext) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            let teams = self.settings()?.teams_dir();
            create_private_dir(&teams)
                .await
                .map_err(|_| Failure::permanent("data_dir_unavailable"))?;
            let wiring = self
                .wiring
                .as_ref()
                .ok_or_else(|| Failure::permanent("memoire_application_missing"))?;
            let catalog = Catalog::open(&ctx, wiring).await?;
            let _ = self.catalog.set(catalog);
            let settings = self.settings()?;
            let data = Arc::new(DataPlane::new(
                teams,
                settings.max_database_bytes,
                Duration::from_secs(settings.idle_close_seconds),
            ));
            let _ = self.data.set(data.clone());
            let _ = self.database.set(ctx.database().clone());
            let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
            let every = data.idle_check();
            tasks.push(
                tokio::spawn(async move {
                    let mut tick = tokio::time::interval(every);
                    loop {
                        tick.tick().await;
                        data.close_idle();
                    }
                })
                .abort_handle(),
            );
            let (uses, db) = (self.last_used.clone(), ctx.database().clone());
            let every = Duration::from_secs(settings.last_used_flush_seconds);
            tasks.push(
                tokio::spawn(async move {
                    let mut tick = tokio::time::interval(every);
                    loop {
                        tick.tick().await;
                        if uses.flush(&db).await.is_err() {
                            merezha_telemetry::event!(WARN, "memoire.last_used_flush_failed");
                        }
                    }
                })
                .abort_handle(),
            );
            Ok(())
        })
    }

    /// Stops the background work and writes the last uses not flushed yet.
    fn shutdown(&self, deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            for task in self
                .tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .drain(..)
            {
                task.abort();
            }
            if let Some(db) = self.database.get() {
                let _ = tokio::time::timeout_at(deadline.into(), self.last_used.flush(db)).await;
            }
        })
    }

    fn routes(&self, ctx: &AppContext) -> Router {
        match (self.catalog.get(), self.data.get(), self.settings()) {
            (Some(catalog), Some(data), Ok(settings)) => routes::router(routes::Routes {
                catalog: catalog.clone(),
                data: data.clone(),
                last_used: self.last_used.clone(),
                database: ctx.database().clone(),
                policy: ctx.authorizer().clone(),
                settings: settings.clone(),
            }),
            _ => Router::new(),
        }
    }

    fn ready(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            let teams = self.settings()?.teams_dir();
            match tokio::fs::metadata(&teams).await {
                Ok(meta) if meta.is_dir() => Ok(()),
                _ => Err(Failure::transient("data_dir_unavailable")),
            }
        })
    }
}

/// Creates `path` and its parents; the leaf is mode 0700 on Unix.
async fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_every_contract_code() {
        let module = MemoireModule::new();
        assert_eq!(
            module.errors().len(),
            memoire_api::error::CODES.len() + INTERNAL.len()
        );
        assert!(
            module
                .errors()
                .iter()
                .any(|c| c.code == "database_exists" && c.status == 409)
        );
    }

    #[test]
    fn configure_requires_data_dir_and_rejects_unknown_keys() {
        let mut module = MemoireModule::new();
        let mut section = ModuleSection::empty(NAME);
        section.table.insert("data_dir".into(), toml_string("data"));
        section.table.insert("extra".into(), toml_string("x"));
        let issues = module.configure(&section).unwrap_err();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "modules.memoire.extra");
        assert_eq!(issues[0].code, "unknown_key");

        let issues = module.configure(&ModuleSection::empty(NAME)).unwrap_err();
        assert_eq!(issues[0].key, "modules.memoire.data_dir");
        assert_eq!(issues[0].code, "required");

        section.table.remove("extra");
        module.configure(&section).unwrap();
        assert!(
            module
                .settings()
                .unwrap()
                .teams_dir()
                .ends_with("data/teams")
        );
    }

    fn toml_string(s: &str) -> toml::Value {
        toml::Value::String(s.into())
    }
}
