//! `hatchery doctor [--provider ID]`: environment facts, then one real request.
//!
//! The environment half reuses the daemon's audit vocabulary as a report instead of a
//! refusal. The provider probe is the measured half (llm.md §7): a real round whose findings
//! go to the worklog and calibrate the capability table.

use hatchery_daemon::config::{LayeredConfig, LoadPaths};
use hatchery_daemon::discover::StateDir;
use hatchery_daemon::doctor;

use crate::args::DoctorArgs;

/// Runs the doctor; returns the process exit code (1 when a check failed or the probe failed).
pub async fn run(args: &DoctorArgs) -> i32 {
    println!("hatchery doctor");
    let workspace = std::env::current_dir().ok();

    // Configuration first: a parse error is itself a finding, not a crash.
    let paths = LoadPaths::detect(workspace.as_deref());
    let config = match LayeredConfig::load(&paths) {
        Ok(config) => {
            println!("ok      configuration loaded");
            config
        }
        Err(error) => {
            println!("MISSING configuration — {error}");
            return 1;
        }
    };
    let state = args
        .state_dir
        .as_deref()
        .map_or_else(StateDir::standard, StateDir::at);
    let data_dir = config_data_dir();

    let mut failed = false;
    for check in doctor::environment_checks(&config, &state, &data_dir) {
        if !check.ok {
            failed = true;
        }
        println!("{check}");
    }

    if let Some(provider_id) = &args.provider {
        let Some(provider_config) = config.providers().get(provider_id).cloned() else {
            println!(
                "MISSING provider `{provider_id}` is not configured; known: {}",
                config
                    .providers()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return 1;
        };
        let model = args
            .model
            .clone()
            .or_else(|| provider_config.models.first().cloned());
        let Some(model) = model else {
            println!("MISSING provider `{provider_id}` lists no models");
            return 1;
        };
        println!("probing {provider_id}/{model} (one real request, up to 30s)…");
        let report = doctor::probe_provider(provider_id, &provider_config, &model).await;
        failed |= !report.ok();
        println!("{report}");
        println!("(record this in the worklog: reasoning seen, finish reason, usage)");
    }

    if failed { 1 } else { 0 }
}

/// The data directory the daemon would use, for the writability check.
fn config_data_dir() -> std::path::PathBuf {
    hatchery_daemon::entry::default_data_dir()
}
