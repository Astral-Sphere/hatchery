use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "hatchery-gui: the GTK4 + libadwaita frontend is implemented in M4 (docs/roadmap.md)."
    );
    eprintln!("Until then use the CLI: `cargo run --bin hatchery -- --help`.");
    ExitCode::FAILURE
}
