//! A real daemon in a real subprocess, for the one e2e that compares process shapes
//! (docs/design/testing.md open question 4, decided in the D7 record).
//!
//! The spawning test owns this process's whole world through the environment: where the state
//! and data directories live and which provider endpoint to serve. Nothing is read from the
//! user's config — a test binary must not depend on the machine it runs on.

use std::path::PathBuf;

fn main() {
    let required = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} must name this test daemon's {name}"))
    };
    let options = hatchery_daemon::entry::RunOptions {
        state_dir: Some(PathBuf::from(required("HATCHERY_E2E_STATE"))),
        data_dir: Some(PathBuf::from(required("HATCHERY_E2E_DATA"))),
        workspace: std::env::var("HATCHERY_E2E_WORKSPACE")
            .map(PathBuf::from)
            .ok(),
        serve_stdio: false,
        config_layers: Some(vec![(
            hatchery_protocol::method::ConfigOrigin::User,
            toml::from_str(&format!(
                // The built-ins are blanked the way the daemon's own tests do: the audit would
                // otherwise demand this machine's real provider keys for a hermetic daemon.
                "[providers.deepseek]\n\
                 env_key = \"\"\n\n\
                 [providers.qwen]\n\
                 env_key = \"\"\n\n\
                 [providers.testprov]\n\
                 base_url = \"{}\"\n\
                 env_key = \"PATH\"\n\
                 models = [\"m\"]\n",
                required("HATCHERY_E2E_BASE_URL"),
            ))
            .expect("the injected provider layer is valid TOML"),
        )]),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    if let Err(error) = runtime.block_on(hatchery_daemon::entry::run(options)) {
        eprintln!("e2e_daemon: {error}");
        std::process::exit(1);
    }
}
