use std::process::ExitCode;

use hatchery_cli::args::{self, Command};

const VERSION: &str = concat!("hatchery ", env!("CARGO_PKG_VERSION"));

#[tokio::main]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let command = match args::parse(&argv) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("hatchery: {message}");
            return ExitCode::from(2);
        }
    };
    let code = match command {
        Command::Help => {
            print!("{}", args::usage());
            0
        }
        Command::Version => {
            println!("{VERSION}");
            0
        }
        Command::Chat(chat_args) => {
            let state = state_dir(chat_args.state_dir.clone());
            hatchery_cli::chat::run(state, chat_args).await
        }
        Command::Exec(exec_args) => {
            let state = state_dir(exec_args.state_dir.clone());
            let mut out = hatchery_cli::exec::StdoutOut;
            hatchery_cli::exec::run(state, &exec_args, &mut out)
                .await
                .exit_code()
        }
        Command::Daemon(action) => hatchery_cli::daemon_cmd::run(&action).await,
        Command::Doctor(doctor_args) => hatchery_cli::doctor_cmd::run(&doctor_args).await,
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn state_dir(override_path: Option<std::path::PathBuf>) -> hatchery_daemon::discover::StateDir {
    override_path.map_or_else(
        hatchery_daemon::discover::StateDir::standard,
        hatchery_daemon::discover::StateDir::at,
    )
}
