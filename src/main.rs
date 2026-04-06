use std::process::ExitCode;

use clap::Parser;
use wperf::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Record(args) => wperf::record::run(&args),
        Command::Report(_args) => {
            eprintln!("wperf report: not yet implemented (planned for W3 #17)");
            return ExitCode::FAILURE;
        }
        Command::Version => {
            println!("wperf {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wperf: {e}");
            ExitCode::FAILURE
        }
    }
}
