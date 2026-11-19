//! The `memoire-gateway` binary.
//!
//! ```text
//! memoire-gateway run --config PATH [--listen ADDRESS] [--set SECTION.KEY=VALUE]...
//! memoire-gateway build [--manifest PATH] [--workspace PATH]
//! memoire-gateway version
//! ```
//!
//! `run` starts the Merezha host with its blocks (identity included) and the
//! memoire module. `build` is Merezha's guest build at the pinned revision:
//! it compiles the guests of a runtime manifest into components.
use memoire_gateway::{MemoireModule, Wiring};
use merezha_platform::{ConfigInput, ProjectConfig};
use std::{path::PathBuf, process::ExitCode};

const USAGE: &str = "usage: memoire-gateway run --config PATH [--listen ADDRESS] [--set SECTION.KEY=VALUE]...\n\
       memoire-gateway build [--manifest PATH] [--workspace PATH]\n\
       memoire-gateway version";

fn main() -> ExitCode {
    // SAFETY: no other thread exists yet; the runtime is built below.
    let ignored = unsafe { merezha_telemetry::pipeline::scrub_otel_environment() };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut args = std::env::args().skip(1);
    let command = args.next();
    match command.as_deref() {
        Some("build") => return merezha_cli::guest::build_main(args),
        Some("version") => {
            println!(
                "{}",
                serde_json::json!({"name": "memoire-gateway", "version": env!("CARGO_PKG_VERSION")})
            );
            return ExitCode::SUCCESS;
        }
        Some("run") => {}
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let Ok(arguments) = merezha_cli::run::arguments(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return ExitCode::from(2);
    };
    runtime.block_on(run(arguments.config, arguments.overrides, ignored))
}

async fn run(config: PathBuf, overrides: Vec<(String, String)>, ignored: Vec<String>) -> ExitCode {
    let input = ConfigInput {
        path: config,
        env: std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect(),
        overrides,
    };
    let (mut modules, identity) = merezha_cli::host_blocks(&input);
    modules.push(Box::new(match wiring(&input) {
        Some(wiring) => MemoireModule::with_catalog(wiring),
        // Startup then fails on the configuration's own problems, or
        // `memoire_application_missing` without `[application]`.
        None => MemoireModule::new(),
    }));
    merezha_cli::run::run_with(
        input,
        modules,
        Some(identity),
        merezha_platform::host::shutdown_signal(),
        ignored,
    )
    .await
}

/// The runtime manifest and the application document the project names,
/// for the catalog the memoire module opens beside the host's application.
fn wiring(input: &ConfigInput) -> Option<Wiring> {
    let project = ProjectConfig::load(input).ok()?;
    let application = merezha_cli::read_application_config(&project.application?.path).ok()?;
    Some(Wiring {
        manifest: project.runtime.manifest,
        application,
    })
}
