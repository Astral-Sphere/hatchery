use std::process::ExitCode;

const USAGE: &str = "\
hatchery — an open source AI agent harness

USAGE:
    hatchery [OPTIONS]

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print version information

Subcommands (chat, exec, acp, daemon, doctor, config) land in M1 and later;
see docs/roadmap.md.
";

fn main() -> ExitCode {
    let first = std::env::args().nth(1);
    match first.as_deref() {
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") => {
            println!("hatchery {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        other => {
            match other {
                Some(arg) => eprintln!("hatchery: unexpected argument {arg:?}"),
                None => eprintln!("hatchery: no subcommand implemented yet"),
            }
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::USAGE;

    #[test]
    fn usage_names_the_roadmap() {
        assert!(USAGE.contains("docs/roadmap.md"));
    }
}
