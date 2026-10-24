use std::process::ExitCode;

fn main() -> ExitCode {
    match xtask::run(std::env::args().skip(1)) {
        Ok(report) => {
            if !report.is_empty() {
                println!("{report}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}
