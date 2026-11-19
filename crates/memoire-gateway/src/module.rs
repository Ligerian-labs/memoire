//! The `memoire` Merezha module.
use crate::{config::Settings, routes};
use axum::Router;
use merezha_domain::Failure;
use merezha_platform::{AppContext, BoxFuture, ConfigIssue, ErrorCode, Module, ModuleSection};
use std::sync::LazyLock;

/// The module name: the configuration section `[modules.memoire]` and the
/// route prefix `/v1/memoire`. Never changes.
pub const NAME: &str = "memoire";

/// memoire's error codes (docs/api.md, "Errors") in Merezha's registry form.
static ERRORS: LazyLock<Vec<ErrorCode>> = LazyLock::new(|| {
    memoire_api::error::CODES
        .iter()
        .map(|c| ErrorCode::new(c.code, c.status))
        .collect()
});

/// The module. It owns the data directory; the catalog guest owns which
/// databases exist.
#[derive(Default)]
pub struct MemoireModule {
    settings: Option<Settings>,
}

impl MemoireModule {
    pub fn new() -> Self {
        Self::default()
    }

    fn settings(&self) -> Result<&Settings, Failure> {
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
        self.settings = Some(Settings::parse(section)?);
        Ok(())
    }

    fn errors(&self) -> &[ErrorCode] {
        &ERRORS
    }

    fn start(&self, _ctx: AppContext) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            let teams = self.settings()?.teams_dir();
            create_private_dir(&teams)
                .await
                .map_err(|_| Failure::permanent("data_dir_unavailable"))
        })
    }

    fn routes(&self, _ctx: &AppContext) -> Router {
        routes::router()
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
        assert_eq!(module.errors().len(), memoire_api::error::CODES.len());
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
